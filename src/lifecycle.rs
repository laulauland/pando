use crate::{
    backend::CowBackend,
    home::{ensure_home, state_dir, PandoLock},
    metadata::{read_metadata, write_metadata, Metadata},
    naming::validate_name,
};
use anyhow::{bail, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

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

    let state_dir = state_dir(home, name);
    let workspace_path = backend.create(&state_dir, &source)?;
    write_metadata(
        &state_dir,
        &Metadata::new(name, source, workspace_path.clone()),
    )?;
    Ok(workspace_path)
}

pub fn destroy_workspace<B: CowBackend>(home: &Path, backend: &B, name: &str) -> Result<()> {
    validate_name(name)?;
    let _lock = PandoLock::acquire(home)?;
    ensure_home(home)?;
    backend.destroy(&state_dir(home, name))
}

pub fn list_workspaces(home: &Path) -> Result<Vec<Metadata>> {
    if !home.exists() {
        return Ok(Vec::new());
    }

    let mut metadata = Vec::new();
    for entry in fs::read_dir(home)? {
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
    use crate::{backend::FsCowBackend, home::state_dir, metadata::metadata_path};
    use std::fs;

    #[test]
    fn backend_agnostic_lifecycle_uses_home_name_state_dir() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("README.md"), "demo").unwrap();

        let home = tempfile::tempdir().unwrap();
        let backend = FsCowBackend;

        let workspace = create_workspace(home.path(), &backend, "demo", source.path()).unwrap();
        let state_dir = state_dir(home.path(), "demo");

        assert_eq!(workspace, state_dir.join("workspace"));
        assert!(workspace.join("README.md").exists());
        assert!(metadata_path(&state_dir).exists());
        assert_eq!(list_workspaces(home.path()).unwrap()[0].name, "demo");

        destroy_workspace(home.path(), &backend, "demo").unwrap();
        assert!(!state_dir.exists());
        assert!(list_workspaces(home.path()).unwrap().is_empty());
    }
}
