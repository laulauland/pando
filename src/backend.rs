use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

const WORKSPACE_DIR: &str = "workspace";
#[cfg(any(test, target_os = "linux"))]
const OVERLAY_DIR: &str = "overlay";
#[cfg(any(test, target_os = "linux"))]
const OVERLAY_UPPER_DIR: &str = "upper";
#[cfg(any(test, target_os = "linux"))]
const OVERLAY_WORK_DIR: &str = "work";

pub trait CowBackend {
    fn create(&self, state_dir: &Path, workspace_path: &Path, source: &Path) -> Result<PathBuf>;
    fn destroy(&self, state_dir: &Path, workspace_path: &Path) -> Result<()>;
    fn migrate_legacy(
        &self,
        legacy_state_dir: &Path,
        state_dir: &Path,
        workspace_path: &Path,
        source: &Path,
    ) -> Result<PathBuf>;
    fn resume_migration(
        &self,
        state_dir: &Path,
        workspace_path: &Path,
        source: &Path,
    ) -> Result<PathBuf>;
}

/// Portable, eager-copy backend used by tests and unsupported platforms.
#[derive(Debug, Clone, Default)]
pub struct SimpleCowBackend;

impl CowBackend for SimpleCowBackend {
    fn create(&self, state_dir: &Path, workspace_path: &Path, source: &Path) -> Result<PathBuf> {
        ensure_new_workspace_paths(state_dir, workspace_path)?;
        fs::create_dir_all(state_dir)?;
        copy_recursively(source, workspace_path)?;
        Ok(workspace_path.to_path_buf())
    }

    fn destroy(&self, state_dir: &Path, workspace_path: &Path) -> Result<()> {
        remove_workspace_paths(state_dir, workspace_path)
    }

    fn migrate_legacy(
        &self,
        legacy_state_dir: &Path,
        state_dir: &Path,
        workspace_path: &Path,
        _source: &Path,
    ) -> Result<PathBuf> {
        migrate_legacy_simple(legacy_state_dir, state_dir, workspace_path)
    }

    fn resume_migration(
        &self,
        state_dir: &Path,
        workspace_path: &Path,
        _source: &Path,
    ) -> Result<PathBuf> {
        resume_legacy_simple(state_dir, workspace_path)
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Default)]
pub struct OverlayFsBackend;

#[cfg(target_os = "linux")]
impl CowBackend for OverlayFsBackend {
    fn create(&self, state_dir: &Path, workspace_path: &Path, source: &Path) -> Result<PathBuf> {
        ensure_new_workspace_paths(state_dir, workspace_path)?;
        let source = source.canonicalize().with_context(|| {
            format!(
                "could not canonicalize overlay lowerdir: {}",
                source.display()
            )
        })?;

        let paths = OverlayPaths::new(state_dir, workspace_path);
        fs::create_dir_all(&paths.upper)
            .with_context(|| format!("could not create upperdir: {}", paths.upper.display()))?;
        fs::create_dir_all(&paths.work)
            .with_context(|| format!("could not create workdir: {}", paths.work.display()))?;
        fs::create_dir_all(&paths.workspace).with_context(|| {
            format!(
                "could not create workspace mountpoint: {}",
                paths.workspace.display()
            )
        })?;

        let mount_result = mount_platform_overlay(&source, &paths);
        if let Err(err) = mount_result {
            let _ = remove_workspace_paths(state_dir, workspace_path);
            return Err(err);
        }
        Ok(paths.workspace)
    }

    fn destroy(&self, state_dir: &Path, workspace_path: &Path) -> Result<()> {
        unmount_if_mounted(workspace_path)?;
        remove_workspace_paths(state_dir, workspace_path)
    }

    fn migrate_legacy(
        &self,
        legacy_state_dir: &Path,
        state_dir: &Path,
        workspace_path: &Path,
        source: &Path,
    ) -> Result<PathBuf> {
        migrate_legacy_overlay(legacy_state_dir, state_dir, workspace_path, source)
    }

    fn resume_migration(
        &self,
        state_dir: &Path,
        workspace_path: &Path,
        source: &Path,
    ) -> Result<PathBuf> {
        resume_legacy_overlay(state_dir, workspace_path, source)
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Default)]
pub struct ApfsCloneBackend;

