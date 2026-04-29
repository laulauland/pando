use crate::{
    backend::CowBackend,
    home::{ensure_home, workspaces_dir, PandoLock},
    metadata::{read_metadata, write_metadata, Metadata},
    naming::validate_name,
};
use anyhow::{bail, Result};
use std::{fs, path::{Path, PathBuf}};

pub fn create_workspace<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    from: &Path,
) -> Result<PathBuf> {
    validate_name(name)?;
    let _lock = PandoLock::acquire(home)?;
    ensure_home(home)?;

    let source = from.canonicalize()?;
    if !source.is_dir() {
        bail!("source must be a directory: {}", source.display());
    }

    let workspace = backend.workspace_path(name);
    backend.create(&source, &workspace)?;
    write_metadata(&workspace, &Metadata::new(name, source))?;
    Ok(workspace)
}

pub fn destroy_workspace<B: CowBackend>(home: &Path, backend: &B, name: &str) -> Result<()> {
    validate_name(name)?;
    let _lock = PandoLock::acquire(home)?;
    ensure_home(home)?;
    backend.destroy(&backend.workspace_path(name))
}

pub fn list_workspaces(home: &Path) -> Result<Vec<Metadata>> {
    let dir = workspaces_dir(home);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut metadata = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Ok(item) = read_metadata(&entry.path()) {
                metadata.push(item);
            }
        }
    }
    metadata.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::{create_workspace, destroy_workspace, list_workspaces};
    use crate::{backend::MockBackend, home::workspaces_dir};
    use std::fs;

    #[test]
    fn backend_agnostic_lifecycle() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("README.md"), "demo").unwrap();

        let home = tempfile::tempdir().unwrap();
        let backend = MockBackend::new(workspaces_dir(home.path()));

        let workspace = create_workspace(home.path(), &backend, "demo", source.path()).unwrap();
        assert!(workspace.join("README.md").exists());
        assert_eq!(list_workspaces(home.path()).unwrap()[0].name, "demo");

        destroy_workspace(home.path(), &backend, "demo").unwrap();
        assert!(!workspace.exists());
        assert!(list_workspaces(home.path()).unwrap().is_empty());
    }
}
