use crate::{
    backend::CowBackend,
    home::{ensure_home, state_dir, PandoLock},
    jj::{forget_pando_workspace, has_jj_repo, pando_workspace_name, register_pando_workspace},
    metadata::{read_metadata, write_metadata, JjMetadata, Metadata},
    naming::validate_name,
};
use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn create_workspace<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    from: &Path,
    from_revset: Option<&str>,
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

    let jj = match register_jj_if_needed(&source, &workspace_path, name, from_revset) {
        Ok(jj) => jj,
        Err(err) => {
            backend.destroy(&state_dir).with_context(|| {
                format!(
                    "jj registration failed ({err:#}); additionally failed to clean up state dir {}",
                    state_dir.display()
                )
            })?;
            return Err(err);
        }
    };

    let mut metadata = Metadata::new(name, source, workspace_path.clone());
    metadata.jj = jj;
    write_metadata(&state_dir, &metadata)?;
    Ok(workspace_path)
}

fn register_jj_if_needed(
    source: &Path,
    workspace_path: &Path,
    name: &str,
    from_revset: Option<&str>,
) -> Result<Option<JjMetadata>> {
    if !has_jj_repo(source) {
        return Ok(None);
    }

    let registration = register_pando_workspace(source, workspace_path, name, from_revset)?;
    Ok(Some(JjMetadata {
        workspace_name: Some(registration.workspace_name),
        base_commit: Some(registration.base_commit),
    }))
}

pub fn destroy_workspace<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    keep_jj_workspace: bool,
) -> Result<()> {
    validate_name(name)?;
    let _lock = PandoLock::acquire(home)?;
    ensure_home(home)?;

    let state_dir = state_dir(home, name);
    if !keep_jj_workspace {
        forget_registered_jj_workspace(&state_dir)?;
    }

    backend.destroy(&state_dir)
}

fn forget_registered_jj_workspace(state_dir: &Path) -> Result<()> {
    let Ok(metadata) = read_metadata(state_dir) else {
        return Ok(());
    };
    let Some(jj) = metadata.jj else {
        return Ok(());
    };

    let workspace_name = jj
        .workspace_name
        .unwrap_or_else(|| pando_workspace_name(&metadata.name));
    forget_pando_workspace(&metadata.canonical_root, &workspace_name)
        .with_context(|| format!("could not forget jj workspace '{workspace_name}'"))
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
    use crate::{
        backend::SimpleCowBackend,
        home::state_dir,
        metadata::{metadata_path, write_metadata, JjMetadata, Metadata},
    };
    use proptest::prelude::*;
    use std::{collections::BTreeSet, fs};

    #[test]
    fn backend_agnostic_lifecycle_uses_home_name_state_dir() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("README.md"), "demo").unwrap();

        let home = tempfile::tempdir().unwrap();
        let backend = SimpleCowBackend;

        let workspace =
            create_workspace(home.path(), &backend, "demo", source.path(), None).unwrap();
        let state_dir = state_dir(home.path(), "demo");

        assert_eq!(workspace, state_dir.join("workspace"));
        assert!(workspace.join("README.md").exists());
        assert!(metadata_path(&state_dir).exists());
        assert_eq!(list_workspaces(home.path()).unwrap()[0].name, "demo");

        destroy_workspace(home.path(), &backend, "demo", false).unwrap();
        assert!(!state_dir.exists());
        assert!(list_workspaces(home.path()).unwrap().is_empty());
    }

    #[test]
    fn create_cleans_up_state_dir_when_jj_registration_fails() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("README.md"), "demo").unwrap();
        fs::create_dir(source.path().join(".jj")).unwrap();

        let home = tempfile::tempdir().unwrap();
        let backend = SimpleCowBackend;
        let state_dir = state_dir(home.path(), "demo");

        let result = create_workspace(home.path(), &backend, "demo", source.path(), None);

        assert!(result.is_err());
        assert!(!state_dir.exists());
    }

    #[test]
    fn destroy_keeps_state_dir_when_jj_forget_fails() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = state_dir(home.path(), "demo");
        let workspace_path = state_dir.join("workspace");
        let mut metadata = Metadata::new("demo", home.path().join("missing-root"), workspace_path);
        metadata.jj = Some(JjMetadata {
            workspace_name: Some("pando-demo".to_owned()),
            base_commit: None,
        });
        write_metadata(&state_dir, &metadata).unwrap();

        let result = destroy_workspace(home.path(), &SimpleCowBackend, "demo", false);

        assert!(result.is_err());
        assert!(
            state_dir.exists(),
            "failed jj forget must not remove pando state"
        );
    }

    #[derive(Debug, Clone, Copy)]
    enum Operation {
        Create(usize),
        Destroy(usize),
        List,
    }

    fn operation() -> impl Strategy<Value = Operation> {
        prop_oneof![
            (0usize..3).prop_map(Operation::Create),
            (0usize..3).prop_map(Operation::Destroy),
            Just(Operation::List),
        ]
    }

    fn assert_list_matches(home: &std::path::Path, existing: &BTreeSet<&'static str>) {
        let listed: BTreeSet<String> = list_workspaces(home)
            .unwrap()
            .into_iter()
            .map(|metadata| metadata.name)
            .collect();
        let expected: BTreeSet<String> = existing.iter().map(|name| (*name).to_owned()).collect();
        assert_eq!(listed, expected);
    }

    proptest! {
        #[test]
        fn simple_backend_lifecycle_operation_sequences_preserve_list_invariant(
            operations in prop::collection::vec(operation(), 0..32)
        ) {
            const NAMES: [&str; 3] = ["alpha", "beta", "gamma"];

            let source = tempfile::tempdir().unwrap();
            fs::create_dir(source.path().join("nested")).unwrap();
            fs::write(source.path().join("README.md"), "demo").unwrap();
            fs::write(source.path().join("nested/file.txt"), "nested").unwrap();

            let home = tempfile::tempdir().unwrap();
            let backend = SimpleCowBackend;
            let mut existing = BTreeSet::new();

            for operation in operations {
                match operation {
                    Operation::Create(index) => {
                        let name = NAMES[index];
                        let result = create_workspace(home.path(), &backend, name, source.path(), None);
                        if existing.contains(name) {
                            prop_assert!(result.is_err(), "duplicate create for {name:?} should fail");
                        } else {
                            let workspace = result.unwrap();
                            prop_assert_eq!(&workspace, &state_dir(home.path(), name).join("workspace"));
                            prop_assert!(workspace.join("README.md").exists());
                            prop_assert!(workspace.join("nested/file.txt").exists());
                            existing.insert(name);
                        }
                    }
                    Operation::Destroy(index) => {
                        let name = NAMES[index];
                        let result = destroy_workspace(home.path(), &backend, name, false);
                        prop_assert!(result.is_ok(), "destroy is idempotent for missing non-jj workspaces");
                        existing.remove(name);
                    }
                    Operation::List => {}
                }

                assert_list_matches(home.path(), &existing);
            }
        }
    }
}
