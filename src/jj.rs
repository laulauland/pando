use anyhow::{bail, Context, Result};
use jj_lib::{
    config::StackedConfig,
    fileset::FilesetAliasesMap,
    object_id::ObjectId,
    ref_name::{WorkspaceName, WorkspaceNameBuf},
    repo::{Repo as _, StoreFactories},
    repo_path::RepoPathUiConverter,
    revset::{
        RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetParseContext,
        RevsetWorkspaceContext, SymbolResolver,
    },
    settings::UserSettings,
    workspace::{default_working_copy_factories, default_working_copy_factory, Workspace},
    workspace_store::{SimpleWorkspaceStore, WorkspaceStore as _},
};
use pollster::FutureExt as _;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Minimal jj filesystem checks for workspace registration.
///
/// Pando treats the canonical root as jj-backed only when the root itself
/// contains `.jj/`. We intentionally do not walk parents: workspaces are
/// created for an explicit canonical directory, and registration needs that
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JjRegistration {
    pub workspace_name: String,
    pub base_commit: String,
}

pub fn pando_workspace_name(name: &str) -> String {
    format!("pando-{name}")
}

/// Register `workspace_root` as a native jj workspace in `canonical_root`'s repo.
///
/// This uses jj-lib's public `Workspace::init_workspace_with_existing_repo()`
/// API, which creates an initial "add workspace" operation at the root commit.
/// We then create a second operation to move the pando workspace's `@` to a
/// fresh working-copy commit based on the canonical workspace's current `@`
/// parent. This is intentionally two op-log entries for readability and API
/// stability in the first native registration implementation.
pub fn register_pando_workspace(
    canonical_root: &Path,
    workspace_root: &Path,
    name: &str,
    from_revset: Option<&str>,
) -> Result<JjRegistration> {
    let canonical_root = canonical_jj_root(canonical_root)?;
    let workspace_root = workspace_root.canonicalize().with_context(|| {
        format!(
            "could not canonicalize pando workspace root: {}",
            workspace_root.display()
        )
    })?;

    // Cow backends copy or expose the canonical `.jj`. Replace that with a
    // pando-local `.jj` directory pointing at the existing canonical repo.
    let copied_jj_dir = workspace_root.join(".jj");
    if copied_jj_dir.exists() {
        fs::remove_dir_all(&copied_jj_dir).with_context(|| {
            format!(
                "could not remove copied jj directory before registration: {}",
                copied_jj_dir.display()
            )
        })?;
    }

    let settings = UserSettings::from_config(StackedConfig::with_defaults())?;
    let store_factories = StoreFactories::default();
    let wc_factories = default_working_copy_factories();
    let canonical_workspace = Workspace::load(
        &settings,
        canonical_root.path(),
        &store_factories,
        &wc_factories,
    )
    .with_context(|| {
        format!(
            "could not load canonical jj workspace: {}",
            canonical_root.path().display()
        )
    })?;
    let canonical_repo = canonical_workspace
        .repo_loader()
        .load_at_head()
        .block_on()?;
    let repo_path = canonical_workspace.repo_path().to_path_buf();
    let workspace_name = WorkspaceNameBuf::from(pando_workspace_name(name));

    let (mut pando_workspace, repo_after_init) = Workspace::init_workspace_with_existing_repo(
        &workspace_root,
        &repo_path,
        &canonical_repo,
        &*default_working_copy_factory(),
        workspace_name.clone(),
    )
    .block_on()
    .with_context(|| {
        format!(
            "could not initialize jj workspace {} at {}",
            workspace_name.as_symbol(),
            workspace_root.display()
        )
    })?;

    let base_commit = match from_revset {
        Some(revset) => resolve_single_revset_commit(
            &canonical_repo,
            canonical_root.path(),
            canonical_workspace.workspace_name(),
            &settings,
            revset,
        )?,
        None => default_base_commit(&canonical_repo, canonical_workspace.workspace_name())?,
    };
    let mut tx = repo_after_init.start_transaction();
    let repo_mut = tx.repo_mut();
    let wc_commit = repo_mut
        .check_out(workspace_name.clone(), &base_commit)
        .block_on()?;
    repo_mut.rebase_descendants().block_on()?;
    let repo = tx
        .commit(format!(
            "set pando workspace '{}' base",
            workspace_name.as_symbol()
        ))
        .block_on()?;

    // Make the working-copy state agree with the selected commit. The physical
    // files are already supplied by the COW backend; checkout reconciles jj's
    // recorded tree state with the working-copy commit.
    pando_workspace
        .check_out(repo.op_id().clone(), None, &wc_commit)
        .block_on()?;

    Ok(JjRegistration {
        workspace_name: workspace_name.as_str().to_owned(),
        base_commit: base_commit.id().hex(),
    })
}

