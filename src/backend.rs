use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

const WORKSPACE_DIR: &str = "workspace";

pub trait CowBackend {
    fn create(&self, state_dir: &Path, source: &Path) -> Result<PathBuf>;
    fn destroy(&self, state_dir: &Path) -> Result<()>;
    fn workspace_path(&self, state_dir: &Path) -> PathBuf;
}

#[derive(Debug, Clone, Default)]
pub struct FsCowBackend;

impl CowBackend for FsCowBackend {
    fn create(&self, state_dir: &Path, source: &Path) -> Result<PathBuf> {
        if state_dir.exists() {
            bail!("workspace already exists: {}", state_dir.display());
        }

        let workspace_path = self.workspace_path(state_dir);
        copy_recursively(source, &workspace_path)?;
        Ok(workspace_path)
    }

    fn destroy(&self, state_dir: &Path) -> Result<()> {
        if state_dir.exists() {
            fs::remove_dir_all(state_dir)?;
        }
        Ok(())
    }

    fn workspace_path(&self, state_dir: &Path) -> PathBuf {
        state_dir.join(WORKSPACE_DIR)
    }
}

pub fn copy_recursively(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::metadata(source)
        .with_context(|| format!("could not read source metadata: {}", source.display()))?;
    if !metadata.is_dir() {
        bail!("source must be a directory: {}", source.display());
    }

    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
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
    use super::{CowBackend, FsCowBackend};
    use std::fs;

    #[test]
    fn fs_backend_copies_and_destroys_workspace() {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("nested")).unwrap();
        fs::write(source.path().join("nested/file.txt"), "hello").unwrap();

        let state_dir = tempfile::tempdir().unwrap().path().join("copy");
        let backend = FsCowBackend;

        let workspace = backend.create(&state_dir, source.path()).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("nested/file.txt")).unwrap(),
            "hello"
        );
        assert_eq!(workspace, backend.workspace_path(&state_dir));

        backend.destroy(&state_dir).unwrap();
        assert!(!state_dir.exists());
    }
}
