use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

const WORKSPACE_DIR: &str = "workspace";
#[cfg(any(test, target_os = "linux"))]
const OVERLAY_UPPER_DIR: &str = "upper";
#[cfg(any(test, target_os = "linux"))]
const OVERLAY_WORK_DIR: &str = "work";
#[cfg(any(test, target_os = "linux"))]
const OVERLAY_MERGED_DIR: &str = "merged";

pub trait CowBackend {
    fn create(&self, state_dir: &Path, source: &Path) -> Result<PathBuf>;
    fn destroy(&self, state_dir: &Path) -> Result<()>;
    fn workspace_path(&self, state_dir: &Path) -> PathBuf;
}

/// Portable, eager-copy backend used by tests and unsupported platforms.
#[derive(Debug, Clone, Default)]
pub struct SimpleCowBackend;

impl CowBackend for SimpleCowBackend {
    fn create(&self, state_dir: &Path, source: &Path) -> Result<PathBuf> {
        ensure_new_state_dir(state_dir)?;

        let workspace_path = self.workspace_path(state_dir);
        copy_recursively(source, &workspace_path)?;
        Ok(workspace_path)
    }

    fn destroy(&self, state_dir: &Path) -> Result<()> {
        remove_state_dir_if_exists(state_dir)
    }

    fn workspace_path(&self, state_dir: &Path) -> PathBuf {
        simple_workspace_path(state_dir)
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Default)]
pub struct OverlayFsBackend;

#[cfg(target_os = "linux")]
impl CowBackend for OverlayFsBackend {
    fn create(&self, state_dir: &Path, source: &Path) -> Result<PathBuf> {
        ensure_new_state_dir(state_dir)?;
        let source = source.canonicalize().with_context(|| {
            format!(
                "could not canonicalize overlay lowerdir: {}",
                source.display()
            )
        })?;

        let paths = OverlayPaths::new(state_dir);
        fs::create_dir_all(&paths.upper)
            .with_context(|| format!("could not create upperdir: {}", paths.upper.display()))?;
        fs::create_dir_all(&paths.work)
            .with_context(|| format!("could not create workdir: {}", paths.work.display()))?;
        fs::create_dir_all(&paths.merged).with_context(|| {
            format!(
                "could not create merged mountpoint: {}",
                paths.merged.display()
            )
        })?;

        let mount_result = if is_root() {
            mount_overlay(&source, &paths.upper, &paths.work, &paths.merged)
        } else {
            fuse_mount_overlay(&source, &paths.upper, &paths.work, &paths.merged)
        };
        if let Err(err) = mount_result {
            let _ = remove_state_dir_if_exists(state_dir);
            return Err(err);
        }
        Ok(paths.merged)
    }

    fn destroy(&self, state_dir: &Path) -> Result<()> {
        let merged = self.workspace_path(state_dir);
        if merged.exists() {
            match detect_mount_type(&merged) {
                Some(MountType::Kernel) => unmount_overlay(&merged)?,
                Some(MountType::Fuse) => fuse_unmount(&merged)?,
                None => {}
            }
        }
        remove_state_dir_if_exists(state_dir)
    }

    fn workspace_path(&self, state_dir: &Path) -> PathBuf {
        overlay_merged_path(state_dir)
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Default)]
pub struct ApfsCloneBackend;

#[cfg(target_os = "macos")]
impl CowBackend for ApfsCloneBackend {
    fn create(&self, state_dir: &Path, source: &Path) -> Result<PathBuf> {
        ensure_new_state_dir(state_dir)?;

        fs::create_dir_all(state_dir)
            .with_context(|| format!("could not create state dir: {}", state_dir.display()))?;
        let workspace_path = self.workspace_path(state_dir);
        clone_path(source, &workspace_path)?;
        Ok(workspace_path)
    }

    fn destroy(&self, state_dir: &Path) -> Result<()> {
        remove_state_dir_if_exists(state_dir)
    }

    fn workspace_path(&self, state_dir: &Path) -> PathBuf {
        simple_workspace_path(state_dir)
    }
}

#[cfg(target_os = "linux")]
pub type PlatformCowBackend = OverlayFsBackend;
#[cfg(target_os = "macos")]
pub type PlatformCowBackend = ApfsCloneBackend;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub type PlatformCowBackend = SimpleCowBackend;

fn simple_workspace_path(state_dir: &Path) -> PathBuf {
    state_dir.join(WORKSPACE_DIR)
}

#[cfg(any(test, target_os = "linux"))]
fn overlay_merged_path(state_dir: &Path) -> PathBuf {
    state_dir.join(OVERLAY_MERGED_DIR)
}

