use crate::{
    backend::CowBackend,
    home::{ensure_home, state_dir, state_root, workspace_dir, PandoLock},
    jj::{
        forget_pando_workspace, pando_workspace_name, preflight_jj_registration,
        register_pando_workspace, JjRegistrationPreflight,
    },
    metadata::{read_metadata, write_metadata, JjMetadata, Metadata},
    naming::validate_name,
};

#[cfg(feature = "microvm-boxlite")]
use crate::jj::jj_runtime_mount;
#[cfg(feature = "microvm-boxlite")]
use crate::metadata::RuntimeMetadata;
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
    create_workspace_locked(home, backend, name, from, from_revset)
}

fn create_workspace_locked<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    from: &Path,
    from_revset: Option<&str>,
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
    destroy_workspace_locked(home, backend, name, keep_jj_workspace)
}

fn destroy_workspace_locked<B: CowBackend>(
    home: &Path,
    backend: &B,
    name: &str,
    keep_jj_workspace: bool,
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
        if metadata.runtime.is_some() {
            bail!("workspace has a runtime; remove it with a runtime-enabled Pando build");
        }
    }
    if !keep_jj_workspace {
        forget_registered_jj_workspace(&state_dir)?;
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
) -> Result<PathBuf> {
    use crate::runtime::{BoxLiteRuntimeBackend, RuntimeBackend, RuntimeSpec};

    validate_name(name)?;
    let _lock = acquire_runtime_lock(home).await?;
    ensure_home(home)?;
    let provider_name = runtime_attempt_name(name)?;
    let workspace_path = create_workspace_locked(home, backend, name, from, from_revset)?;
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
    let runtime = match BoxLiteRuntimeBackend::new(home) {
        Ok(runtime) => runtime,
        Err(error) => {
            destroy_workspace_locked(home, backend, name, false).with_context(|| {
                format!("runtime initialization failed ({error:#}); workspace rollback failed")
            })?;
            return Err(error);
        }
    };
    let attempt_path = state_dir(home, name).join("runtime-attempt");
    if let Err(error) = write_runtime_attempt(&attempt_path, &provider_name) {
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::AlreadyExists)
        {
            return Err(error.context("existing runtime attempt reservation retained"));
        }
        destroy_workspace_locked(home, backend, name, false).with_context(|| {
            format!("runtime attempt reservation failed ({error:#}); workspace rollback failed")
        })?;
        return Err(error);
    }
    match runtime.find(&provider_name).await {
        Ok(None) => {}
        Ok(Some(_)) => {
            bail!("runtime create attempt token unexpectedly already exists; workspace retained")
        }
        Err(error) => {
            return Err(
                error.context("could not preflight runtime create attempt; workspace retained")
            )
        }
    }
    let mut runtime_spec = RuntimeSpec::new(image.clone())
        .with_workspace(workspace_path.clone())
        .with_name(provider_name.clone());
    if let Some(mount) = jj_mount {
        runtime_spec = runtime_spec.with_jj_store(mount);
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
    metadata.runtime = Some(RuntimeMetadata { identity, image });
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
    fs::remove_file(attempt_path).context("could not clear completed runtime create attempt")?;
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
fn write_runtime_attempt(path: &Path, provider_name: &str) -> Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).with_context(|| {
        format!(
            "could not exclusively reserve runtime attempt {}",
            path.display()
        )
    })?;
    if let Err(error) = file
        .write_all(provider_name.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error.into());
    }
    let parent_path = path
        .parent()
        .context("runtime attempt path has no parent")?;
    fs::File::open(parent_path)?.sync_all()?;
    Ok(())
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
    let metadata = read_metadata(&state_dir(home, name))
        .with_context(|| format!("workspace not found: {name}"))?;
    let runtime_metadata = metadata
        .runtime
        .ok_or_else(|| anyhow::anyhow!("workspace has no runtime: {name}"))?;
    let runtime = BoxLiteRuntimeBackend::new(home)?;
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
    let metadata = read_metadata(&state_dir(home, name))?;
    let runtime_metadata = metadata
        .runtime
        .ok_or_else(|| anyhow::anyhow!("workspace has no runtime: {name}"))?;
    let runtime = BoxLiteRuntimeBackend::new(home)?;
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
    let metadata = read_metadata(&state_dir(home, name))
        .with_context(|| format!("workspace not found: {name}"))?;
    let Some(runtime_metadata) = metadata.runtime.clone() else {
        return Ok((metadata, None));
    };
    let info = BoxLiteRuntimeBackend::new(home)?
        .inspect(&runtime_metadata.identity)
        .await?;
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
    let state_dir = state_dir(home, name);
    let mut metadata = read_metadata(&state_dir)?;
    let runtime_metadata = metadata
        .runtime
        .clone()
        .ok_or_else(|| anyhow::anyhow!("workspace has no runtime: {name}"))?;
    let runtime = BoxLiteRuntimeBackend::new(home)?;
    destroy_runtime_locked(
        backend,
        &runtime,
        &mut metadata,
        RuntimeRemoval {
            home,
            name,
            keep_jj_workspace,
            runtime: &runtime_metadata,
        },
        write_metadata,
    )
    .await
}

