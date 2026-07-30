use anyhow::{bail, Context, Result};
use jj_lib::{
    backend::CommitId,
    config::{ConfigSource, StackedConfig},
    fileset::FilesetAliasesMap,
    id_prefix::IdPrefixContext,
    object_id::ObjectId as _,
    ref_name::{WorkspaceName, WorkspaceNameBuf},
    repo::{Repo as _, RepoLoader, StoreFactories},
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

#[derive(Debug)]
pub struct JjRuntimeMount {
    volumes: Vec<JjRuntimeVolume>,
    identity: Vec<PathIdentity>,
}

#[derive(Debug)]
pub(crate) struct JjRuntimeVolume {
    host_path: PathBuf,
    guest_path: PathBuf,
}

struct ValidatedGitBackend {
    host_path: PathBuf,
    guest_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl JjRuntimeMount {
    pub fn host_repo_path(&self) -> &Path {
        &self.volumes[0].host_path
    }

    pub fn guest_repo_path(&self) -> &Path {
        &self.volumes[0].guest_path
    }

    #[cfg(any(test, feature = "microvm-boxlite"))]
    pub(crate) fn volumes(&self) -> impl Iterator<Item = (&Path, &Path)> {
        self.volumes
            .iter()
            .map(|volume| (volume.host_path.as_path(), volume.guest_path.as_path()))
    }

    pub fn revalidate(&self) -> Result<()> {
        for expected in &self.identity {
            let actual = path_identity(&expected.path)?;
            if &actual != expected {
                bail!(
                    "validated jj store path identity changed before provider handoff: {}",
                    expected.path.display()
                );
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn path_identity(path: &Path) -> Result<PathIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "jj store identity path is not a real directory: {}",
            path.display()
        );
    }
    Ok(PathIdentity {
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn path_identity(path: &Path) -> Result<PathIdentity> {
    bail!(
        "safe jj runtime store identity validation is unsupported on this platform: {}",
        path.display()
    )
}

fn capture_path_identity(path: &Path) -> Result<Vec<PathIdentity>> {
    let mut identities = vec![path_identity(Path::new("/"))?];
    let mut current = PathBuf::from("/");
    for component in path.components() {
        if let std::path::Component::Normal(component) = component {
            current.push(component);
            identities.push(path_identity(&current)?);
        }
    }
    Ok(identities)
}

/// Validate the native workspace pointer and reproduce its resolution in the guest.
pub fn jj_runtime_mount(
    workspace_root: &Path,
    canonical_root: &Path,
) -> Result<Option<JjRuntimeMount>> {
    let pointer_path = workspace_root.join(".jj/repo");
    if !workspace_root.join(".jj").exists() {
        return Ok(None);
    }
    let pointer_metadata = fs::symlink_metadata(&pointer_path).with_context(|| {
        format!(
            "jj workspace repo pointer is missing: {}",
            pointer_path.display()
        )
    })?;
    if !pointer_metadata.file_type().is_file() || pointer_metadata.file_type().is_symlink() {
        bail!("jj workspace repo pointer must be a regular file");
    }
    let pointer_bytes = fs::read(&pointer_path)?;
    let pointer_text = std::str::from_utf8(&pointer_bytes)
        .context("jj workspace repo pointer is not valid UTF-8")?;
    if pointer_text.is_empty()
        || pointer_text.trim() != pointer_text
        || pointer_text.bytes().any(|byte| byte == 0)
    {
        bail!("jj workspace repo pointer has unsafe or malformed contents");
    }
    let pointer = Path::new(pointer_text);
    if pointer.is_absolute() {
        bail!("jj workspace repo pointer must be relative");
    }
    if pointer.components().any(|component| {
        matches!(
            component,
            std::path::Component::RootDir | std::path::Component::Prefix(_)
        )
    }) {
        bail!("jj workspace repo pointer has an unsafe path form");
    }

    let actual_repo = pointer_path
        .parent()
        .context("jj workspace repo pointer has no parent")?
        .join(pointer)
        .canonicalize()
        .context("could not resolve jj workspace repo pointer")?;
    let expected_repo = canonical_root
        .join(".jj/repo")
        .canonicalize()
        .context("could not resolve canonical jj repository store")?;
    if actual_repo != expected_repo {
        bail!(
            "jj workspace repo pointer resolves to {}, expected {}",
            actual_repo.display(),
            expected_repo.display()
        );
    }
    let mut guest_repo = PathBuf::from("/workspace/.jj");
    resolve_guest_relative(&mut guest_repo, pointer)?;
    if guest_repo == Path::new("/") || guest_repo.starts_with("/workspace") {
        bail!("jj workspace repo pointer produces an unsafe guest mount path");
    }

    let mut volumes = vec![JjRuntimeVolume {
        host_path: expected_repo.clone(),
        guest_path: guest_repo.clone(),
    }];
    let git_backend = validate_runtime_store(&expected_repo, canonical_root, &guest_repo)?;
    if let Some(ValidatedGitBackend {
        host_path: host_git,
        guest_path: Some(guest_git),
    }) = git_backend.as_ref()
    {
        if guest_git == Path::new("/")
            || guest_git.starts_with("/workspace")
            || paths_overlap(&guest_repo, guest_git)
        {
            bail!("jj git backend produces an unsafe or overlapping guest mount path");
        }
        volumes.push(JjRuntimeVolume {
            host_path: host_git.clone(),
            guest_path: guest_git.clone(),
        });
    }
    let mut identity = Vec::new();
    for volume in &volumes {
        for item in capture_path_identity(&volume.host_path)? {
            if !identity.contains(&item) {
                identity.push(item);
            }
        }
    }
    if let Some(backend) = git_backend {
        for item in capture_path_identity(&backend.host_path)? {
            if !identity.contains(&item) {
                identity.push(item);
            }
        }
    }
    Ok(Some(JjRuntimeMount { volumes, identity }))
}

fn resolve_guest_relative(destination: &mut PathBuf, relative: &Path) -> Result<()> {
    for component in relative.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                destination.pop();
            }
            std::path::Component::Normal(component) => destination.push(component),
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                bail!("jj path must be relative")
            }
        }
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn validate_runtime_store(
    expected_repo: &Path,
    canonical_root: &Path,
    guest_repo: &Path,
) -> Result<Option<ValidatedGitBackend>> {
    let git_target = expected_repo.join("store/git_target");
    match fs::symlink_metadata(&git_target) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                bail!("jj git backend target must be a regular file");
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("could not inspect jj git backend target"),
    }
    let target = fs::read_to_string(&git_target).context("could not read jj git backend target")?;
    if target.is_empty()
        || target.trim() != target
        || target.bytes().any(|byte| byte == 0)
        || Path::new(&target).is_absolute()
    {
        bail!("jj git backend target has unsafe or malformed contents");
    }
    let target = Path::new(&target);
    let mut lexical_target = expected_repo.join("store");
    resolve_guest_relative(&mut lexical_target, target)?;
    if !lexical_target.starts_with(expected_repo) && lexical_target != canonical_root.join(".git") {
        bail!(
            "jj repository store depends on unsupported external path: {}",
            lexical_target.display()
        );
    }
    let resolved_target = git_target
        .parent()
        .context("jj git backend target has no parent")?
        .join(target)
        .canonicalize()
        .context("could not resolve jj git backend target")?;
    if resolved_target != lexical_target {
        bail!("jj git backend target traverses a symlink");
    }
    if resolved_target.starts_with(expected_repo) {
        path_identity(&resolved_target)
            .context("self-contained jj git backend is not a real directory")?;
        return Ok(Some(ValidatedGitBackend {
            host_path: resolved_target,
            guest_path: None,
        }));
    }

    let canonical_git = canonical_root.join(".git");
    path_identity(&canonical_git)?;
    let canonical_git = canonical_git
        .canonicalize()
        .context("could not resolve colocated jj git backend")?;
    if resolved_target != canonical_git {
        bail!(
            "jj repository store depends on unsupported external path: {}",
            resolved_target.display()
        );
    }
    let mut guest_target = guest_repo.join("store");
    resolve_guest_relative(&mut guest_target, target)?;
    Ok(Some(ValidatedGitBackend {
        host_path: canonical_git,
        guest_path: Some(guest_target),
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JjRegistration {
    pub workspace_name: String,
    pub base_commit: String,
    pub base_revision: String,
}

pub struct JjRegistrationPreflight {
    repo_loader: RepoLoader,
    repo_path: PathBuf,
    workspace_name: WorkspaceNameBuf,
    base_commit_id: CommitId,
    copied_source_base_commit_id: Option<CommitId>,
    base_revision: String,
}

impl JjRegistrationPreflight {
    /// Prove the canonical store is self-contained and has stable real-directory ancestors
    /// before a runtime-backed create mutates workspace storage.
    pub fn preflight_runtime_mount(&self) -> Result<()> {
        let canonical_root = self
            .repo_path
            .parent()
            .and_then(Path::parent)
            .context("canonical jj repository has no working-copy root")?;
        validate_runtime_store(
            &self.repo_path,
            canonical_root,
            Path::new("/preflight/.jj/repo"),
        )?;
        capture_path_identity(&self.repo_path)?;
        Ok(())
    }
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

/// Resolve and validate all jj inputs before workspace population begins.
pub fn preflight_jj_registration(
    canonical_root: &Path,
    name: &str,
    from_revset: Option<&str>,
) -> Result<Option<JjRegistrationPreflight>> {
    if !has_jj_repo(canonical_root) {
        return Ok(None);
    }

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
    let base_commit = match from_revset {
        Some(revset) => resolve_single_revset_commit(
            &repo,
            canonical_root.path(),
            canonical_workspace.workspace_name(),
            &settings,
            revset,
        )?,
        None => default_base_commit(&repo, canonical_workspace.workspace_name())?,
    };
    let copied_source_base_commit_id = match from_revset {
        Some(_) => Some(
            default_base_commit(&repo, canonical_workspace.workspace_name())?
                .id()
                .clone(),
        ),
        None => None,
    };
    let base_revision = format_change_revision(repo.as_ref(), base_commit.change_id())?;

    Ok(Some(JjRegistrationPreflight {
        repo_path: canonical_workspace.repo_path().to_path_buf(),
        repo_loader: canonical_workspace.repo_loader().clone(),
        workspace_name: WorkspaceNameBuf::from(pando_workspace_name(name)),
        base_commit_id: base_commit.id().clone(),
        copied_source_base_commit_id,
        base_revision,
    }))
}

/// Register `workspace_root` as a native jj workspace using preflighted inputs.
///
/// This uses jj-lib's public `Workspace::init_workspace_with_existing_repo()`
/// API, which creates an initial "add workspace" operation at the root commit.
/// We then create a second operation to move the pando workspace's `@` to a
/// fresh working-copy commit based on the commit selected during preflight.
/// This is intentionally two op-log entries for readability and API stability
/// in the first native registration implementation.
pub fn register_pando_workspace(
    workspace_root: &Path,
    preflight: JjRegistrationPreflight,
) -> Result<JjRegistration> {
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

    let JjRegistrationPreflight {
        repo_loader,
        repo_path,
        workspace_name,
        base_commit_id,
        copied_source_base_commit_id,
        base_revision,
    } = preflight;
    let canonical_repo = repo_loader.load_at_head().block_on()?;
    let base_commit = canonical_repo.store().get_commit(&base_commit_id)?;
    let copied_source_base_commit = copied_source_base_commit_id
        .as_ref()
        .map(|id| canonical_repo.store().get_commit(id))
        .transpose()?;

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
        base_revision,
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

    if repo.view().get_wc_commit_id(&workspace_name).is_some() {
        let mut tx = repo.start_transaction();
        tx.repo_mut().remove_wc_commit(&workspace_name).block_on()?;
        tx.repo_mut().rebase_descendants().block_on()?;
        tx.commit(format!("pando: remove {}", workspace_name.as_symbol()))
            .block_on()?;
    }

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
    use super::{canonical_jj_root, has_jj_repo, jj_runtime_mount, pando_workspace_name};
    use std::{fs, path::Path};

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

    fn pointer_fixture(
        pointer: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("nested level/deeper/work space");
        let canonical = root.path().join("canonical space");
        fs::create_dir_all(workspace.join(".jj")).unwrap();
        fs::create_dir_all(canonical.join(".jj/repo")).unwrap();
        fs::write(workspace.join(".jj/repo"), pointer).unwrap();
        (root, workspace, canonical)
    }

    #[cfg(unix)]
    #[test]
    fn runtime_mount_resolves_nested_relative_pointer_with_spaces() {
        let (_root, workspace, canonical) =
            pointer_fixture("../../../../canonical space/./.jj/repo");
        let mount = jj_runtime_mount(&workspace, &canonical).unwrap().unwrap();
        assert_eq!(mount.host_repo_path(), canonical.join(".jj/repo"));
        assert_eq!(
            mount.guest_repo_path(),
            Path::new("/canonical space/.jj/repo")
        );
        mount.revalidate().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_mount_adds_standard_colocated_git_backend() {
        let (_root, workspace, canonical) =
            pointer_fixture("../../../../canonical space/./.jj/repo");
        fs::create_dir_all(canonical.join(".jj/repo/store")).unwrap();
        fs::create_dir(canonical.join(".git")).unwrap();
        fs::write(canonical.join(".jj/repo/store/git_target"), "../../../.git").unwrap();

        let mount = jj_runtime_mount(&workspace, &canonical).unwrap().unwrap();
        let volumes = mount.volumes().collect::<Vec<_>>();
        assert_eq!(
            volumes,
            vec![
                (
                    canonical.join(".jj/repo").as_path(),
                    Path::new("/canonical space/.jj/repo")
                ),
                (
                    canonical.join(".git").as_path(),
                    Path::new("/canonical space/.git")
                ),
            ]
        );
        mount.revalidate().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_mount_keeps_self_contained_git_backend_inside_repo_mount() {
        let (_root, workspace, canonical) =
            pointer_fixture("../../../../canonical space/./.jj/repo");
        fs::create_dir_all(canonical.join(".jj/repo/store/git")).unwrap();
        fs::write(canonical.join(".jj/repo/store/git_target"), "git").unwrap();

        let mount = jj_runtime_mount(&workspace, &canonical).unwrap().unwrap();
        assert_eq!(mount.volumes().count(), 1);
        mount.revalidate().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_mount_rejects_non_directory_self_contained_git_backend() {
        let (_root, workspace, canonical) =
            pointer_fixture("../../../../canonical space/./.jj/repo");
        fs::create_dir_all(canonical.join(".jj/repo/store")).unwrap();
        fs::write(canonical.join(".jj/repo/store/git"), "not a git directory").unwrap();
        fs::write(canonical.join(".jj/repo/store/git_target"), "git").unwrap();

        assert!(jj_runtime_mount(&workspace, &canonical).is_err());
    }

    #[test]
    fn runtime_mount_accepts_non_jj_workspace_without_a_store_mount() {
        let root = tempfile::tempdir().unwrap();
        assert!(jj_runtime_mount(root.path(), root.path())
            .unwrap()
            .is_none());
    }

    #[test]
    fn runtime_mount_rejects_malformed_absolute_and_mismatched_pointers() {
        for pointer in ["/tmp/store", "../../../../canonical space/.jj/repo\n", ""] {
            let (_root, workspace, canonical) = pointer_fixture(pointer);
            assert!(jj_runtime_mount(&workspace, &canonical).is_err());
        }

        let (root, workspace, canonical) = pointer_fixture("../../../../other/.jj/repo");
        fs::create_dir_all(root.path().join("other/.jj/repo")).unwrap();
        assert!(jj_runtime_mount(&workspace, &canonical).is_err());
    }

    #[test]
    fn runtime_mount_rejects_store_destination_overlapping_workspace() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let canonical = workspace.join("canonical");
        fs::create_dir_all(workspace.join(".jj")).unwrap();
        fs::create_dir_all(canonical.join(".jj/repo")).unwrap();
        fs::write(workspace.join(".jj/repo"), "../canonical/.jj/repo").unwrap();
        assert!(jj_runtime_mount(&workspace, &canonical).is_err());
    }

    #[test]
    fn runtime_mount_rejects_repo_store_with_external_git_backend() {
        let (root, workspace, canonical) = pointer_fixture("../../../../canonical space/.jj/repo");
        fs::create_dir_all(canonical.join(".jj/repo/store")).unwrap();
        fs::create_dir_all(root.path().join("external.git")).unwrap();
        fs::write(
            canonical.join(".jj/repo/store/git_target"),
            "../../../../external.git",
        )
        .unwrap();
        assert!(jj_runtime_mount(&workspace, &canonical).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_mount_rejects_symlinked_colocated_git_inputs() {
        use std::os::unix::fs::symlink;

        let (root, workspace, canonical) = pointer_fixture("../../../../canonical space/.jj/repo");
        fs::create_dir_all(canonical.join(".jj/repo/store")).unwrap();
        fs::create_dir(root.path().join("git-target")).unwrap();
        symlink(root.path().join("git-target"), canonical.join(".git")).unwrap();
        fs::write(canonical.join(".jj/repo/store/git_target"), "../../../.git").unwrap();
        assert!(jj_runtime_mount(&workspace, &canonical).is_err());

        fs::remove_file(canonical.join(".git")).unwrap();
        fs::create_dir(canonical.join(".git")).unwrap();
        fs::remove_file(canonical.join(".jj/repo/store/git_target")).unwrap();
        symlink(
            canonical.join(".git"),
            canonical.join(".jj/repo/store/git-link"),
        )
        .unwrap();
        fs::write(canonical.join(".jj/repo/store/git_target"), "git-link").unwrap();
        assert!(jj_runtime_mount(&workspace, &canonical).is_err());

        fs::remove_file(canonical.join(".jj/repo/store/git-link")).unwrap();
        fs::remove_file(canonical.join(".jj/repo/store/git_target")).unwrap();
        fs::write(root.path().join("target-file"), "../../../.git").unwrap();
        symlink(
            root.path().join("target-file"),
            canonical.join(".jj/repo/store/git_target"),
        )
        .unwrap();
        assert!(jj_runtime_mount(&workspace, &canonical).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_mount_revalidation_detects_store_identity_replacement() {
        let (_root, workspace, canonical) = pointer_fixture("../../../../canonical space/.jj/repo");
        let mount = jj_runtime_mount(&workspace, &canonical).unwrap().unwrap();
        fs::rename(
            canonical.join(".jj/repo"),
            canonical.join(".jj/original-repo"),
        )
        .unwrap();
        fs::create_dir(canonical.join(".jj/repo")).unwrap();
        assert!(mount.revalidate().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_mount_revalidation_detects_colocated_git_replacement() {
        let (_root, workspace, canonical) = pointer_fixture("../../../../canonical space/.jj/repo");
        fs::create_dir_all(canonical.join(".jj/repo/store")).unwrap();
        fs::create_dir(canonical.join(".git")).unwrap();
        fs::write(canonical.join(".jj/repo/store/git_target"), "../../../.git").unwrap();
        let mount = jj_runtime_mount(&workspace, &canonical).unwrap().unwrap();
        fs::rename(canonical.join(".git"), canonical.join(".git-original")).unwrap();
        fs::create_dir(canonical.join(".git")).unwrap();
        assert!(mount.revalidate().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_mount_revalidation_detects_self_contained_git_replacement() {
        let (_root, workspace, canonical) =
            pointer_fixture("../../../../canonical space/./.jj/repo");
        let git_backend = canonical.join(".jj/repo/store/git");
        fs::create_dir_all(&git_backend).unwrap();
        fs::write(canonical.join(".jj/repo/store/git_target"), "git").unwrap();
        let mount = jj_runtime_mount(&workspace, &canonical).unwrap().unwrap();
        fs::rename(&git_backend, canonical.join(".jj/repo/store/original-git")).unwrap();
        fs::create_dir(&git_backend).unwrap();

        assert!(mount.revalidate().is_err());
    }

    #[cfg(not(unix))]
    #[test]
    fn runtime_mount_explicitly_rejects_unsupported_identity_platform() {
        let (_root, workspace, canonical) = pointer_fixture("../../../../canonical space/.jj/repo");
        let error = jj_runtime_mount(&workspace, &canonical).unwrap_err();
        assert!(error.to_string().contains("unsupported on this platform"));
    }
}
