use crate::{
    backend::CowBackend,
    home::{ensure_home, state_dir, state_root, workspace_dir, PandoLock},
    jj::{
        forget_pando_workspace, pando_workspace_name, preflight_jj_registration,
        register_pando_workspace, JjRegistrationPreflight,
    },
    metadata::{
        has_any_runtime_transactions, has_runtime_transaction, read_metadata, write_metadata,
        JjMetadata, Metadata,
    },
    naming::validate_name,
};

#[cfg(feature = "microvm-boxlite")]
use crate::jj::jj_runtime_mount;
#[cfg(feature = "microvm-boxlite")]
use crate::metadata::{
    clear_runtime_transaction, runtime_transaction_directories, runtime_transaction_path,
    write_runtime_transaction, RuntimeCreateTransaction, RuntimeMetadata,
};
use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(feature = "microvm-boxlite")]
async fn acquire_runtime_lock(home: &Path) -> Result<PandoLock> {
    let home = home.to_owned();
    tokio::task::spawn_blocking(move || PandoLock::acquire(&home))
        .await
        .context("runtime lock task failed")?
}

#[cfg(all(feature = "microvm-boxlite", unix, debug_assertions))]
fn injected_crash(point: &str) {
    if std::env::var("PANDO_TEST_CRASH_POINT").as_deref() == Ok(point) {
        // SAFETY: getpid returns this process and SIGKILL is enabled only by explicit debug tests.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    }
}

#[cfg(not(all(feature = "microvm-boxlite", unix, debug_assertions)))]
fn injected_crash(_point: &str) {}

#[cfg(feature = "microvm-boxlite")]
async fn recover_runtime_transactions<B, R>(home: &Path, backend: &B, runtime: &R) -> Result<()>
where
    B: CowBackend,
    R: crate::runtime::RuntimeBackend,
{
    for directory in runtime_transaction_directories(home)? {
        let Some(transaction) = directory.read()? else {
            if !directory.contains_only_optional_temp()? {
                bail!(
                    "runtime transaction directory without a journal contains unknown files: {}",
                    directory.path().display()
                );
            }
            directory.clear()?;
            continue;
        };
        validate_name(&transaction.name)?;
        if directory.name() != transaction.name {
            bail!("runtime recovery record name does not match its directory");
        }
        let state_dir = state_dir(home, &transaction.name);
        if read_metadata(&state_dir).is_ok_and(|metadata| metadata.runtime.is_some()) {
            clear_runtime_transaction(home, &transaction.name)?;
            continue;
        }
        let identity = match transaction.identity {
            Some(identity) => runtime.contains(&identity).await?.then_some(identity),
            None => runtime
                .find(&transaction.provider_name)
                .await
                .context("could not reconcile provisional runtime by exact create token")?,
        };
        if let Some(identity) = identity {
            runtime.stop(&identity).await?;
            runtime.remove(&identity).await?;
        }
        if state_dir.exists() || workspace_dir(home, &transaction.name).exists() {
            if crate::jj::has_jj_repo(&transaction.canonical_root) {
                forget_pando_workspace(
                    &transaction.canonical_root,
                    &pando_workspace_name(&transaction.name),
                )?;
            }
            backend.destroy(&state_dir, &workspace_dir(home, &transaction.name))?;
        }
        clear_runtime_transaction(home, &transaction.name)?;
    }
    Ok(())
}

#[cfg(feature = "microvm-boxlite")]
async fn recover_platform_runtime_transactions<R>(home: &Path, runtime: &R) -> Result<()>
where
    R: crate::runtime::RuntimeBackend,
{
    recover_runtime_transactions(
        home,
        &crate::backend::PlatformCowBackend::default(),
        runtime,
    )
    .await
}

#[cfg(feature = "microvm-boxlite")]
pub async fn reconcile_runtime_transactions<B: CowBackend>(home: &Path, backend: &B) -> Result<()> {
    let _lock = acquire_runtime_lock(home).await?;
    ensure_home(home)?;
    let runtime = crate::runtime::BoxLiteRuntimeBackend::new(home)?;
    recover_runtime_transactions(home, backend, &runtime).await
}

#[cfg(feature = "microvm-boxlite")]
async fn recover_if_needed_locked<B: CowBackend>(home: &Path, backend: &B) -> Result<()> {
    if has_any_runtime_transactions(home)? {
        let runtime = crate::runtime::BoxLiteRuntimeBackend::new(home)?;
        recover_runtime_transactions(home, backend, &runtime).await?;
    }
    Ok(())
}

#[cfg(feature = "microvm-boxlite")]
pub async fn create_workspace_reconciled<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    from: &Path,
    from_revset: Option<&str>,
) -> Result<PathBuf> {
    validate_name(name)?;
    let _lock = acquire_runtime_lock(home).await?;
    ensure_home(home)?;
    recover_if_needed_locked(home, backend).await?;
    create_workspace_locked(home, backend, name, from, from_revset)
}

#[cfg(feature = "microvm-boxlite")]
pub async fn list_workspaces_reconciled<B: CowBackend>(
    home: &Path,
    backend: &B,
) -> Result<Vec<Metadata>> {
    let _lock = acquire_runtime_lock(home).await?;
    ensure_home(home)?;
    recover_if_needed_locked(home, backend).await?;
    list_workspaces(home)
}