#[cfg(feature = "microvm-boxlite")]
struct RuntimeRemoval<'a> {
    home: &'a Path,
    name: &'a str,
    keep_jj_workspace: bool,
    runtime: &'a RuntimeMetadata,
}

#[cfg(feature = "microvm-boxlite")]
async fn destroy_runtime_locked<B, R, W>(
    backend: &B,
    runtime: &R,
    metadata: &mut Metadata,
    removal: RuntimeRemoval<'_>,
    write: W,
) -> Result<()>
where
    B: CowBackend,
    R: crate::runtime::RuntimeBackend,
    W: Fn(&Path, &Metadata) -> Result<()>,
{
    let state_dir = state_dir(removal.home, removal.name);
    if runtime
        .contains(&removal.runtime.identity)
        .await
        .context("could not authoritatively reconcile runtime identity before removal")?
    {
        runtime.stop(&removal.runtime.identity).await?;
        runtime.remove(&removal.runtime.identity).await?;
    }
    metadata.runtime = None;
    write(&state_dir, metadata).context(
        "runtime was removed; metadata publication failed, so removal can be retried safely",
    )?;
    destroy_workspace_locked(
        removal.home,
        backend,
        removal.name,
        removal.keep_jj_workspace,
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
    let state_root = state_root(home);
    if !state_root.exists() {
        return Ok(Vec::new());
    }

    let mut metadata = Vec::new();
    for entry in fs::read_dir(state_root)? {
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
    #[cfg(feature = "microvm-boxlite")]
    use super::{create_workspace_with_runtime, destroy_workspace_with_runtime};
    use crate::{
        backend::SimpleCowBackend,
        home::{state_dir, workspace_dir},
        metadata::{
            metadata_path, read_metadata, write_metadata, JjMetadata, Metadata, RuntimeMetadata,
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
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("path separators"));
        assert!(!home.path().join("runtime").exists());
        assert!(!home.path().join("workspaces").exists());
    }

    #[cfg(all(feature = "microvm-boxlite", unix))]
    #[test]
    fn runtime_attempt_reservation_is_private_and_exclusive() {
        use std::os::unix::fs::PermissionsExt;

        let state = tempfile::tempdir().unwrap();
        let path = state.path().join("runtime-attempt");
        super::write_runtime_attempt(&path, "attempt-one").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(super::write_runtime_attempt(&path, "attempt-two").is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "attempt-one");
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
    async fn post_provider_persistence_failure_retries_without_touching_absent_provider() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = state_dir(home.path(), "demo");
        let workspace_path = workspace_dir(home.path(), "demo");
        fs::create_dir_all(&workspace_path).unwrap();
        let runtime_metadata = RuntimeMetadata {
            identity: RuntimeIdentity::new("provider"),
            image: "alpine:3.22".to_owned(),
        };
        let mut metadata = Metadata::new("demo", home.path().to_owned(), workspace_path);
        metadata.runtime = Some(runtime_metadata.clone());
        write_metadata(&state_dir, &metadata).unwrap();
        let destroys = Arc::new(AtomicUsize::new(0));

        let error = super::destroy_runtime_locked(
            &DestroyCountingBackend(Arc::clone(&destroys)),
            &FailingRuntime {
                stop_fails: false,
                remove_fails: false,
                find_identity: Some(runtime_metadata.identity.clone()),
                find_fails: false,
            },
            &mut metadata,
            super::RuntimeRemoval {
                home: home.path(),
                name: "demo",
                keep_jj_workspace: false,
                runtime: &runtime_metadata,
            },
            |_, _| anyhow::bail!("injected metadata failure"),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("metadata publication failed"));
        assert_eq!(destroys.load(Ordering::SeqCst), 0);

        super::destroy_runtime_locked(
            &DestroyCountingBackend(Arc::clone(&destroys)),
            &FailingRuntime {
                stop_fails: true,
                remove_fails: true,
                find_identity: None,
                find_fails: false,
            },
            &mut metadata,
            super::RuntimeRemoval {
                home: home.path(),
                name: "demo",
                keep_jj_workspace: false,
                runtime: &runtime_metadata,
            },
            write_metadata,
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
        };
        let workspace_path = workspace_dir(home.path(), "demo");
        fs::create_dir_all(&workspace_path).unwrap();
        let mut metadata = Metadata::new("demo", home.path().to_owned(), workspace_path);
        metadata.runtime = Some(runtime_metadata.clone());
        let destroys = Arc::new(AtomicUsize::new(0));
        let error = super::destroy_runtime_locked(
            &DestroyCountingBackend(Arc::clone(&destroys)),
            &FailingRuntime {
                stop_fails: true,
                remove_fails: true,
                find_identity: None,
                find_fails: true,
            },
            &mut metadata,
            super::RuntimeRemoval {
                home: home.path(),
                name: "demo",
                keep_jj_workspace: false,
                runtime: &runtime_metadata,
            },
            write_metadata,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("authoritatively reconcile"));
        assert_eq!(destroys.load(Ordering::SeqCst), 0);
        assert!(metadata.runtime.is_some());
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