#[cfg(any(test, target_os = "linux"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct OverlayPaths {
    upper: PathBuf,
    work: PathBuf,
    merged: PathBuf,
}

#[cfg(any(test, target_os = "linux"))]
impl OverlayPaths {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            upper: state_dir.join(OVERLAY_UPPER_DIR),
            work: state_dir.join(OVERLAY_WORK_DIR),
            merged: overlay_merged_path(state_dir),
        }
    }
}

fn ensure_new_state_dir(state_dir: &Path) -> Result<()> {
    if state_dir.exists() {
        bail!("workspace already exists: {}", state_dir.display());
    }
    Ok(())
}

fn remove_state_dir_if_exists(state_dir: &Path) -> Result<()> {
    if state_dir.exists() {
        fs::remove_dir_all(state_dir)
            .with_context(|| format!("could not remove state dir: {}", state_dir.display()))?;
    }
    Ok(())
}

fn copy_recursively(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::metadata(source)
        .with_context(|| format!("could not read source metadata: {}", source.display()))?;
    if !metadata.is_dir() {
        bail!("source must be a directory: {}", source.display());
    }

    fs::create_dir_all(destination)
        .with_context(|| format!("could not create directory: {}", destination.display()))?;
    for entry in fs::read_dir(source)
        .with_context(|| format!("could not read directory: {}", source.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());

        if file_type.is_dir() {
            copy_recursively(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "could not copy {} to {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
        } else if file_type.is_symlink() {
            copy_symlink(&source_path, &destination_path)?;
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn clone_path(source: &Path, destination: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source_c = CString::new(source.as_os_str().as_bytes())
        .with_context(|| format!("path contains NUL byte: {}", source.display()))?;
    let destination_c = CString::new(destination.as_os_str().as_bytes())
        .with_context(|| format!("path contains NUL byte: {}", destination.display()))?;

    let rc = unsafe { libc::clonefile(source_c.as_ptr(), destination_c.as_ptr(), 0) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "clonefile failed from {} to {}",
                source.display(),
                destination.display()
            )
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn mount_overlay(lower: &Path, upper: &Path, work: &Path, merged: &Path) -> Result<()> {
    use std::ffi::CString;

    let source = CString::new("overlay")?;
    let target = path_to_cstring(merged)?;
    let fstype = CString::new("overlay")?;
    let options = CString::new(overlay_mount_options(lower, upper, work))?;

    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            0,
            options.as_ptr().cast(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "could not mount overlay lowerdir={} upperdir={} workdir={} at {}",
                lower.display(),
                upper.display(),
                work.display(),
                merged.display()
            )
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn unmount_overlay(merged: &Path) -> Result<()> {
    let target = path_to_cstring(merged)?;
    let rc = unsafe { libc::umount(target.as_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("could not unmount overlay: {}", merged.display()));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MountType {
    Kernel,
    Fuse,
}

/// Read `/proc/mounts` to determine whether `merged` is a kernel overlay or
/// fuse-overlayfs mount. Returns `None` when the path has no mount entry.
#[cfg(target_os = "linux")]
fn detect_mount_type(merged: &Path) -> Option<MountType> {
    let canonical = merged.canonicalize().ok()?;
    let canonical_str = canonical.to_str()?;
    let mounts = fs::read_to_string("/proc/mounts").ok()?;
    for line in mounts.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 3 && fields[1] == canonical_str {
            return match fields[2] {
                "overlay" => Some(MountType::Kernel),
                "fuse.fuse-overlayfs" => Some(MountType::Fuse),
                _ => None,
            };
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn fuse_mount_overlay(lower: &Path, upper: &Path, work: &Path, merged: &Path) -> Result<()> {
    let binary = resolve_fuse_overlayfs()?;
    let options = overlay_mount_options(lower, upper, work);
    let output = std::process::Command::new(&binary)
        .arg("-o")
        .arg(&options)
        .arg(merged)
        .output()
        .with_context(|| format!("could not run {}", binary.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "fuse-overlayfs mount failed at {}: {}",
            merged.display(),
            stderr.trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn fuse_unmount(merged: &Path) -> Result<()> {
    let binary = resolve_fusermount();
    let output = std::process::Command::new(&binary)
        .arg("-u")
        .arg(merged)
        .output()
        .with_context(|| format!("could not run {binary}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "could not unmount fuse overlay at {}: {}",
            merged.display(),
            stderr.trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn resolve_fusermount() -> &'static str {
    for candidate in ["fusermount3", "fusermount"] {
        if std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            // Leak a &'static str — called at most twice per process.
            return Box::leak(candidate.to_owned().into_boxed_str());
        }
    }
    "fusermount3"
}

/// Find fuse-overlayfs: check PATH first, then the pando cache dir, and
/// download a static binary as a last resort.
#[cfg(target_os = "linux")]
fn resolve_fuse_overlayfs() -> Result<PathBuf> {
    if let Ok(output) = std::process::Command::new("fuse-overlayfs")
        .arg("--version")
        .output()
    {
        if output.status.success() {
            return Ok(PathBuf::from("fuse-overlayfs"));
        }
    }

    let cached = fuse_overlayfs_cache_path()?;
    if cached.is_file() {
        return Ok(cached);
    }

    download_fuse_overlayfs(&cached)?;
    Ok(cached)
}

#[cfg(target_os = "linux")]
fn fuse_overlayfs_cache_path() -> Result<PathBuf> {
    let data_dir =
        dirs::data_dir().ok_or_else(|| anyhow::anyhow!("could not determine data directory"))?;
    Ok(data_dir.join("pando/bin/fuse-overlayfs"))
}

#[cfg(target_os = "linux")]
fn download_fuse_overlayfs(target: &Path) -> Result<()> {
    const VERSION: &str = "v1.16";
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => bail!("no prebuilt fuse-overlayfs binary for architecture: {other}"),
    };
    let url = format!(
        "https://github.com/containers/fuse-overlayfs/releases/download/{VERSION}/fuse-overlayfs-{arch}"
    );

    eprintln!(
        "fuse-overlayfs not found; downloading static binary to {}...",
        target.display()
    );

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    let output = std::process::Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(target)
        .arg(&url)
        .output()
        .context("could not run curl to download fuse-overlayfs")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to download fuse-overlayfs from {url}: {}",
            stderr.trim()
        );
    }

    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(target, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("could not make {} executable", target.display()))?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn overlay_mount_options(lower: &Path, upper: &Path, work: &Path) -> String {
    format!(
        "lowerdir={},upperdir={},workdir={}",
        lower.display(),
        upper.display(),
        work.display()
    )
}

#[cfg(target_os = "linux")]
fn path_to_cstring(path: &Path) -> Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("path contains NUL byte: {}", path.display()))
}

#[cfg(unix)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
    Ok(())
}

#[cfg(windows)]
fn copy_symlink(source: &Path, destination: &Path) -> Result<()> {
    let target = fs::read_link(source)?;
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(target, destination)?;
    } else {
        std::os::windows::fs::symlink_file(target, destination)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        overlay_merged_path, simple_workspace_path, CowBackend, OverlayPaths, SimpleCowBackend,
    };
    use std::{fs, path::PathBuf};

    #[test]
    fn simple_backend_copies_and_destroys_workspace() {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("nested")).unwrap();
        fs::write(source.path().join("nested/file.txt"), "hello").unwrap();

        let state_dir = tempfile::tempdir().unwrap().path().join("copy");
        let backend = SimpleCowBackend;

        let workspace = backend.create(&state_dir, source.path()).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("nested/file.txt")).unwrap(),
            "hello"
        );
        assert_eq!(workspace, backend.workspace_path(&state_dir));

        backend.destroy(&state_dir).unwrap();
        assert!(!state_dir.exists());
    }

    #[test]
    fn backend_workspace_paths_match_specs() {
        let state_dir = PathBuf::from("/tmp/pando/workspaces/demo");
        assert_eq!(
            simple_workspace_path(&state_dir),
            PathBuf::from("/tmp/pando/workspaces/demo/workspace")
        );
        assert_eq!(
            overlay_merged_path(&state_dir),
            PathBuf::from("/tmp/pando/workspaces/demo/merged")
        );
        assert_eq!(
            OverlayPaths::new(&state_dir),
            OverlayPaths {
                upper: PathBuf::from("/tmp/pando/workspaces/demo/upper"),
                work: PathBuf::from("/tmp/pando/workspaces/demo/work"),
                merged: PathBuf::from("/tmp/pando/workspaces/demo/merged"),
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn overlay_mount_options_are_readable() {
        assert_eq!(
            super::overlay_mount_options(
                &PathBuf::from("/src"),
                &PathBuf::from("/state/upper"),
                &PathBuf::from("/state/work")
            ),
            "lowerdir=/src,upperdir=/state/upper,workdir=/state/work"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires APFS clonefile support on the temp directory volume"]
    fn apfs_backend_clones_workspace() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("file.txt"), "hello").unwrap();
        let state_dir = tempfile::tempdir().unwrap().path().join("clone");
        let backend = super::ApfsCloneBackend;

        let workspace = backend.create(&state_dir, source.path()).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("file.txt")).unwrap(),
            "hello"
        );
    }
}