#[cfg(target_os = "macos")]
impl CowBackend for ApfsCloneBackend {
    fn create(&self, state_dir: &Path, workspace_path: &Path, source: &Path) -> Result<PathBuf> {
        ensure_new_workspace_paths(state_dir, workspace_path)?;

        fs::create_dir_all(state_dir)
            .with_context(|| format!("could not create state dir: {}", state_dir.display()))?;
        clone_path(source, &workspace_path)?;
        Ok(workspace_path.to_path_buf())
    }

    fn destroy(&self, state_dir: &Path, workspace_path: &Path) -> Result<()> {
        remove_workspace_paths(state_dir, workspace_path)
    }

    fn migrate_legacy(
        &self,
        legacy_state_dir: &Path,
        state_dir: &Path,
        workspace_path: &Path,
        _source: &Path,
    ) -> Result<PathBuf> {
        migrate_legacy_simple(legacy_state_dir, state_dir, workspace_path)
    }

    fn resume_migration(
        &self,
        state_dir: &Path,
        workspace_path: &Path,
        _source: &Path,
    ) -> Result<PathBuf> {
        resume_legacy_simple(state_dir, workspace_path)
    }
}

#[cfg(target_os = "linux")]
pub type PlatformCowBackend = OverlayFsBackend;
#[cfg(target_os = "macos")]
pub type PlatformCowBackend = ApfsCloneBackend;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub type PlatformCowBackend = SimpleCowBackend;

fn legacy_simple_workspace_path(state_dir: &Path) -> PathBuf {
    state_dir.join(WORKSPACE_DIR)
}

#[cfg(any(test, target_os = "linux"))]
fn legacy_overlay_workspace_path(state_dir: &Path) -> PathBuf {
    state_dir.join("merged")
}

#[cfg(any(test, target_os = "linux"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct OverlayPaths {
    upper: PathBuf,
    work: PathBuf,
    workspace: PathBuf,
}

#[cfg(any(test, target_os = "linux"))]
impl OverlayPaths {
    pub fn new(state_dir: &Path, workspace_path: &Path) -> Self {
        let overlay = state_dir.join(OVERLAY_DIR);
        Self {
            upper: overlay.join(OVERLAY_UPPER_DIR),
            work: overlay.join(OVERLAY_WORK_DIR),
            workspace: workspace_path.to_path_buf(),
        }
    }
}

fn ensure_new_workspace_paths(state_dir: &Path, workspace_path: &Path) -> Result<()> {
    if state_dir.exists() {
        bail!("workspace already exists: {}", state_dir.display());
    }
    if workspace_path.exists() {
        bail!("workspace already exists: {}", workspace_path.display());
    }
    Ok(())
}

fn remove_workspace_paths(state_dir: &Path, workspace_path: &Path) -> Result<()> {
    if workspace_path.exists() {
        fs::remove_dir_all(workspace_path)
            .with_context(|| format!("could not remove workspace: {}", workspace_path.display()))?;
    }
    if state_dir.exists() {
        fs::remove_dir_all(state_dir)
            .with_context(|| format!("could not remove state dir: {}", state_dir.display()))?;
    }
    Ok(())
}

fn migrate_legacy_simple(
    legacy_state_dir: &Path,
    state_dir: &Path,
    workspace_path: &Path,
) -> Result<PathBuf> {
    ensure_new_workspace_paths(state_dir, workspace_path)?;
    fs::create_dir_all(
        state_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("state directory has no parent"))?,
    )?;
    fs::create_dir_all(
        workspace_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("workspace path has no parent"))?,
    )?;

    fs::rename(legacy_state_dir, state_dir).with_context(|| {
        format!(
            "could not move legacy state {} to {}",
            legacy_state_dir.display(),
            state_dir.display()
        )
    })?;

    let legacy_workspace = legacy_simple_workspace_path(state_dir);
    if let Err(err) = fs::rename(&legacy_workspace, workspace_path) {
        let migration_err = anyhow::Error::from(err).context(format!(
            "could not move legacy workspace {} to {}",
            legacy_workspace.display(),
            workspace_path.display()
        ));
        return match fs::rename(state_dir, legacy_state_dir) {
            Ok(()) => Err(migration_err),
            Err(rollback_err) => Err(migration_err)
                .context(format!("migration rollback also failed: {rollback_err}")),
        };
    }

    Ok(workspace_path.to_path_buf())
}

