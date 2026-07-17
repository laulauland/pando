use crate::{
    backend::CowBackend,
    home::{ensure_home, state_dir, state_root, workspace_dir, PandoLock},
    metadata::{metadata_path, read_metadata, write_metadata, Metadata},
    naming::validate_name,
};
use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn migrate_legacy_home_if_needed<B: CowBackend>(
    legacy_home: &Path,
    home: &Path,
    backend: &B,
) -> Result<()> {
    if legacy_home != home && !legacy_home.exists() {
        let _home_lock = PandoLock::acquire(home)?;
        ensure_home(home)?;
        if !legacy_home.exists() {
            return repair_migrated_workspaces(home, backend);
        }
    }

    {
        let _legacy_lock = PandoLock::acquire_legacy(legacy_home)?;
        let _home_lock = PandoLock::acquire(home)?;
        ensure_home(home)?;
        repair_migrated_workspaces(home, backend)?;

        let legacy_workspaces = legacy_workspaces(legacy_home)?;
        preflight_destinations(home, &legacy_workspaces)?;

        for (legacy_state_dir, mut metadata) in legacy_workspaces {
            let state_dir = state_dir(home, &metadata.name);
            let workspace_path = workspace_dir(home, &metadata.name);
            eprintln!("Migrating Pando workspace '{}'...", metadata.name);
            backend.migrate_legacy(
                &legacy_state_dir,
                &state_dir,
                &workspace_path,
                &metadata.canonical_root,
            )?;
            metadata.workspace_path = workspace_path;
            repair_jj_repo_pointer(&metadata)?;
            write_metadata(&state_dir, &metadata)?;
        }
        repair_migrated_workspaces(home, backend)?;
    }

    cleanup_legacy_home(legacy_home, home);
    Ok(())
}

fn cleanup_legacy_home(legacy_home: &Path, home: &Path) {
    let _ = fs::remove_file(legacy_home.join(".lock"));
    if legacy_home != home {
        let _ = fs::remove_dir(legacy_home);
    }
}

fn legacy_workspaces(legacy_home: &Path) -> Result<Vec<(PathBuf, Metadata)>> {
    if !legacy_home.exists() {
        return Ok(Vec::new());
    }

    let mut workspaces = Vec::new();
    for entry in fs::read_dir(legacy_home)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || !metadata_path(&entry.path()).is_file() {
            continue;
        }

        let metadata = read_metadata(&entry.path()).with_context(|| {
            format!(
                "could not read legacy workspace metadata: {}",
                entry.path().display()
            )
        })?;
        validate_name(&metadata.name)?;
        if entry.file_name() != metadata.name.as_str() {
            bail!(
                "legacy workspace directory '{}' does not match metadata name '{}'",
                entry.file_name().to_string_lossy(),
                metadata.name
            );
        }
        workspaces.push((entry.path(), metadata));
    }
    workspaces.sort_by(|left, right| left.1.name.cmp(&right.1.name));
    Ok(workspaces)
}

fn preflight_destinations(home: &Path, workspaces: &[(PathBuf, Metadata)]) -> Result<()> {
    for (_, metadata) in workspaces {
        let state_dir = state_dir(home, &metadata.name);
        let workspace_path = workspace_dir(home, &metadata.name);
        if state_dir.exists() || workspace_path.exists() {
            bail!(
                "cannot migrate workspace '{}': destination already exists",
                metadata.name
            );
        }
    }
    Ok(())
}

fn repair_migrated_workspaces<B: CowBackend>(home: &Path, backend: &B) -> Result<()> {
    let state_root = state_root(home);
    if !state_root.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(state_root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || !metadata_path(&entry.path()).is_file() {
            continue;
        }
        let mut metadata = read_metadata(&entry.path())?;
        validate_name(&metadata.name)?;
        if entry.file_name() != metadata.name.as_str() {
            bail!(
                "workspace state directory '{}' does not match metadata name '{}'",
                entry.file_name().to_string_lossy(),
                metadata.name
            );
        }
        let expected = workspace_dir(home, &metadata.name);
        let metadata_changed = metadata.workspace_path != expected;
        if metadata_changed || !expected.exists() {
            backend.resume_migration(&entry.path(), &expected, &metadata.canonical_root)?;
            metadata.workspace_path = expected;
        }
        if metadata_changed {
            repair_jj_repo_pointer(&metadata)?;
            write_metadata(&entry.path(), &metadata)?;
        }
    }
    Ok(())
}

