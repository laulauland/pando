use anyhow::{bail, Context, Result};
use jj_lib::{
    backend::CommitId,
    config::{ConfigSource, StackedConfig},
    fileset::FilesetAliasesMap,
    id_prefix::IdPrefixContext,
    object_id::ObjectId as _,
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
    collections::BTreeSet,
    env, fs, io,
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
    pub base_revision: String,
}

pub fn pando_workspace_name(name: &str) -> String {
    format!("pando-{name}")
}

fn load_user_settings() -> Result<UserSettings> {
    let mut config = StackedConfig::with_defaults();
    // Workspace registration only needs identity for the pando-created working
    // copy commit. Repository config is intentionally left to Workspace::load().
    for path in jj_user_config_paths() {
        if path.is_file() {
            config.load_file(ConfigSource::User, path)?;
        }
    }
    Ok(UserSettings::from_config(config)?)
}

fn jj_user_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Load broad/default locations first and the explicit XDG_CONFIG_HOME path
    // last, so a test or caller-provided XDG config can override them.
    if let Some(home) = dirs::home_dir() {
        push_unique_path(&mut paths, home.join(".config/jj/config.toml"));
    }

    if let Some(config_dir) = dirs::config_dir() {
        push_unique_path(&mut paths, config_dir.join("jj/config.toml"));
    }

    if let Some(xdg_config_home) = env::var_os("XDG_CONFIG_HOME") {
        push_unique_path(
            &mut paths,
            PathBuf::from(xdg_config_home).join("jj/config.toml"),
        );
    }

    paths
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
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

    let settings = load_user_settings()?;
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
    let copied_source_base_commit = match from_revset {
        Some(_) => Some(default_base_commit(
            &canonical_repo,
            canonical_workspace.workspace_name(),
        )?),
        None => None,
    };

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

    if let Some(copied_source_base_commit) = copied_source_base_commit.as_ref() {
        // The COW backend cloned the source checkout before jj registration. jj's
        // fresh working-copy state starts from an empty tree, so checkout will
        // not overwrite files that already exist on disk. For an explicit
        // --from, clear copied tracked files first so checkout materializes the
        // requested tree while leaving ignored/untracked build state intact.
        remove_copied_tracked_files(&workspace_root, [&base_commit, copied_source_base_commit])?;
    }

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

    // Make the working-copy state agree with the selected commit. Without an
    // explicit --from, physical tracked files are already supplied by the COW
    // backend; with --from, copied tracked files were cleared above so checkout
    // writes the requested tree.
    pando_workspace
        .check_out(repo.op_id().clone(), None, &wc_commit)
        .block_on()?;

    Ok(JjRegistration {
        workspace_name: workspace_name.as_str().to_owned(),
        base_commit: base_commit.id().hex(),
        base_revision: format_change_revision(canonical_repo.as_ref(), base_commit.change_id())?,
    })
}

fn remove_copied_tracked_files<'a>(
    workspace_root: &Path,
    commits: impl IntoIterator<Item = &'a jj_lib::commit::Commit>,
) -> Result<()> {
    let mut paths = BTreeSet::new();
    for commit in commits {
        for (path, value) in commit.tree().entries() {
            value.with_context(|| {
                format!(
                    "could not read tracked path while preparing jj workspace {}",
                    commit.id().hex()
                )
            })?;
            paths.insert(path);
        }
    }

    for path in paths {
        let disk_path = path.to_fs_path(workspace_root)?;
        match fs::remove_file(&disk_path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_err)
                if disk_path
                    .symlink_metadata()
                    .is_ok_and(|metadata| metadata.is_dir()) => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "could not remove copied tracked file before jj checkout: {}",
                        disk_path.display()
                    )
                });
            }
        }
    }

    Ok(())
}

/// Resolve the jj change id for a stored base commit hash.
pub fn lookup_base_revision(
    canonical_root: &Path,
    base_commit_hex: &str,
) -> Result<Option<String>> {
    if !has_jj_repo(canonical_root) {
        return Ok(None);
    }

    let canonical_root = canonical_jj_root(canonical_root)?;
    let commit_id = CommitId::try_from_hex(base_commit_hex)
        .with_context(|| format!("invalid stored base commit id '{base_commit_hex}'"))?;
    let settings = load_user_settings()?;
    let store_factories = StoreFactories::default();
    let wc_factories = default_working_copy_factories();
    let workspace = Workspace::load(
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
    let repo = workspace
        .repo_loader()
        .load_at_head()
        .block_on()
        .with_context(|| {
            format!(
                "could not load jj repo at {}",
                canonical_root.path().display()
            )
        })?;

    if !repo.index().has_id(&commit_id)? {
        return Ok(None);
    }

    let commit = repo.store().get_commit(&commit_id)?;
    Ok(Some(format_change_revision(
        repo.as_ref(),
        commit.change_id(),
    )?))
}

fn format_change_revision(
    repo: &dyn jj_lib::repo::Repo,
    change_id: &jj_lib::backend::ChangeId,
) -> Result<String> {
    let prefix_context = IdPrefixContext::default();
    let prefix_index = prefix_context
        .populate(repo)
        .context("could not load jj id prefix index")?;
    let len = prefix_index
        .shortest_change_prefix_len(repo, change_id)
        .context("could not determine jj change id prefix length")?;
    let id_sym = change_id.reverse_hex();
    Ok(id_sym[..len].to_owned())
}

pub fn forget_pando_workspace(canonical_root: &Path, workspace_name: &str) -> Result<()> {
    let canonical_root = canonical_jj_root(canonical_root)?;
    let settings = load_user_settings()?;
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
    tx.commit(format!("pando: remove {}", workspace_name.as_symbol()))
        .block_on()?;

    // Known limitation: jj-lib keeps workspace names in SimpleWorkspaceStore,
    // which is not part of the repo transaction above. If this forget step
    // fails, callers keep Pando state so the remove can be retried.
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
    let ids: Vec<_> = evaluated
        .iter()
        .take(2)
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("could not evaluate jj revset '{revset}'"))?;

    match ids.as_slice() {
        [] => bail!("jj revset '{revset}' resolved to no commits"),
        [id] => Ok(repo.store().get_commit(id)?),
        [_, _] => bail!("jj revset '{revset}' resolved to more than one commit"),
        _ => unreachable!("only two revset ids were collected"),
    }
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
