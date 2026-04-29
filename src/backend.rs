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

        mount_overlay(&source, &paths.upper, &paths.work, &paths.merged)?;
        Ok(paths.merged)
    }

    fn destroy(&self, state_dir: &Path) -> Result<()> {
        let merged = self.workspace_path(state_dir);
        if merged.exists() {
            unmount_overlay(&merged)?;
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

        let workspace_path = self.workspace_path(state_dir);
        clone_recursively(source, &workspace_path)?;
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
fn clone_recursively(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("could not read source metadata: {}", source.display()))?;

    if metadata.is_dir() {
        fs::create_dir_all(destination)
            .with_context(|| format!("could not create directory: {}", destination.display()))?;
        for entry in fs::read_dir(source)
            .with_context(|| format!("could not read directory: {}", source.display()))?
        {
            let entry = entry?;
            clone_recursively(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }

    if metadata.file_type().is_symlink() {
        copy_symlink(source, destination)?;
        return Ok(());
    }

    if metadata.is_file() {
        clone_file(source, destination)?;
        return Ok(());
    }

    bail!("unsupported file type in source tree: {}", source.display());
}

#[cfg(target_os = "macos")]
fn clone_file(source: &Path, destination: &Path) -> Result<()> {
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