fn repair_jj_repo_pointer(metadata: &Metadata) -> Result<()> {
    if metadata.jj.is_none() || !metadata.workspace_path.exists() {
        return Ok(());
    }

    let pointer = metadata.workspace_path.join(".jj/repo");
    if !pointer.is_file() {
        return Ok(());
    }
    let repo_path = metadata.canonical_root.join(".jj/repo");
    let desired = repo_path.as_os_str().as_encoded_bytes();
    if fs::read(&pointer).is_ok_and(|current| current == desired) {
        return Ok(());
    }
    fs::write(&pointer, desired).with_context(|| {
        format!(
            "could not update jj repository pointer: {}",
            pointer.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::migrate_legacy_home_if_needed;
    use crate::{
        backend::{CowBackend, SimpleCowBackend},
        home::{state_dir, workspace_dir},
        metadata::{read_metadata, write_metadata, JjMetadata, Metadata},
    };
    use std::{
        cell::Cell,
        fs,
        path::{Path, PathBuf},
    };

    struct FailSecondMigration {
        migrations: Cell<usize>,
    }

    impl CowBackend for FailSecondMigration {
        fn create(
            &self,
            state_dir: &Path,
            workspace_path: &Path,
            source: &Path,
        ) -> anyhow::Result<PathBuf> {
            SimpleCowBackend.create(state_dir, workspace_path, source)
        }

        fn destroy(&self, state_dir: &Path, workspace_path: &Path) -> anyhow::Result<()> {
            SimpleCowBackend.destroy(state_dir, workspace_path)
        }

        fn migrate_legacy(
            &self,
            legacy_state_dir: &Path,
            state_dir: &Path,
            workspace_path: &Path,
            source: &Path,
        ) -> anyhow::Result<PathBuf> {
            let migration = self.migrations.get() + 1;
            self.migrations.set(migration);
            if migration == 2 {
                anyhow::bail!("injected second-workspace migration failure");
            }
            SimpleCowBackend.migrate_legacy(legacy_state_dir, state_dir, workspace_path, source)
        }

        fn resume_migration(
            &self,
            state_dir: &Path,
            workspace_path: &Path,
            source: &Path,
        ) -> anyhow::Result<PathBuf> {
            SimpleCowBackend.resume_migration(state_dir, workspace_path, source)
        }
    }

    #[test]
    fn migrates_legacy_simple_workspace_to_split_layout() {
        let root = tempfile::tempdir().unwrap();
        let legacy_home = root.path().join("legacy");
        let home = root.path().join(".pando");
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("README.md"), "canonical\n").unwrap();

        let legacy_state = legacy_home.join("demo");
        let legacy_workspace = legacy_state.join("workspace");
        fs::create_dir_all(&legacy_workspace).unwrap();
        fs::write(legacy_workspace.join("README.md"), "workspace edit\n").unwrap();
        let mut metadata = Metadata::new("demo", source.clone(), legacy_workspace);
        metadata.jj = Some(JjMetadata {
            workspace_name: Some("pando-demo".to_owned()),
            base_commit: None,
            base_revision: None,
        });
        fs::create_dir_all(legacy_state.join("workspace/.jj")).unwrap();
        fs::write(legacy_state.join("workspace/.jj/repo"), "../../old/repo").unwrap();
        fs::create_dir_all(source.join(".jj/repo")).unwrap();
        write_metadata(&legacy_state, &metadata).unwrap();

        migrate_legacy_home_if_needed(&legacy_home, &home, &SimpleCowBackend).unwrap();

        let migrated_state = state_dir(&home, "demo");
        let migrated_workspace = workspace_dir(&home, "demo");
        assert!(!legacy_home.exists());
        assert_eq!(
            fs::read_to_string(migrated_workspace.join("README.md")).unwrap(),
            "workspace edit\n"
        );
        assert_eq!(
            read_metadata(&migrated_state).unwrap().workspace_path,
            migrated_workspace
        );
        assert_eq!(
            fs::read_to_string(migrated_workspace.join(".jj/repo")).unwrap(),
            source.join(".jj/repo").to_string_lossy()
        );
    }

    #[test]
    fn migration_refuses_destination_collisions_without_moving_legacy_state() {
        let root = tempfile::tempdir().unwrap();
        let legacy_home = root.path().join("legacy");
        let home = root.path().join(".pando");
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();

        let legacy_state = legacy_home.join("demo");
        let legacy_workspace = legacy_state.join("workspace");
        fs::create_dir_all(&legacy_workspace).unwrap();
        write_metadata(
            &legacy_state,
            &Metadata::new("demo", source, legacy_workspace),
        )
        .unwrap();
        fs::create_dir_all(workspace_dir(&home, "demo")).unwrap();

        let result = migrate_legacy_home_if_needed(&legacy_home, &home, &SimpleCowBackend);

        assert!(result.is_err());
        assert!(legacy_state.exists());
        assert!(!state_dir(&home, "demo").exists());
    }

    #[test]
    fn migration_is_a_noop_after_success() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join(".pando");

        migrate_legacy_home_if_needed(root.path(), &home, &SimpleCowBackend).unwrap();
        migrate_legacy_home_if_needed(root.path(), &home, &SimpleCowBackend).unwrap();

        assert!(home.join("state").is_dir());
        assert!(home.join("workspaces").is_dir());
    }

    #[test]
    fn does_not_create_a_missing_legacy_home() {
        let root = tempfile::tempdir().unwrap();
        let legacy_home = root.path().join("missing/legacy");
        let home = root.path().join(".pando");

        migrate_legacy_home_if_needed(&legacy_home, &home, &SimpleCowBackend).unwrap();

        assert!(!legacy_home.exists());
        assert!(home.join("state").is_dir());
        assert!(home.join("workspaces").is_dir());
    }

    #[test]
    fn retries_after_a_later_workspace_fails() {
        let root = tempfile::tempdir().unwrap();
        let legacy_home = root.path().join("legacy");
        let home = root.path().join(".pando");
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        for name in ["alpha", "beta"] {
            let legacy_state = legacy_home.join(name);
            let legacy_workspace = legacy_state.join("workspace");
            fs::create_dir_all(&legacy_workspace).unwrap();
            fs::write(legacy_workspace.join("file.txt"), name).unwrap();
            write_metadata(
                &legacy_state,
                &Metadata::new(name, source.clone(), legacy_workspace),
            )
            .unwrap();
        }

        let result = migrate_legacy_home_if_needed(
            &legacy_home,
            &home,
            &FailSecondMigration {
                migrations: Cell::new(0),
            },
        );
        assert!(result.is_err());
        assert!(workspace_dir(&home, "alpha").exists());
        assert!(legacy_home.join("beta").exists());

        migrate_legacy_home_if_needed(&legacy_home, &home, &SimpleCowBackend).unwrap();

        assert!(!legacy_home.exists());
        for name in ["alpha", "beta"] {
            assert_eq!(
                fs::read_to_string(workspace_dir(&home, name).join("file.txt")).unwrap(),
                name
            );
        }
    }

    #[test]
    fn resumes_after_state_moved_before_workspace() {
        let root = tempfile::tempdir().unwrap();
        let legacy_home = root.path().join("legacy");
        let home = root.path().join(".pando");
        let source = root.path().join("source");
        let partial_state = state_dir(&home, "demo");
        let partial_workspace = partial_state.join("workspace");
        fs::create_dir(&source).unwrap();
        fs::create_dir_all(&partial_workspace).unwrap();
        fs::write(partial_workspace.join("file.txt"), "preserved").unwrap();
        write_metadata(
            &partial_state,
            &Metadata::new("demo", source, legacy_home.join("demo/workspace")),
        )
        .unwrap();

        migrate_legacy_home_if_needed(&legacy_home, &home, &SimpleCowBackend).unwrap();

        assert_eq!(
            fs::read_to_string(workspace_dir(&home, "demo").join("file.txt")).unwrap(),
            "preserved"
        );
        assert_eq!(
            read_metadata(&partial_state).unwrap().workspace_path,
            workspace_dir(&home, "demo")
        );
    }

    #[test]
    fn migrates_custom_home_in_place() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("custom-home");
        let source = root.path().join("source");
        let legacy_state = home.join("demo");
        let legacy_workspace = legacy_state.join("workspace");
        fs::create_dir(&source).unwrap();
        fs::create_dir_all(&legacy_workspace).unwrap();
        fs::write(legacy_workspace.join("file.txt"), "preserved").unwrap();
        write_metadata(
            &legacy_state,
            &Metadata::new("demo", source, legacy_workspace),
        )
        .unwrap();

        migrate_legacy_home_if_needed(&home, &home, &SimpleCowBackend).unwrap();

        assert!(!legacy_state.exists());
        assert_eq!(
            fs::read_to_string(workspace_dir(&home, "demo").join("file.txt")).unwrap(),
            "preserved"
        );
        assert!(state_dir(&home, "demo").join("meta.toml").is_file());
        assert!(!home.join(".lock").exists());
    }
}
