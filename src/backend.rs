use anyhow::{bail, Context, Result};
use std::{fs, path::{Path, PathBuf}};

pub trait CowBackend {
    fn create(&self, source: &Path, destination: &Path) -> Result<()>;
    fn destroy(&self, workspace_path: &Path) -> Result<()>;
    fn workspace_path(&self, name: &str) -> PathBuf;
}

#[derive(Debug, Clone)]
pub struct FsCowBackend {
    workspaces_dir: PathBuf,
}

impl FsCowBackend {
    pub fn new(workspaces_dir: PathBuf) -> Self {
        Self { workspaces_dir }
    }
}

impl CowBackend for FsCowBackend {
    fn create(&self, source: &Path, destination: &Path) -> Result<()> {
        if destination.exists() {
            bail!("workspace already exists: {}", destination.display());
        }
        copy_recursively(source, destination)
    }

    fn destroy(&self, workspace_path: &Path) -> Result<()> {
        if workspace_path.exists() {
            fs::remove_dir_all(workspace_path)?;
        }
        Ok(())
    }

    fn workspace_path(&self, name: &str) -> PathBuf {
        self.workspaces_dir.join(name)
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

#[derive(Debug, Clone)]
pub struct MockBackend {
    workspaces_dir: PathBuf,
}

impl MockBackend {
    pub fn new(workspaces_dir: PathBuf) -> Self {
        Self { workspaces_dir }
    }
}

impl CowBackend for MockBackend {
    fn create(&self, source: &Path, destination: &Path) -> Result<()> {
        copy_recursively(source, destination)
    }

    fn destroy(&self, workspace_path: &Path) -> Result<()> {
        if workspace_path.exists() {
            fs::remove_dir_all(workspace_path)?;
        }
        Ok(())
    }

    fn workspace_path(&self, name: &str) -> PathBuf {
        self.workspaces_dir.join(name)
    }
}

#[cfg(test)]
mod tests {
    use super::{CowBackend, MockBackend};
    use std::fs;

    #[test]
    fn mock_backend_copies_and_destroys_workspace() {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("nested")).unwrap();
        fs::write(source.path().join("nested/file.txt"), "hello").unwrap();

        let home = tempfile::tempdir().unwrap();
        let backend = MockBackend::new(home.path().join("workspaces"));
        let workspace = backend.workspace_path("copy");

        backend.create(source.path(), &workspace).unwrap();
        assert_eq!(fs::read_to_string(workspace.join("nested/file.txt")).unwrap(), "hello");

        backend.destroy(&workspace).unwrap();
        assert!(!workspace.exists());
    }
}
