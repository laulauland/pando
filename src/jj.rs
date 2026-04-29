use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// Minimal jj repository discovery for Pando's V2 integration.
///
/// Pando treats the canonical root as a jj repository only when the root itself
/// contains `.jj/`. We intentionally do not walk parents here: workspaces are
/// created for an explicit canonical directory, and V2 registration needs that
/// exact root.
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

/// Compile-time guard for the jj-lib APIs the V2 implementation will use.
///
/// The full workspace registration flow is implemented in later tickets. This
/// helper exists so this crate is pinned to jj-lib 0.40 and the expected core
/// types stay visible to the compiler while V1 behavior remains unchanged.
#[allow(dead_code)]
fn _jj_lib_040_api_guard(
    settings: &jj_lib::settings::UserSettings,
    store: &jj_lib::repo_path::RepoPath,
) {
    let _ = settings;
    let _ = store;
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