#[cfg(feature = "microvm-boxlite")]
pub async fn read_workspace_reconciled<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
) -> Result<Metadata> {
    validate_name(name)?;
    let _lock = acquire_runtime_lock(home).await?;
    ensure_home(home)?;
    recover_if_needed_locked(home, backend).await?;
    read_metadata(&state_dir(home, name)).with_context(|| format!("workspace not found: {name}"))
}

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
    has_any_runtime_transactions(home)?;
    create_workspace_locked(home, backend, name, from, from_revset)
}

fn create_workspace_locked<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    from: &Path,
    from_revset: Option<&str>,
) -> Result<PathBuf> {
    create_workspace_locked_with_hook(home, backend, name, from, from_revset, |_, _| Ok(()))
}

fn create_workspace_locked_with_hook<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    from: &Path,
    from_revset: Option<&str>,
    before_storage: impl FnOnce(&Path, Option<&JjRegistrationPreflight>) -> Result<()>,
) -> Result<PathBuf> {
    let source = from.canonicalize()?;
    if !source.is_dir() {
        bail!("source must be a directory: {}", source.display());
    }

    let state_dir = state_dir(home, name);
    let workspace_path = workspace_dir(home, name);
    if state_dir.exists() || workspace_path.exists() {
        bail!("workspace already exists: {}", state_dir.display());
    }
    let jj_preflight = preflight_jj_registration(&source, name, from_revset)?;
    before_storage(&source, jj_preflight.as_ref())?;
    let workspace_path = backend.create(&state_dir, &workspace_path, &source)?;

    let jj = match register_jj_if_needed(&workspace_path, jj_preflight) {
        Ok(jj) => jj,
        Err(err) => {
            backend.destroy(&state_dir, &workspace_path).with_context(|| {
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
    workspace_path: &Path,
    preflight: Option<JjRegistrationPreflight>,
) -> Result<Option<JjMetadata>> {
    let Some(preflight) = preflight else {
        return Ok(None);
    };

    let registration = register_pando_workspace(workspace_path, preflight)?;
    Ok(Some(JjMetadata {
        workspace_name: Some(registration.workspace_name),
        base_commit: Some(registration.base_commit),
        base_revision: Some(registration.base_revision),
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
    if has_runtime_transaction(home, name)? {
        bail!("workspace has an incomplete runtime transaction; retry with a runtime-enabled Pando build");
    }
    destroy_workspace_locked(home, backend, name, keep_jj_workspace)
}

fn destroy_workspace_locked<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    keep_jj_workspace: bool,
) -> Result<()> {
    destroy_workspace_storage_locked(home, backend, name, keep_jj_workspace, true)
}

fn destroy_workspace_storage_locked<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    keep_jj_workspace: bool,
    reject_runtime: bool,
) -> Result<()> {
    let state_dir = state_dir(home, name);
    let workspace_path = workspace_dir(home, name);
    if let Ok(metadata) = read_metadata(&state_dir) {
        if metadata.workspace_path != workspace_path {
            bail!(
                "workspace metadata path does not match managed path: {}",
                workspace_path.display()
            );
        }
        if reject_runtime && metadata.runtime.is_some() {
            bail!("workspace has a runtime; remove it with a runtime-enabled Pando build");
        }
    }
    if !keep_jj_workspace {
        forget_registered_jj_workspace(&state_dir)?;
        if !reject_runtime {
            injected_crash("remove-jj-forgotten");
        }
    }

    backend.destroy(&state_dir, &workspace_path)
}

#[cfg(feature = "microvm-boxlite")]
pub async fn create_workspace_with_runtime<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    from: &Path,
    from_revset: Option<&str>,
    image: String,
    policy: crate::runtime::RuntimePolicy,
) -> Result<PathBuf> {
    use crate::runtime::{BoxLiteRuntimeBackend, RuntimeBackend, RuntimeSpec};

    validate_name(name)?;
    policy.validate()?;
    crate::runtime::validate_runtime_platform()?;
    let _lock = acquire_runtime_lock(home).await?;
    ensure_home(home)?;
    let runtime = BoxLiteRuntimeBackend::new(home)?;
    recover_runtime_transactions(home, backend, &runtime).await?;
    let provider_name = runtime_attempt_name(name)?;
    match runtime.find(&provider_name).await {
        Ok(None) => {}
        Ok(Some(_)) => bail!("runtime create token unexpectedly already exists"),
        Err(error) => return Err(error.context("could not preflight runtime create token")),
    }
    let mut transaction = RuntimeCreateTransaction {
        name: name.to_owned(),
        provider_name: provider_name.clone(),
        image: image.clone(),
        canonical_root: from.canonicalize()?,
        identity: None,
    };
    let workspace_path =
        create_workspace_locked_with_hook(home, backend, name, from, from_revset, |_, jj| {
            if let Some(jj) = jj {
                jj.preflight_runtime_mount()?;
            }
            if runtime_transaction_path(home, name).exists() {
                bail!("runtime create transaction already exists for workspace: {name}");
            }
            write_runtime_transaction(home, &transaction)?;
            injected_crash("create-intent");
            Ok(())
        })?;
    let metadata = match read_metadata(&state_dir(home, name)) {
        Ok(metadata) => metadata,
        Err(error) => {
            destroy_workspace_locked(home, backend, name, false).with_context(|| {
                format!("workspace metadata validation failed ({error:#}); rollback failed")
            })?;
            return Err(error);
        }
    };
    let jj_mount = match jj_runtime_mount(&workspace_path, &metadata.canonical_root) {
        Ok(mount) => mount,
        Err(error) => {
            destroy_workspace_locked(home, backend, name, false).with_context(|| {
                format!("jj runtime mount validation failed ({error:#}); workspace rollback failed")
            })?;
            return Err(error.context("jj runtime mount validation failed"));
        }
    };
    let mut runtime_spec = RuntimeSpec::new(image.clone())
        .with_workspace(workspace_path.clone())
        .with_name(provider_name.clone())
        .with_policy(policy);
    if let Some(mount) = jj_mount {
        runtime_spec = runtime_spec.with_jj_store(mount);
        #[cfg(target_os = "linux")]
        {
            let guest_jj_stage = match crate::runtime::prepare_guest_jj_stage(&workspace_path) {
                Ok(path) => path,
                Err(error) => {
                    destroy_workspace_locked(home, backend, name, false).with_context(|| {
                        format!("guest jj staging failed ({error:#}); workspace rollback failed")
                    })?;
                    return Err(error.context("guest jj staging failed"));
                }
            };
            runtime_spec = runtime_spec.with_guest_jj_stage(guest_jj_stage);
        }
    }
    let identity = match runtime.create(runtime_spec).await {
        Ok(identity) => identity,
        Err(error) => {
            if let Err(reconcile) =
                rollback_failed_runtime_create(home, backend, name, &runtime, &provider_name).await
            {
                return Err(error.context(format!(
                    "runtime creation failed and provider reconciliation failed ({reconcile:#}); workspace retained at {}",
                    workspace_path.display()
                )));
            }
            return Err(error);
        }
    };
    injected_crash("provider-created");
    transaction.identity = Some(identity.clone());
    if let Err(error) = write_runtime_transaction(home, &transaction) {
        if let Err(cleanup) =
            rollback_created_runtime(home, backend, name, &runtime, &identity).await
        {
            return Err(error.context(format!(
                "could not persist provider identity and rollback failed ({cleanup:#})"
            )));
        }
        return Err(error);
    }

    if let Err(error) = runtime.start(&identity).await {
        if let Err(cleanup) =
            rollback_created_runtime(home, backend, name, &runtime, &identity).await
        {
            return Err(error.context(format!(
                "runtime start failed; provider cleanup failed ({cleanup:#}); workspace retained at {}",
                workspace_path.display()
            )));
        }
        return Err(error);
    }
    injected_crash("provider-started");

    let state_dir = state_dir(home, name);
    let mut metadata = match read_metadata(&state_dir) {
        Ok(metadata) => metadata,
        Err(error) => {
            if let Err(cleanup) =
                rollback_created_runtime(home, backend, name, &runtime, &identity).await
            {
                return Err(error.context(format!(
                    "could not read workspace metadata after runtime start; provider cleanup failed ({cleanup:#}); workspace retained at {} with provider id {}",
                    workspace_path.display(), identity.as_str()
                )));
            }
            return Err(error);
        }
    };
    metadata.runtime = Some(RuntimeMetadata {
        identity,
        image,
        policy,
    });
    if let Err(error) = write_metadata(&state_dir, &metadata) {
        if let Some(runtime_metadata) = metadata.runtime.as_ref() {
            if let Err(cleanup) =
                rollback_created_runtime(home, backend, name, &runtime, &runtime_metadata.identity)
                    .await
            {
                return Err(error.context(format!(
                    "runtime metadata publication failed; provider cleanup failed ({cleanup:#}); workspace retained at {} with provider id {}",
                    workspace_path.display(),
                    runtime_metadata.identity.as_str()
                )));
            }
        }
        return Err(error);
    }
    injected_crash("metadata-published");
    clear_runtime_transaction(home, name)
        .context("workspace committed but runtime create transaction could not be cleared")?;
    Ok(workspace_path)
}

#[cfg(feature = "microvm-boxlite")]
fn runtime_attempt_name(workspace_name: &str) -> Result<String> {
    use std::io::Read;

    runtime_attempt_name_with(workspace_name, |random| {
        fs::File::open("/dev/urandom")
            .context("could not open system random source")?
            .read_exact(random)
            .context("could not generate runtime create token")
    })
}

#[cfg(feature = "microvm-boxlite")]
fn runtime_attempt_name_with(
    workspace_name: &str,
    fill_random: impl FnOnce(&mut [u8]) -> Result<()>,
) -> Result<String> {
    let mut random = [0_u8; 16];
    fill_random(&mut random)?;
    let token = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("pando-{workspace_name}-{token}"))
}

#[cfg(feature = "microvm-boxlite")]
async fn rollback_failed_runtime_create<B: CowBackend, R: crate::runtime::RuntimeBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    runtime: &R,
    provider_name: &str,
) -> Result<()> {
    if let Some(identity) = runtime
        .find(provider_name)
        .await
        .context("could not determine whether BoxLite allocated a partial runtime")?
    {
        rollback_created_runtime(home, backend, name, runtime, &identity).await
    } else {
        destroy_workspace_locked(home, backend, name, false)
            .context("provider reported no partial runtime but workspace rollback failed")
    }
}

#[cfg(feature = "microvm-boxlite")]
async fn rollback_created_runtime<B: CowBackend, R: crate::runtime::RuntimeBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    runtime: &R,
    identity: &crate::runtime::RuntimeIdentity,
) -> Result<()> {
    runtime
        .stop(identity)
        .await
        .context("could not prove runtime stopped during rollback")?;
    runtime
        .remove(identity)
        .await
        .context("could not remove runtime during rollback")?;
    destroy_workspace_locked(home, backend, name, false)
        .context("runtime was removed but workspace rollback failed")
}

#[cfg(feature = "microvm-boxlite")]
pub async fn execute_in_workspace(
    home: &Path,
    name: &str,
    arguments: Vec<String>,
    terminal: bool,
) -> Result<i32> {
    use crate::runtime::{BoxLiteRuntimeBackend, RuntimeBackend, RuntimeCommand};

    validate_name(name)?;
    let _lock = acquire_runtime_lock(home).await?;
    let runtime = BoxLiteRuntimeBackend::new(home)?;
    recover_platform_runtime_transactions(home, &runtime).await?;
    let metadata = read_metadata(&state_dir(home, name))
        .with_context(|| format!("workspace not found: {name}"))?;
    let runtime_metadata = metadata
        .runtime
        .ok_or_else(|| anyhow::anyhow!("workspace has no runtime: {name}"))?;
    let command = if terminal {
        RuntimeCommand::terminal(arguments)
    } else {
        RuntimeCommand::new(arguments)
    };
    runtime.execute(&runtime_metadata.identity, command).await
}

#[cfg(feature = "microvm-boxlite")]
pub async fn stop_workspace_runtime(home: &Path, name: &str) -> Result<()> {
    use crate::runtime::{BoxLiteRuntimeBackend, RuntimeBackend};

    validate_name(name)?;
    let _lock = acquire_runtime_lock(home).await?;
    let runtime = BoxLiteRuntimeBackend::new(home)?;
    recover_platform_runtime_transactions(home, &runtime).await?;
    let metadata = read_metadata(&state_dir(home, name))?;
    let runtime_metadata = metadata
        .runtime
        .ok_or_else(|| anyhow::anyhow!("workspace has no runtime: {name}"))?;
    runtime.stop(&runtime_metadata.identity).await?;
    Ok(())
}

#[cfg(feature = "microvm-boxlite")]
pub async fn inspect_workspace_runtime(
    home: &Path,
    name: &str,
) -> Result<(Metadata, Option<crate::runtime::RuntimeInfo>)> {
    use crate::runtime::{BoxLiteRuntimeBackend, RuntimeBackend};

    validate_name(name)?;
    let _lock = acquire_runtime_lock(home).await?;
    recover_if_needed_locked(home, &crate::backend::PlatformCowBackend::default()).await?;
    let metadata = read_metadata(&state_dir(home, name))
        .with_context(|| format!("workspace not found: {name}"))?;
    let Some(runtime_metadata) = metadata.runtime.clone() else {
        return Ok((metadata, None));
    };
    let runtime = BoxLiteRuntimeBackend::new(home)?;
    let info = runtime.inspect(&runtime_metadata.identity).await?;
    Ok((metadata, Some(info)))
}

#[cfg(feature = "microvm-boxlite")]
pub async fn destroy_workspace_with_runtime<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    keep_jj_workspace: bool,
) -> Result<()> {
    use crate::runtime::BoxLiteRuntimeBackend;

    validate_name(name)?;
    let _lock = acquire_runtime_lock(home).await?;
    ensure_home(home)?;
    recover_if_needed_locked(home, backend).await?;
    let state_dir = state_dir(home, name);
    let metadata = read_metadata(&state_dir)?;
    if let Some(runtime_metadata) = metadata.runtime.as_ref() {
        let runtime = BoxLiteRuntimeBackend::new(home)?;
        destroy_runtime_locked(
            backend,
            &runtime,
            RuntimeRemoval {
                home,
                name,
                keep_jj_workspace,
                runtime: runtime_metadata,
            },
        )
        .await
    } else {
        destroy_workspace_storage_locked(home, backend, name, keep_jj_workspace, true)
    }
}

