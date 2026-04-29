use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// Minimal jj filesystem checks for future workspace registration.
///
/// These helpers do not load or modify a jj repository. Pando treats the
/// canonical root as jj-backed only when the root itself contains `.jj/`. We
/// intentionally do not walk parents: workspaces are created for an explicit
/// canonical directory, and later registration work needs that exact root.
pub fn has_jj_repo(canonical_root: &Path) -> bool {
    canonical_root.join(".jj").is_dir()
}

/// Validated canonical root for a jj-backed Pando workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JjCanonicalRoot {
    path: PathBuf,
}

impl JjCanonicalRoot {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn canonical_jj_root(canonical_root: &Path) -> Result<JjCanonicalRoot> {
    let path = canonical_root.canonicalize()?;
    if !has_jj_repo(&path) {
        bail!("not a jj repository root: {}", path.display());
    }
    Ok(JjCanonicalRoot { path })
}

/// Compile-time guard for the jj-lib 0.40 APIs expected by later registration
/// work.
///
/// The actual register/forget flow is intentionally not implemented here. This
/// keeps the dependency pinned to the workspace-loading and workspace-store API
/// shape without constructing a repository or touching user data.
#[allow(dead_code)]
fn _jj_lib_040_api_guard() {
    let _: fn(
        &jj_lib::settings::UserSettings,
        &Path,
        &jj_lib::repo::StoreFactories,
        &jj_lib::workspace::WorkingCopyFactories,
    ) -> Result<jj_lib::workspace::Workspace, jj_lib::workspace::WorkspaceLoadError> =
        jj_lib::workspace::Workspace::load;

    let _: fn(
        &Path,
    ) -> Result<
        jj_lib::workspace_store::SimpleWorkspaceStore,
        jj_lib::workspace_store::WorkspaceStoreError,
    > = jj_lib::workspace_store::SimpleWorkspaceStore::load;
    let _: fn(
        &jj_lib::workspace_store::SimpleWorkspaceStore,
        &jj_lib::ref_name::WorkspaceName,
        &Path,
    ) -> Result<(), jj_lib::workspace_store::WorkspaceStoreError> =
        <jj_lib::workspace_store::SimpleWorkspaceStore as jj_lib::workspace_store::WorkspaceStore>::add;
    let _: fn(
        &jj_lib::workspace_store::SimpleWorkspaceStore,
        &[&jj_lib::ref_name::WorkspaceName],
    ) -> Result<(), jj_lib::workspace_store::WorkspaceStoreError> =
        <jj_lib::workspace_store::SimpleWorkspaceStore as jj_lib::workspace_store::WorkspaceStore>::forget;
    let _: fn(&str) -> &jj_lib::ref_name::WorkspaceName = jj_lib::ref_name::WorkspaceName::new;
    let _: fn(&jj_lib::ref_name::WorkspaceName) -> &str = jj_lib::ref_name::WorkspaceName::as_str;
}

#[cfg(test)]
mod tests {
    use super::{canonical_jj_root, has_jj_repo};
    use std::fs;

    #[test]
    fn detects_jj_only_at_canonical_root() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_jj_repo(dir.path()));

        fs::create_dir(dir.path().join(".jj")).unwrap();
        assert!(has_jj_repo(dir.path()));
    }

    #[test]
    fn validates_canonical_jj_root() {
        let dir = tempfile::tempdir().unwrap();
        assert!(canonical_jj_root(dir.path()).is_err());

        fs::create_dir(dir.path().join(".jj")).unwrap();
        let root = canonical_jj_root(dir.path()).unwrap();
        assert_eq!(root.path(), dir.path().canonicalize().unwrap());
    }
}