pub fn forget_pando_workspace(canonical_root: &Path, workspace_name: &str) -> Result<()> {
    let canonical_root = canonical_jj_root(canonical_root)?;
    let settings = UserSettings::from_config(StackedConfig::with_defaults())?;
    let store_factories = StoreFactories::default();
    let wc_factories = default_working_copy_factories();
    let canonical_workspace = Workspace::load(
        &settings,
        canonical_root.path(),
        &store_factories,
        &wc_factories,
    )
    .with_context(|| {
        format!(
            "could not load canonical jj workspace: {}",
            canonical_root.path().display()
        )
    })?;
    let repo = canonical_workspace
        .repo_loader()
        .load_at_head()
        .block_on()?;
    let workspace_name = WorkspaceNameBuf::from(workspace_name.to_owned());

    let mut tx = repo.start_transaction();
    tx.repo_mut().remove_wc_commit(&workspace_name).block_on()?;
    tx.repo_mut().rebase_descendants().block_on()?;
    tx.commit(format!("pando: destroy {}", workspace_name.as_symbol()))
        .block_on()?;

    // Known limitation: jj-lib keeps workspace names in SimpleWorkspaceStore,
    // which is not part of the repo transaction above. If this forget step
    // fails, callers keep Pando state so the destroy can be retried.
    let workspace_store = SimpleWorkspaceStore::load(canonical_workspace.repo_path())?;
    workspace_store.forget(&[&workspace_name])?;
    Ok(())
}

fn resolve_single_revset_commit(
    repo: &std::sync::Arc<jj_lib::repo::ReadonlyRepo>,
    canonical_root: &Path,
    canonical_workspace_name: &WorkspaceName,
    settings: &UserSettings,
    revset: &str,
) -> Result<jj_lib::commit::Commit> {
    let path_converter = RepoPathUiConverter::Fs {
        cwd: canonical_root.to_path_buf(),
        base: canonical_root.to_path_buf(),
    };
    let workspace_context = RevsetWorkspaceContext {
        path_converter: &path_converter,
        workspace_name: canonical_workspace_name,
    };
    let extensions = RevsetExtensions::default();
    let aliases_map = RevsetAliasesMap::new();
    let fileset_aliases_map = FilesetAliasesMap::new();
    let context = RevsetParseContext {
        aliases_map: &aliases_map,
        local_variables: Default::default(),
        user_email: settings.user_email(),
        date_pattern_context: chrono::Utc::now().fixed_offset().into(),
        default_ignored_remote: Some("git".as_ref()),
        fileset_aliases_map: &fileset_aliases_map,
        use_glob_by_default: true,
        extensions: &extensions,
        workspace: Some(workspace_context),
    };
    let expression = jj_lib::revset::parse(&mut RevsetDiagnostics::new(), revset, &context)
        .with_context(|| format!("could not parse jj revset '{revset}'"))?;
    let symbol_resolver = SymbolResolver::new(repo.as_ref(), extensions.symbol_resolvers());
    let resolved = expression
        .resolve_user_expression(repo.as_ref(), &symbol_resolver)
        .with_context(|| format!("could not resolve jj revset '{revset}'"))?;
    let evaluated = resolved
        .evaluate(repo.as_ref())
        .with_context(|| format!("could not evaluate jj revset '{revset}'"))?;
    let mut ids = evaluated.iter();
    let Some(id) = ids
        .next()
        .transpose()
        .with_context(|| format!("could not evaluate jj revset '{revset}'"))?
    else {
        bail!("jj revset '{revset}' resolved to no commits");
    };
    if ids
        .next()
        .transpose()
        .with_context(|| format!("could not evaluate jj revset '{revset}'"))?
        .is_some()
    {
        bail!("jj revset '{revset}' resolved to more than one commit");
    }

    Ok(repo.store().get_commit(&id)?)
}

fn default_base_commit(
    repo: &std::sync::Arc<jj_lib::repo::ReadonlyRepo>,
    canonical_workspace_name: &WorkspaceName,
) -> Result<jj_lib::commit::Commit> {
    let wc_commit_id = repo
        .view()
        .get_wc_commit_id(canonical_workspace_name)
        .with_context(|| {
            format!(
                "canonical workspace '{}' has no working-copy commit",
                canonical_workspace_name.as_symbol()
            )
        })?;
    let wc_commit = repo.store().get_commit(wc_commit_id)?;
    let parent_ids = wc_commit.parent_ids();
    let base_id = parent_ids
        .first()
        .unwrap_or_else(|| repo.store().root_commit_id());
    Ok(repo.store().get_commit(base_id)?)
}

#[cfg(test)]
mod tests {
    use super::{canonical_jj_root, has_jj_repo, pando_workspace_name};
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

    #[test]
    fn formats_pando_workspace_name() {
        assert_eq!(pando_workspace_name("foo"), "pando-foo");
    }
}