#[cfg(feature = "microvm-boxlite")]
struct RuntimeRemoval<'a> {
    home: &'a Path,
    name: &'a str,
    keep_jj_workspace: bool,
    runtime: &'a RuntimeMetadata,
}

#[cfg(feature = "microvm-boxlite")]
async fn destroy_runtime_locked<B, R>(
    backend: &B,
    runtime: &R,
    removal: RuntimeRemoval<'_>,
) -> Result<()>
where
    B: CowBackend,
    R: crate::runtime::RuntimeBackend,
{
    if runtime
        .contains(&removal.runtime.identity)
        .await
        .context("could not authoritatively reconcile runtime identity before removal")?
    {
        runtime.stop(&removal.runtime.identity).await?;
        injected_crash("remove-stopped");
        runtime.remove(&removal.runtime.identity).await?;
        injected_crash("remove-provider-removed");
    }
    destroy_workspace_storage_locked(
        removal.home,
        backend,
        removal.name,
        removal.keep_jj_workspace,
        false,
    )
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
    let transactions = crate::metadata::runtime_transaction_directories(home)?;
    let state_root = state_root(home);
    if !state_root.exists() {
        return Ok(Vec::new());
    }

    let mut metadata = Vec::new();
    for entry in fs::read_dir(state_root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if transactions
                .iter()
                .any(|transaction| transaction.name() == entry.file_name().to_string_lossy())
            {
                continue;
            }
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
    #[cfg(feature = "microvm-boxlite")]
    use super::{create_workspace_with_runtime, destroy_workspace_with_runtime};
    #[cfg(feature = "microvm-boxlite")]
    use crate::metadata::runtime_transaction_path;
    use crate::{
        backend::SimpleCowBackend,
        home::{state_dir, workspace_dir},
        metadata::{
            metadata_path, read_metadata, write_metadata, write_runtime_transaction, JjMetadata,
            Metadata, RuntimeCreateTransaction, RuntimeMetadata,
        },
        runtime::RuntimeIdentity,
    };
    #[cfg(feature = "microvm-boxlite")]
    use anyhow::Result;
    use proptest::prelude::*;
    #[cfg(feature = "microvm-boxlite")]
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::{collections::BTreeSet, fs, path::Path};

    #[test]
    fn backend_agnostic_lifecycle_uses_home_name_state_dir() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("README.md"), "demo").unwrap();

        let home = tempfile::tempdir().unwrap();
        let backend = SimpleCowBackend;

        let workspace =
            create_workspace(home.path(), &backend, "demo", source.path(), None).unwrap();
        let state_dir = state_dir(home.path(), "demo");

        assert_eq!(workspace, workspace_dir(home.path(), "demo"));
        assert!(workspace.join("README.md").exists());
        assert!(metadata_path(&state_dir).exists());
        assert_eq!(list_workspaces(home.path()).unwrap()[0].name, "demo");

        destroy_workspace(home.path(), &backend, "demo", false).unwrap();
        assert!(!state_dir.exists());
        assert!(list_workspaces(home.path()).unwrap().is_empty());
    }

    #[test]
    fn provisional_runtime_workspace_is_hidden_and_host_destroy_fails_closed() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("README.md"), "demo").unwrap();
        let home = tempfile::tempdir().unwrap();
        let backend = SimpleCowBackend;
        create_workspace(home.path(), &backend, "demo", source.path(), None).unwrap();
        write_runtime_transaction(
            home.path(),
            &RuntimeCreateTransaction {
                name: "demo".to_owned(),
                provider_name: "owned-token".to_owned(),
                image: "alpine:3.22".to_owned(),
                canonical_root: source.path().to_owned(),
                identity: None,
            },
        )
        .unwrap();

        assert!(list_workspaces(home.path()).unwrap().is_empty());
        let error = destroy_workspace(home.path(), &backend, "demo", false).unwrap_err();
        assert!(error.to_string().contains("incomplete runtime transaction"));
        assert!(state_dir(home.path(), "demo").exists());
        assert!(workspace_dir(home.path(), "demo").exists());
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn runtime_destroy_rejects_invalid_name_before_state_or_provider_access() {
        let home = tempfile::tempdir().unwrap();
        let escaped_state = home.path().join("victim");
        let mut metadata = Metadata::new(
            "victim",
            home.path().join("source"),
            home.path().join("workspaces/victim"),
        );
        metadata.runtime = Some(RuntimeMetadata {
            identity: RuntimeIdentity::new("must-not-be-inspected"),
            image: "alpine:3.22".to_owned(),
            policy: Default::default(),
        });
        write_metadata(&escaped_state, &metadata).unwrap();

        let error =
            destroy_workspace_with_runtime(home.path(), &SimpleCowBackend, "../victim", false)
                .await
                .unwrap_err();

        assert!(error.to_string().contains("path separators"));
        assert!(metadata_path(&escaped_state).exists());
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn runtime_create_rejects_invalid_name_before_provider_initialization() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();

        let error = create_workspace_with_runtime(
            home.path(),
            &SimpleCowBackend,
            "../invalid",
            source.path(),
            None,
            "alpine:3.22".to_owned(),
            crate::runtime::RuntimePolicy::default(),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("path separators"));
        assert!(!home.path().join("runtime").exists());
        assert!(!home.path().join("workspaces").exists());
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn runtime_create_rejects_invalid_policy_before_workspace_mutation() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let policy = crate::runtime::RuntimePolicy {
            cpu_count: 0,
            seccomp: crate::runtime::RuntimeSeccompPolicy::AllowUnqualifiedProvider,
            ..Default::default()
        };

        let error = create_workspace_with_runtime(
            home.path(),
            &SimpleCowBackend,
            "invalid-policy",
            source.path(),
            None,
            "alpine:3.22".to_owned(),
            policy,
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("CPU count"));
        assert!(!home.path().join("runtime").exists());
        assert!(!home.path().join("workspaces").exists());
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test(flavor = "current_thread")]
    async fn async_runtime_lock_waits_without_blocking_current_thread() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("state")).unwrap();
        let held = crate::home::PandoLock::acquire(home.path()).unwrap();
        let path = home.path().to_owned();
        let waiter = tokio::spawn(async move { super::acquire_runtime_lock(&path).await.unwrap() });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(held);
        let acquired = tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
            .await
            .expect("async lock acquisition deadlocked")
            .unwrap();
        drop(acquired);
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test(flavor = "current_thread")]
    async fn reconciled_list_cannot_observe_between_locked_workspace_transitions() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("README.md"), "demo").unwrap();
        fs::create_dir_all(home.path().join("state")).unwrap();
        let held = crate::home::PandoLock::acquire(home.path()).unwrap();
        let home_path = home.path().to_owned();
        let waiter = tokio::spawn(async move {
            super::list_workspaces_reconciled(&home_path, &SimpleCowBackend).await
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        super::create_workspace_locked(home.path(), &SimpleCowBackend, "demo", source.path(), None)
            .unwrap();
        drop(held);

        let listed = waiter.await.unwrap().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "demo");
    }

    #[cfg(feature = "microvm-boxlite")]
    #[test]
    fn runtime_attempt_generation_failure_is_reported_before_workspace_creation() {
        let error = super::runtime_attempt_name_with("demo", |_| {
            anyhow::bail!("injected random source failure")
        })
        .unwrap_err();
        assert!(error.to_string().contains("random source failure"));
    }

    #[cfg(feature = "microvm-boxlite")]
    struct FailingRuntime {
        stop_fails: bool,
        remove_fails: bool,
        find_identity: Option<RuntimeIdentity>,
        find_fails: bool,
    }

    #[cfg(feature = "microvm-boxlite")]
    impl crate::runtime::RuntimeBackend for FailingRuntime {
        async fn create(&self, _spec: crate::runtime::RuntimeSpec) -> Result<RuntimeIdentity> {
            unreachable!()
        }

        async fn start(&self, _identity: &RuntimeIdentity) -> Result<()> {
            unreachable!()
        }

        async fn find(&self, _name: &str) -> Result<Option<RuntimeIdentity>> {
            if self.find_fails {
                anyhow::bail!("injected lookup failure")
            }
            Ok(self.find_identity.clone())
        }

        async fn contains(&self, identity: &RuntimeIdentity) -> Result<bool> {
            if self.find_fails {
                anyhow::bail!("injected identity query failure")
            }
            Ok(self.find_identity.as_ref() == Some(identity))
        }

        async fn inspect(
            &self,
            _identity: &RuntimeIdentity,
        ) -> Result<crate::runtime::RuntimeInfo> {
            unreachable!()
        }

        async fn stop(&self, _identity: &RuntimeIdentity) -> Result<()> {
            if self.stop_fails {
                anyhow::bail!("injected ownership proof failure")
            }
            Ok(())
        }

        async fn remove(&self, _identity: &RuntimeIdentity) -> Result<()> {
            if self.remove_fails {
                anyhow::bail!("injected provider remove failure")
            }
            Ok(())
        }

        async fn execute(
            &self,
            _identity: &RuntimeIdentity,
            _command: crate::runtime::RuntimeCommand,
        ) -> Result<i32> {
            unreachable!()
        }
    }

    #[cfg(feature = "microvm-boxlite")]
    fn provisional_transaction(home: &Path, source: &Path, identity: Option<RuntimeIdentity>) {
        write_runtime_transaction(
            home,
            &RuntimeCreateTransaction {
                name: "demo".to_owned(),
                provider_name: "pando-demo-owned-token".to_owned(),
                image: "alpine:3.22".to_owned(),
                canonical_root: source.to_owned(),
                identity,
            },
        )
        .unwrap();
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn recovery_removes_only_recorded_identity_and_provisional_storage() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let state = state_dir(home.path(), "demo");
        let workspace = workspace_dir(home.path(), "demo");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        write_metadata(
            &state,
            &Metadata::new("demo", source.path().to_owned(), workspace.clone()),
        )
        .unwrap();
        provisional_transaction(
            home.path(),
            source.path(),
            Some(RuntimeIdentity::new("owned")),
        );

        super::recover_runtime_transactions(
            home.path(),
            &SimpleCowBackend,
            &FailingRuntime {
                stop_fails: true,
                remove_fails: true,
                find_identity: Some(RuntimeIdentity::new("unrelated")),
                find_fails: false,
            },
        )
        .await
        .unwrap();

        assert!(!state.exists());
        assert!(!workspace.exists());
        assert!(!runtime_transaction_path(home.path(), "demo").exists());
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn recovery_failure_retains_journal_and_storage_for_retry() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let state = state_dir(home.path(), "demo");
        let workspace = workspace_dir(home.path(), "demo");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        write_metadata(
            &state,
            &Metadata::new("demo", source.path().to_owned(), workspace.clone()),
        )
        .unwrap();
        provisional_transaction(
            home.path(),
            source.path(),
            Some(RuntimeIdentity::new("owned")),
        );

        let error = super::recover_runtime_transactions(
            home.path(),
            &SimpleCowBackend,
            &FailingRuntime {
                stop_fails: true,
                remove_fails: false,
                find_identity: Some(RuntimeIdentity::new("owned")),
                find_fails: false,
            },
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("ownership proof failure"));
        assert!(runtime_transaction_path(home.path(), "demo").exists());
        assert!(state.exists());
        assert!(workspace.exists());
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn recovery_removes_attempt_owned_orphan_journal_temp() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join("transactions/demo");
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(
            home.path().join("transactions"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(
            directory.join(crate::metadata::RUNTIME_TRANSACTION_TEMP_FILE),
            "partial",
        )
        .unwrap();
        fs::set_permissions(
            directory.join(crate::metadata::RUNTIME_TRANSACTION_TEMP_FILE),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();

        super::recover_runtime_transactions(
            home.path(),
            &SimpleCowBackend,
            &FailingRuntime {
                stop_fails: true,
                remove_fails: true,
                find_identity: None,
                find_fails: true,
            },
        )
        .await
        .unwrap();

        assert!(!directory.exists());
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn recovery_treats_published_runtime_metadata_as_commit_point() {
        let home = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let state = state_dir(home.path(), "demo");
        let workspace = workspace_dir(home.path(), "demo");
        fs::create_dir_all(&workspace).unwrap();
        let mut metadata = Metadata::new("demo", source.path().to_owned(), workspace.clone());
        metadata.runtime = Some(RuntimeMetadata {
            identity: RuntimeIdentity::new("committed"),
            image: "alpine:3.22".to_owned(),
            policy: Default::default(),
        });
        write_metadata(&state, &metadata).unwrap();
        provisional_transaction(
            home.path(),
            source.path(),
            Some(RuntimeIdentity::new("committed")),
        );

        super::recover_runtime_transactions(
            home.path(),
            &SimpleCowBackend,
            &FailingRuntime {
                stop_fails: true,
                remove_fails: true,
                find_identity: None,
                find_fails: true,
            },
        )
        .await
        .unwrap();

        assert!(state.exists());
        assert!(workspace.exists());
        assert!(!runtime_transaction_path(home.path(), "demo").exists());
    }

    #[cfg(feature = "microvm-boxlite")]
    struct DestroyCountingBackend(Arc<AtomicUsize>);

    #[cfg(feature = "microvm-boxlite")]
    impl crate::backend::CowBackend for DestroyCountingBackend {
        fn create(&self, _: &Path, _: &Path, _: &Path) -> Result<std::path::PathBuf> {
            unreachable!()
        }

        fn destroy(&self, _: &Path, _: &Path) -> Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn migrate_legacy(
            &self,
            _: &Path,
            _: &Path,
            _: &Path,
            _: &Path,
        ) -> Result<std::path::PathBuf> {
            unreachable!()
        }

        fn resume_migration(&self, _: &Path, _: &Path, _: &Path) -> Result<std::path::PathBuf> {
            unreachable!()
        }
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn start_failure_rollback_retains_workspace_when_stop_proof_fails() {
        let destroys = Arc::new(AtomicUsize::new(0));
        let error = super::rollback_created_runtime(
            Path::new("/tmp/pando-test-home"),
            &DestroyCountingBackend(Arc::clone(&destroys)),
            "demo",
            &FailingRuntime {
                stop_fails: true,
                remove_fails: false,
                find_identity: None,
                find_fails: false,
            },
            &RuntimeIdentity::new("provider"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("prove runtime stopped"));
        assert_eq!(destroys.load(Ordering::SeqCst), 0);
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn metadata_failure_rollback_retains_workspace_when_remove_fails() {
        let destroys = Arc::new(AtomicUsize::new(0));
        let error = super::rollback_created_runtime(
            Path::new("/tmp/pando-test-home"),
            &DestroyCountingBackend(Arc::clone(&destroys)),
            "demo",
            &FailingRuntime {
                stop_fails: false,
                remove_fails: true,
                find_identity: None,
                find_fails: false,
            },
            &RuntimeIdentity::new("provider"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("remove runtime"));
        assert_eq!(destroys.load(Ordering::SeqCst), 0);
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn confirmed_absent_provider_allows_storage_cleanup_without_metadata_transition() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = state_dir(home.path(), "demo");
        let workspace_path = workspace_dir(home.path(), "demo");
        fs::create_dir_all(&workspace_path).unwrap();
        let runtime_metadata = RuntimeMetadata {
            identity: RuntimeIdentity::new("provider"),
            image: "alpine:3.22".to_owned(),
            policy: Default::default(),
        };
        let mut metadata = Metadata::new("demo", home.path().to_owned(), workspace_path);
        metadata.runtime = Some(runtime_metadata.clone());
        write_metadata(&state_dir, &metadata).unwrap();
        let destroys = Arc::new(AtomicUsize::new(0));

        super::destroy_runtime_locked(
            &DestroyCountingBackend(Arc::clone(&destroys)),
            &FailingRuntime {
                stop_fails: true,
                remove_fails: true,
                find_identity: None,
                find_fails: false,
            },
            super::RuntimeRemoval {
                home: home.path(),
                name: "demo",
                keep_jj_workspace: false,
                runtime: &runtime_metadata,
            },
        )
        .await
        .unwrap();
        assert_eq!(destroys.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn ambiguous_provider_identity_query_fails_closed_before_removal() {
        let home = tempfile::tempdir().unwrap();
        let runtime_metadata = RuntimeMetadata {
            identity: RuntimeIdentity::new("provider"),
            image: "alpine:3.22".to_owned(),
            policy: Default::default(),
        };
        let workspace_path = workspace_dir(home.path(), "demo");
        fs::create_dir_all(&workspace_path).unwrap();
        let mut metadata = Metadata::new("demo", home.path().to_owned(), workspace_path);
        metadata.runtime = Some(runtime_metadata.clone());
        write_metadata(&state_dir(home.path(), "demo"), &metadata).unwrap();
        let destroys = Arc::new(AtomicUsize::new(0));
        let error = super::destroy_runtime_locked(
            &DestroyCountingBackend(Arc::clone(&destroys)),
            &FailingRuntime {
                stop_fails: true,
                remove_fails: true,
                find_identity: None,
                find_fails: true,
            },
            super::RuntimeRemoval {
                home: home.path(),
                name: "demo",
                keep_jj_workspace: false,
                runtime: &runtime_metadata,
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("authoritatively reconcile"));
        assert_eq!(destroys.load(Ordering::SeqCst), 0);
        assert!(read_metadata(&state_dir(home.path(), "demo"))
            .unwrap()
            .runtime
            .is_some());
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn partial_create_is_removed_before_workspace_destroy() {
        let destroys = Arc::new(AtomicUsize::new(0));
        super::rollback_failed_runtime_create(
            Path::new("/tmp/pando-test-home"),
            &DestroyCountingBackend(Arc::clone(&destroys)),
            "demo",
            &FailingRuntime {
                stop_fails: false,
                remove_fails: false,
                find_identity: Some(RuntimeIdentity::new("partial")),
                find_fails: false,
            },
            "pando-demo",
        )
        .await
        .unwrap();
        assert_eq!(destroys.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn authoritative_no_partial_allocation_allows_workspace_destroy() {
        let destroys = Arc::new(AtomicUsize::new(0));
        super::rollback_failed_runtime_create(
            Path::new("/tmp/pando-test-home"),
            &DestroyCountingBackend(Arc::clone(&destroys)),
            "demo",
            &FailingRuntime {
                stop_fails: false,
                remove_fails: false,
                find_identity: None,
                find_fails: false,
            },
            "pando-demo",
        )
        .await
        .unwrap();
        assert_eq!(destroys.load(Ordering::SeqCst), 1);
    }

    #[cfg(feature = "microvm-boxlite")]
    #[tokio::test]
    async fn create_lookup_or_partial_cleanup_failure_retains_workspace() {
        for runtime in [
            FailingRuntime {
                stop_fails: false,
                remove_fails: false,
                find_identity: None,
                find_fails: true,
            },
            FailingRuntime {
                stop_fails: true,
                remove_fails: false,
                find_identity: Some(RuntimeIdentity::new("partial")),
                find_fails: false,
            },
        ] {
            let destroys = Arc::new(AtomicUsize::new(0));
            assert!(super::rollback_failed_runtime_create(
                Path::new("/tmp/pando-test-home"),
                &DestroyCountingBackend(Arc::clone(&destroys)),
                "demo",
                &runtime,
                "pando-demo",
            )
            .await
            .is_err());
            assert_eq!(destroys.load(Ordering::SeqCst), 0);
        }
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
        let workspace_path = workspace_dir(home.path(), "demo");
        let mut metadata = Metadata::new("demo", home.path().join("missing-root"), workspace_path);
        metadata.jj = Some(JjMetadata {
            workspace_name: Some("pando-demo".to_owned()),
            base_commit: None,
            base_revision: None,
        });
        write_metadata(&state_dir, &metadata).unwrap();

        let result = destroy_workspace(home.path(), &SimpleCowBackend, "demo", false);

        assert!(result.is_err());
        assert!(
            state_dir.exists(),
            "failed jj forget must not remove pando state"
        );
    }

    #[test]
    fn destroy_rejects_workspace_path_outside_managed_layout() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let state_dir = state_dir(home.path(), "demo");
        write_metadata(
            &state_dir,
            &Metadata::new(
                "demo",
                home.path().to_path_buf(),
                outside.path().to_path_buf(),
            ),
        )
        .unwrap();

        let result = destroy_workspace(home.path(), &SimpleCowBackend, "demo", false);

        assert!(result.is_err());
        assert!(outside.path().exists());
        assert!(state_dir.exists());
    }

    #[test]
    fn host_only_destroy_preserves_runtime_backed_workspace() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = state_dir(home.path(), "demo");
        let workspace_path = workspace_dir(home.path(), "demo");
        fs::create_dir_all(&workspace_path).unwrap();
        let mut metadata = Metadata::new("demo", home.path().to_path_buf(), workspace_path.clone());
        metadata.runtime = Some(RuntimeMetadata {
            identity: RuntimeIdentity::new("box-123"),
            image: "alpine:3.22".to_owned(),
            policy: Default::default(),
        });
        write_metadata(&state_dir, &metadata).unwrap();

        let result = destroy_workspace(home.path(), &SimpleCowBackend, "demo", false);

        assert!(result.is_err());
        assert!(state_dir.exists());
        assert!(workspace_path.exists());
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

    fn assert_list_matches(home: &Path, existing: &BTreeSet<&'static str>) {
        let listed: BTreeSet<String> = list_workspaces(home)
            .unwrap()
            .into_iter()
            .map(|metadata| metadata.name)
            .collect();
        let expected: BTreeSet<String> = existing.iter().map(|name| (*name).to_owned()).collect();
        assert_eq!(listed, expected);
    }

    fn write_source_tree(source: &Path) {
        fs::create_dir(source.join("nested")).unwrap();
        fs::write(source.join("README.md"), "demo").unwrap();
        fs::write(source.join("nested/file.txt"), "nested").unwrap();
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 32, .. ProptestConfig::default() })]

        #[test]
        fn simple_backend_lifecycle_operation_sequences_preserve_list_invariant(
            operations in prop::collection::vec(operation(), 0..24)
        ) {
            const NAMES: [&str; 3] = ["alpha", "beta", "gamma"];

            let source = tempfile::tempdir().unwrap();
            write_source_tree(source.path());
            prop_assert!(!source.path().join(".jj").exists(), "property test source must stay outside jj paths");

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
                            let state_dir = state_dir(home.path(), name);
                            prop_assert_eq!(&workspace, &workspace_dir(home.path(), name));
                            prop_assert!(workspace.join("README.md").exists());
                            prop_assert!(workspace.join("nested/file.txt").exists());
                            prop_assert!(read_metadata(&state_dir).unwrap().jj.is_none());
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