fn resume_legacy_simple(state_dir: &Path, workspace_path: &Path) -> Result<PathBuf> {
    if workspace_path.exists() {
        return Ok(workspace_path.to_path_buf());
    }
    let legacy_workspace = legacy_simple_workspace_path(state_dir);
    if !legacy_workspace.exists() {
        bail!(
            "workspace data missing from both {} and {}",
            legacy_workspace.display(),
            workspace_path.display()
        );
    }
    fs::create_dir_all(
        workspace_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("workspace path has no parent"))?,
    )?;
    fs::rename(&legacy_workspace, workspace_path).with_context(|| {
        format!(
            "could not move legacy workspace {} to {}",
            legacy_workspace.display(),
            workspace_path.display()
        )
    })?;
    Ok(workspace_path.to_path_buf())
}

#[cfg(target_os = "linux")]
fn migrate_legacy_overlay(
    legacy_state_dir: &Path,
    state_dir: &Path,
    workspace_path: &Path,
    source: &Path,
) -> Result<PathBuf> {
    ensure_new_workspace_paths(state_dir, workspace_path)?;
    let legacy_workspace = legacy_overlay_workspace_path(legacy_state_dir);
    unmount_if_mounted(&legacy_workspace)?;

    let result =
        migrate_unmounted_legacy_overlay(legacy_state_dir, state_dir, workspace_path, source);
    if let Err(err) = result {
        let rollback =
            rollback_legacy_overlay_migration(legacy_state_dir, state_dir, workspace_path, source);
        return match rollback {
            Ok(()) => Err(err),
            Err(rollback_err) => {
                Err(err).context(format!("migration rollback also failed: {rollback_err:#}"))
            }
        };
    }

    Ok(workspace_path.to_path_buf())
}

#[cfg(target_os = "linux")]
fn migrate_unmounted_legacy_overlay(
    legacy_state_dir: &Path,
    state_dir: &Path,
    workspace_path: &Path,
    source: &Path,
) -> Result<()> {
    fs::create_dir_all(
        state_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("state directory has no parent"))?,
    )?;
    fs::create_dir_all(
        workspace_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("workspace path has no parent"))?,
    )?;
    fs::rename(legacy_state_dir, state_dir)?;

    resume_legacy_overlay(state_dir, workspace_path, source)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn resume_legacy_overlay(
    state_dir: &Path,
    workspace_path: &Path,
    source: &Path,
) -> Result<PathBuf> {
    if detect_mount_type(workspace_path).is_some() {
        return Ok(workspace_path.to_path_buf());
    }

    let paths = OverlayPaths::new(state_dir, workspace_path);
    let overlay_dir = state_dir.join(OVERLAY_DIR);
    fs::create_dir_all(&overlay_dir)?;
    let legacy_upper = state_dir.join(OVERLAY_UPPER_DIR);
    if !paths.upper.exists() {
        if !legacy_upper.exists() {
            bail!("overlay upperdir is missing: {}", paths.upper.display());
        }
        fs::rename(&legacy_upper, &paths.upper)?;
    } else if legacy_upper.exists() {
        bail!(
            "overlay upperdir exists in both legacy and migrated locations for {}",
            state_dir.display()
        );
    }

    let legacy_work = state_dir.join(OVERLAY_WORK_DIR);
    if legacy_work.exists() {
        fs::remove_dir_all(legacy_work)?;
    }
    if paths.work.exists() {
        fs::remove_dir_all(&paths.work)?;
    }
    fs::create_dir(&paths.work)?;

    let legacy_workspace = legacy_overlay_workspace_path(state_dir);
    if !workspace_path.exists() && legacy_workspace.exists() {
        fs::rename(&legacy_workspace, workspace_path)?;
    } else if workspace_path.exists() && legacy_workspace.exists() {
        bail!(
            "workspace mountpoint exists in both legacy and migrated locations for {}",
            state_dir.display()
        );
    } else if !workspace_path.exists() {
        fs::create_dir_all(workspace_path)?;
    }

    mount_platform_overlay(source, &paths)?;
    Ok(workspace_path.to_path_buf())
}

#[cfg(target_os = "linux")]
fn rollback_legacy_overlay_migration(
    legacy_state_dir: &Path,
    state_dir: &Path,
    workspace_path: &Path,
    source: &Path,
) -> Result<()> {
    if !state_dir.exists() {
        let paths = OverlayPaths {
            upper: legacy_state_dir.join(OVERLAY_UPPER_DIR),
            work: legacy_state_dir.join(OVERLAY_WORK_DIR),
            workspace: legacy_overlay_workspace_path(legacy_state_dir),
        };
        return mount_platform_overlay(source, &paths);
    }

    unmount_if_mounted(workspace_path)?;
    let paths = OverlayPaths::new(state_dir, workspace_path);
    let legacy_workspace = legacy_overlay_workspace_path(state_dir);
    if workspace_path.exists() && legacy_workspace.exists() {
        bail!(
            "cannot roll back: workspace mountpoint exists in both legacy and migrated locations"
        );
    }
    if paths.upper.exists() && state_dir.join(OVERLAY_UPPER_DIR).exists() {
        bail!("cannot roll back: upperdir exists in both legacy and migrated locations");
    }
    if workspace_path.exists() && !legacy_workspace.exists() {
        fs::rename(workspace_path, &legacy_workspace)?;
    }
    if paths.work.exists() {
        fs::remove_dir_all(&paths.work)?;
    }
    if paths.upper.exists() && !state_dir.join(OVERLAY_UPPER_DIR).exists() {
        fs::rename(&paths.upper, state_dir.join(OVERLAY_UPPER_DIR))?;
    }
    let overlay_dir = state_dir.join(OVERLAY_DIR);
    if overlay_dir.exists() {
        fs::remove_dir_all(overlay_dir)?;
    }
    let legacy_work = state_dir.join(OVERLAY_WORK_DIR);
    if legacy_work.exists() {
        fs::remove_dir_all(&legacy_work)?;
    }
    fs::create_dir(&legacy_work)?;
    fs::rename(state_dir, legacy_state_dir)?;

    let legacy_paths = OverlayPaths {
        upper: legacy_state_dir.join(OVERLAY_UPPER_DIR),
        work: legacy_state_dir.join(OVERLAY_WORK_DIR),
        workspace: legacy_overlay_workspace_path(legacy_state_dir),
    };
    mount_platform_overlay(source, &legacy_paths)
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
fn mount_platform_overlay(source: &Path, paths: &OverlayPaths) -> Result<()> {
    if is_root() {
        mount_overlay(source, &paths.upper, &paths.work, &paths.workspace)
    } else {
        fuse_mount_overlay(source, &paths.upper, &paths.work, &paths.workspace)
    }
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
fn unmount_if_mounted(workspace_path: &Path) -> Result<()> {
    if !workspace_path.exists() {
        return Ok(());
    }
    match detect_mount_type(workspace_path) {
        Some(MountType::Kernel) => unmount_overlay(workspace_path),
        Some(MountType::Fuse) => fuse_unmount(workspace_path),
        None => Ok(()),
    }
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
    let output = std::process::Command::new(binary)
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
    use super::{CowBackend, OverlayPaths, SimpleCowBackend};
    use std::{fs, path::PathBuf};

    #[test]
    fn simple_backend_copies_and_destroys_workspace() {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("nested")).unwrap();
        fs::write(source.path().join("nested/file.txt"), "hello").unwrap();

        let root = tempfile::tempdir().unwrap();
        let state_dir = root.path().join("state/copy");
        let workspace_path = root.path().join("workspaces/copy");
        let backend = SimpleCowBackend;

        let workspace = backend
            .create(&state_dir, &workspace_path, source.path())
            .unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("nested/file.txt")).unwrap(),
            "hello"
        );
        assert_eq!(workspace, workspace_path);

        backend.destroy(&state_dir, &workspace).unwrap();
        assert!(!state_dir.exists());
        assert!(!workspace.exists());
    }

    #[test]
    fn backend_workspace_paths_match_specs() {
        let state_dir = PathBuf::from("/tmp/pando/state/demo");
        let workspace_path = PathBuf::from("/tmp/pando/workspaces/demo");
        assert_eq!(
            OverlayPaths::new(&state_dir, &workspace_path),
            OverlayPaths {
                upper: PathBuf::from("/tmp/pando/state/demo/overlay/upper"),
                work: PathBuf::from("/tmp/pando/state/demo/overlay/work"),
                workspace: workspace_path,
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
        let workspace_path = tempfile::tempdir().unwrap().path().join("workspace");
        let backend = super::ApfsCloneBackend;

        let workspace = backend
            .create(&state_dir, &workspace_path, source.path())
            .unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("file.txt")).unwrap(),
            "hello"
        );
    }
}
