#[cfg(all(feature = "microvm-boxlite", target_os = "linux"))]
use anyhow::Context;
use anyhow::Result;
use serde::{Deserialize, Serialize};
#[cfg(all(feature = "microvm-boxlite", target_os = "linux"))]
use std::path::Path;
use std::{future::Future, path::PathBuf};

pub const GUEST_WORKSPACE_PATH: &str = "/workspace";
#[cfg(feature = "microvm-boxlite")]
const GUEST_JJ_STAGE_PATH: &str = "/workspace/.jj/pando-tools-stage/jj";
pub const DEFAULT_CPU_COUNT: u8 = 2;
pub const DEFAULT_MEMORY_MIB: u32 = 512;

pub fn validate_runtime_platform() -> Result<()> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        validate_linux_kvm_at(std::path::Path::new("/dev/kvm"), |file| {
            use std::os::fd::AsRawFd;
            // KVM_GET_API_VERSION is an argument-free ioctl from linux/kvm.h.
            let version = unsafe { libc::ioctl(file.as_raw_fd(), 0xAE00) };
            if version < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(version)
            }
        })
    }
    #[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
    anyhow::bail!("BoxLite runtimes are currently qualified only on Linux x86_64 with KVM");
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("BoxLite runtimes are supported only on Linux x86_64/KVM");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn validate_linux_kvm_at(
    path: &std::path::Path,
    get_api_version: impl FnOnce(&std::fs::File) -> std::io::Result<i32>,
) -> Result<()> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| anyhow::anyhow!(
            "BoxLite requires readable and writable /dev/kvm access on Linux x86_64 ({error}); enable hardware virtualization and grant this user KVM access"
        ))?;
    let version = get_api_version(&file).map_err(|error| anyhow::anyhow!(
        "BoxLite could not query KVM_GET_API_VERSION on /dev/kvm ({error}); verify that hardware virtualization and the KVM kernel modules are available"
    ))?;
    if version != 12 {
        anyhow::bail!(
            "BoxLite requires KVM API version 12, but /dev/kvm reported {version}; update or enable a compatible KVM kernel module"
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeNetworkPolicy {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeSeccompPolicy {
    Required,
    AllowUnqualifiedProvider,
    /// Metadata written before Pando recorded the provider's seccomp posture.
    LegacyUnqualifiedProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimePolicy {
    pub cpu_count: u8,
    pub memory_mib: u32,
    pub network: RuntimeNetworkPolicy,
    pub seccomp: RuntimeSeccompPolicy,
}

impl Default for RuntimePolicy {
    fn default() -> Self {
        Self {
            cpu_count: DEFAULT_CPU_COUNT,
            memory_mib: DEFAULT_MEMORY_MIB,
            network: RuntimeNetworkPolicy::Enabled,
            seccomp: RuntimeSeccompPolicy::Required,
        }
    }
}

impl RuntimePolicy {
    pub fn validate(&self) -> Result<()> {
        if !(1..=64).contains(&self.cpu_count) {
            anyhow::bail!("runtime CPU count must be between 1 and 64");
        }
        if !(128..=262_144).contains(&self.memory_mib) {
            anyhow::bail!("runtime memory must be between 128 and 262144 MiB");
        }
        #[cfg(target_os = "linux")]
        if self.seccomp == RuntimeSeccompPolicy::Required {
            anyhow::bail!("BoxLite 0.9.7 seccomp is incompatible with Pando's qualified Linux/libkrun path; refusing to weaken the sandbox (pass --allow-unqualified-seccomp only for an explicitly accepted provider risk)");
        }
        if self.seccomp == RuntimeSeccompPolicy::LegacyUnqualifiedProvider {
            anyhow::bail!("legacy runtime security posture cannot be selected for a new runtime");
        }
        #[cfg(not(target_os = "linux"))]
        if self.seccomp == RuntimeSeccompPolicy::AllowUnqualifiedProvider {
            anyhow::bail!("--allow-unqualified-seccomp is only valid on Linux");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeIdentity(String);

impl RuntimeIdentity {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub struct RuntimeSpec {
    image: String,
    workspace_path: Option<PathBuf>,
    name: Option<String>,
    policy: RuntimePolicy,
    #[cfg(feature = "microvm-boxlite")]
    jj_store: Option<crate::jj::JjRuntimeMount>,
    #[cfg(feature = "microvm-boxlite")]
    guest_jj_stage: Option<PathBuf>,
}

impl RuntimeSpec {
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            workspace_path: None,
            name: None,
            policy: RuntimePolicy::default(),
            #[cfg(feature = "microvm-boxlite")]
            jj_store: None,
            #[cfg(feature = "microvm-boxlite")]
            guest_jj_stage: None,
        }
    }

    pub fn with_workspace(mut self, workspace_path: PathBuf) -> Self {
        self.workspace_path = Some(workspace_path);
        self
    }

    pub fn with_name(mut self, name: String) -> Self {
        self.name = Some(name);
        self
    }

    pub fn with_policy(mut self, policy: RuntimePolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn policy(&self) -> RuntimePolicy {
        self.policy
    }

    pub fn image(&self) -> &str {
        &self.image
    }

    #[cfg(feature = "microvm-boxlite")]
    pub(crate) fn with_jj_store(mut self, store: crate::jj::JjRuntimeMount) -> Self {
        debug_assert!(self.workspace_path.is_some());
        self.jj_store = Some(store);
        self
    }

    #[cfg(feature = "microvm-boxlite")]
    pub(crate) fn with_guest_jj_stage(mut self, path: PathBuf) -> Self {
        debug_assert!(self.workspace_path.is_some());
        self.guest_jj_stage = Some(path);
        self
    }
}

#[cfg(all(feature = "microvm-boxlite", target_os = "linux"))]
pub(crate) fn prepare_guest_jj_stage(workspace_root: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let source = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join("jj"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| anyhow::anyhow!("jj is required on PATH for a jj-backed Linux runtime"))?
        .canonicalize()
        .context("could not canonicalize host jj executable")?;
    let metadata = source
        .metadata()
        .context("could not inspect host jj executable")?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        anyhow::bail!(
            "host jj path is not an executable regular file: {}",
            source.display()
        );
    }

    stage_guest_jj(&source, workspace_root)
}

#[cfg(all(feature = "microvm-boxlite", target_os = "linux"))]
fn stage_guest_jj(source: &Path, workspace_root: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let stage_dir = workspace_root.join(".jj/pando-tools-stage");
    std::fs::create_dir_all(&stage_dir)
        .context("could not create Pando guest jj staging directory")?;
    let destination = stage_dir.join("jj");
    let temporary = stage_dir.join(format!("jj.tmp-{}", std::process::id()));
    std::fs::copy(source, &temporary).context("could not copy jj into Pando guest tools")?;
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o555))
        .context("could not seal Pando guest jj permissions")?;
    std::fs::rename(&temporary, &destination)
        .context("could not publish Pando guest jj executable")?;
    Ok(stage_dir)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCommand {
    pub arguments: Vec<String>,
    pub terminal: bool,
}

impl RuntimeCommand {
    pub fn new(arguments: Vec<String>) -> Self {
        Self {
            arguments,
            terminal: false,
        }
    }

    pub fn terminal(arguments: Vec<String>) -> Self {
        Self {
            arguments,
            terminal: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeStatus {
    Configured,
    Running,
    Stopping,
    Stopped,
    Paused,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub identity: RuntimeIdentity,
    pub image: String,
    pub status: RuntimeStatus,
    pub cpu_count: u8,
    pub memory_mib: u32,
}

pub trait RuntimeBackend {
    fn create(&self, spec: RuntimeSpec) -> impl Future<Output = Result<RuntimeIdentity>> + Send;
    fn find(&self, name: &str) -> impl Future<Output = Result<Option<RuntimeIdentity>>> + Send;
    fn contains(&self, identity: &RuntimeIdentity) -> impl Future<Output = Result<bool>> + Send;
    fn start(&self, identity: &RuntimeIdentity) -> impl Future<Output = Result<()>> + Send;
    fn inspect(
        &self,
        identity: &RuntimeIdentity,
    ) -> impl Future<Output = Result<RuntimeInfo>> + Send;
    fn stop(&self, identity: &RuntimeIdentity) -> impl Future<Output = Result<()>> + Send;
    fn remove(&self, identity: &RuntimeIdentity) -> impl Future<Output = Result<()>> + Send;
    fn execute(
        &self,
        identity: &RuntimeIdentity,
        command: RuntimeCommand,
    ) -> impl Future<Output = Result<i32>> + Send;
}

#[cfg(feature = "microvm-boxlite")]
mod boxlite_backend {
    use super::{
        RuntimeBackend, RuntimeCommand, RuntimeIdentity, RuntimeInfo, RuntimeNetworkPolicy,
        RuntimeSpec, RuntimeStatus, GUEST_JJ_STAGE_PATH, GUEST_WORKSPACE_PATH,
    };
    use crate::home::boxlite_runtime_home;
    use anyhow::{anyhow, bail, Context, Result};
    use boxlite::{
        runtime::options::VolumeSpec, BoxCommand, BoxOptions, BoxStatus, BoxliteOptions,
        BoxliteRuntime, NetworkSpec, RootfsSpec,
    };
    use futures::StreamExt;
    #[cfg(unix)]
    use std::os::fd::AsRawFd;
    use std::{
        io::Write,
        path::{Path, PathBuf},
    };

    pub struct BoxLiteRuntimeBackend {
        runtime: BoxliteRuntime,
        runtime_home: PathBuf,
        #[cfg(target_os = "linux")]
        bwrap_executable: PathBuf,
    }

    impl BoxLiteRuntimeBackend {
        pub fn new(pando_home: &Path) -> Result<Self> {
            let runtime_home = absolute_path(&boxlite_runtime_home(pando_home))?;
            std::fs::create_dir_all(&runtime_home)
                .context("could not create BoxLite runtime home")?;
            let runtime_home = runtime_home
                .canonicalize()
                .context("could not canonicalize BoxLite runtime home")?;
            let options = BoxliteOptions {
                home_dir: runtime_home.clone(),
                ..BoxliteOptions::default()
            };
            let runtime = BoxliteRuntime::new(options).context("could not initialize BoxLite")?;
            Ok(Self {
                runtime,
                runtime_home,
                #[cfg(target_os = "linux")]
                bwrap_executable: resolve_executable("bwrap")?,
            })
        }

        async fn get(&self, identity: &RuntimeIdentity) -> Result<boxlite::LiteBox> {
            self.runtime
                .get(identity.as_str())
                .await
                .context("could not query BoxLite runtime")?
                .ok_or_else(|| anyhow!("runtime not found: {}", identity.as_str()))
        }
    }

    fn absolute_path(path: &Path) -> Result<PathBuf> {
        if path.is_absolute() {
            Ok(path.to_owned())
        } else {
            Ok(std::env::current_dir()
                .context("could not resolve relative Pando home")?
                .join(path))
        }
    }

    #[cfg(target_os = "linux")]
    fn resolve_executable(name: &str) -> Result<PathBuf> {
        let path = std::env::var_os("PATH").ok_or_else(|| anyhow!("PATH is not set"))?;
        std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| anyhow!("required BoxLite executable not found: {name}"))?
            .canonicalize()
            .context("could not canonicalize BoxLite executable")
    }

    impl RuntimeBackend for BoxLiteRuntimeBackend {
        async fn create(&self, spec: RuntimeSpec) -> Result<RuntimeIdentity> {
            spec.policy.validate()?;
            if spec.jj_store.is_some() && spec.workspace_path.is_none() {
                bail!("invalid runtime topology: jj store requires /workspace");
            }
            if spec.guest_jj_stage.is_some() && spec.workspace_path.is_none() {
                bail!("invalid runtime topology: guest jj staging requires /workspace");
            }
            let mut advanced = boxlite::AdvancedBoxOptions::default();
            // BoxLite 0.9.7's bundled filter kills libkrun with SIGSYS on gondor's
            // Linux 7.1 kernel. Keep the rest of its jailer enabled while the
            // syscall profile is qualified in stage 5.
            #[cfg(target_os = "linux")]
            {
                advanced.security.seccomp_enabled = false;
            }
            let mut volumes: Vec<VolumeSpec> = spec
                .workspace_path
                .map(|path| -> Result<VolumeSpec> {
                    Ok(VolumeSpec {
                        host_path: path
                            .canonicalize()
                            .context("could not canonicalize runtime workspace")?
                            .to_string_lossy()
                            .into_owned(),
                        guest_path: GUEST_WORKSPACE_PATH.to_owned(),
                        read_only: false,
                    })
                })
                .transpose()?
                .into_iter()
                .collect();
            if let Some(store) = spec.jj_store.as_ref() {
                store.revalidate()?;
                for (host_path, guest_path) in store.volumes() {
                    volumes.push(VolumeSpec {
                        host_path: host_path
                            .canonicalize()
                            .context("could not canonicalize runtime volume")?
                            .to_string_lossy()
                            .into_owned(),
                        guest_path: guest_path.to_string_lossy().into_owned(),
                        read_only: false,
                    });
                }
            }
            let options = BoxOptions {
                cpus: Some(spec.policy.cpu_count),
                memory_mib: Some(spec.policy.memory_mib),
                rootfs: RootfsSpec::Image(spec.image),
                network: match spec.policy.network {
                    RuntimeNetworkPolicy::Enabled => NetworkSpec::Enabled {
                        allow_net: Vec::new(),
                    },
                    RuntimeNetworkPolicy::Disabled => NetworkSpec::Disabled,
                },
                auto_remove: false,
                detach: true,
                advanced,
                volumes,
                working_dir: Some(GUEST_WORKSPACE_PATH.to_owned()),
                ..BoxOptions::default()
            };
            let litebox = self
                .runtime
                .create(options, spec.name)
                .await
                .context("could not create BoxLite runtime")?;
            if spec.guest_jj_stage.is_some() {
                let install_script = format!(
                    "mkdir -p /usr/local/bin && cp {GUEST_JJ_STAGE_PATH} /usr/local/bin/jj.tmp && chmod 0555 /usr/local/bin/jj.tmp && mv /usr/local/bin/jj.tmp /usr/local/bin/jj"
                );
                let install = litebox
                    .exec(BoxCommand::new("sh").args(["-c", &install_script]))
                    .await
                    .context("could not start guest jj installation")?;
                let result = install
                    .wait()
                    .await
                    .context("could not wait for guest jj installation")?;
                if result.exit_code != 0 {
                    let cleanup = self.runtime.remove(litebox.id().as_ref(), false).await;
                    return Err(match cleanup {
                        Ok(()) => anyhow!(
                            "guest jj installation failed with exit code {}",
                            result.exit_code
                        ),
                        Err(cleanup) => anyhow!(
                            "guest jj installation failed with exit code {} and provider cleanup failed: {cleanup}",
                            result.exit_code
                        ),
                    });
                }
                if let Some(stage) = spec.guest_jj_stage.as_ref() {
                    std::fs::remove_dir_all(stage)
                        .context("could not remove Pando guest jj staging directory")?;
                }
            }
            if let Some(store) = spec.jj_store.as_ref() {
                if let Err(error) = store.revalidate() {
                    let cleanup = self.runtime.remove(litebox.id().as_ref(), false).await;
                    return Err(match cleanup {
                        Ok(()) => error.context(
                            "jj store identity changed during provider handoff; provider removed",
                        ),
                        Err(cleanup) => error.context(format!(
                            "jj store identity changed during provider handoff and provider cleanup failed: {cleanup}"
                        )),
                    });
                }
            }
            Ok(RuntimeIdentity::new(litebox.id().to_string()))
        }

        async fn find(&self, name: &str) -> Result<Option<RuntimeIdentity>> {
            Ok(self
                .runtime
                .get(name)
                .await
                .context("could not reconcile BoxLite runtime by name")?
                .map(|runtime| RuntimeIdentity::new(runtime.id().to_string())))
        }

        async fn contains(&self, identity: &RuntimeIdentity) -> Result<bool> {
            let info = self
                .runtime
                .get_info(identity.as_str())
                .await
                .context("could not query BoxLite runtime identity")?;
            Ok(info.is_some_and(|info| info.id.to_string() == identity.as_str()))
        }

        async fn start(&self, identity: &RuntimeIdentity) -> Result<()> {
            self.get(identity)
                .await?
                .start()
                .await
                .context("could not start BoxLite runtime")
        }

        async fn inspect(&self, identity: &RuntimeIdentity) -> Result<RuntimeInfo> {
            let info = self
                .runtime
                .get_info(identity.as_str())
                .await
                .context("could not inspect BoxLite runtime")?
                .ok_or_else(|| anyhow!("runtime not found: {}", identity.as_str()))?;
            Ok(RuntimeInfo {
                identity: RuntimeIdentity::new(info.id.to_string()),
                image: info.image,
                status: runtime_status(info.status),
                cpu_count: info.cpus,
                memory_mib: info.memory_mib,
            })
        }

        async fn stop(&self, identity: &RuntimeIdentity) -> Result<()> {
            let status = self.inspect(identity).await?.status;
            let owned_processes = if status == RuntimeStatus::Running {
                discover_runtime_processes(
                    &self.runtime_home,
                    identity,
                    #[cfg(target_os = "linux")]
                    &self.bwrap_executable,
                )?
            } else {
                Vec::new()
            };
            self.get(identity)
                .await?
                .stop()
                .await
                .context("could not stop BoxLite runtime")?;
            ensure_runtime_processes_stopped(
                owned_processes,
                identity,
                #[cfg(target_os = "linux")]
                &self.runtime_home,
                #[cfg(target_os = "linux")]
                &self.bwrap_executable,
            )
            .await
        }

        async fn remove(&self, identity: &RuntimeIdentity) -> Result<()> {
            if self.inspect(identity).await?.status == RuntimeStatus::Running {
                bail!("BoxLite runtime must be stopped before removal");
            }
            self.runtime
                .remove(identity.as_str(), false)
                .await
                .context("could not remove BoxLite runtime")
        }

        async fn execute(
            &self,
            identity: &RuntimeIdentity,
            command: RuntimeCommand,
        ) -> Result<i32> {
            let (program, arguments) = command
                .arguments
                .split_first()
                .ok_or_else(|| anyhow!("runtime command must not be empty"))?;
            let litebox = self.get(identity).await?;
            let mut execution = litebox
                .exec(
                    BoxCommand::new(program)
                        .args(arguments.iter().cloned())
                        .env(
                            "PATH",
                            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                        )
                        .working_dir(GUEST_WORKSPACE_PATH)
                        .tty(command.terminal),
                )
                .await
                .context("could not execute command in BoxLite runtime")?;

            let terminal = command.terminal;
            let (stream_errors, mut stream_error_rx) = tokio::sync::mpsc::unbounded_channel();
            let stdin_task = execution.stdin().map(|mut input| {
                let errors = stream_errors.clone();
                tokio::spawn(async move {
                    let result = async {
                        let stdin = match async_stdin() {
                            Ok(stdin) => stdin,
                            Err(error) if !terminal && is_epoll_unsupported(&error) => {
                                let Some(file) = duplicate_nonpollable_stdin().context(
                                    "stdin is not pollable and is neither a regular file nor /dev/null",
                                )? else {
                                    input.close();
                                    return Ok::<(), anyhow::Error>(());
                                };
                                let (mut chunks, reader) = spawn_blocking_reader(file);
                                while let Some(chunk) = chunks.recv().await {
                                    input
                                        .write_all(&chunk)
                                        .await
                                        .context("could not stream blocking stdin to runtime")?;
                                }
                                reader
                                    .await
                                    .context("blocking stdin reader task failed")??;
                                input.close();
                                return Ok::<(), anyhow::Error>(());
                            }
                            Err(error) => return Err(error),
                        };
                        let mut buffer = [0_u8; 8192];
                        loop {
                            let length = read_stdin(&stdin, &mut buffer).await?;
                            if length == 0 {
                                input.close();
                                return Ok::<(), anyhow::Error>(());
                            }
                            input
                                .write_all(&buffer[..length])
                                .await
                                .context("could not stream stdin to runtime")?;
                        }
                    }
                    .await;
                    if let Err(error) = &result {
                        let _ = errors.send(anyhow!("stdin forwarding failed: {error:#}"));
                    }
                    result
                })
            });
            let stdout_task = execution.stdout().map(|mut output| {
                let errors = stream_errors.clone();
                tokio::spawn(async move {
                    let result = async {
                        while let Some(chunk) = output.next().await {
                            std::io::stdout().write_all(chunk.as_bytes())?;
                            std::io::stdout().flush()?;
                        }
                        Ok::<(), anyhow::Error>(())
                    }
                    .await;
                    if let Err(error) = &result {
                        let _ = errors.send(anyhow!("stdout forwarding failed: {error:#}"));
                    }
                    result
                })
            });
            let stderr_task = execution.stderr().map(|mut output| {
                let errors = stream_errors.clone();
                tokio::spawn(async move {
                    let result = async {
                        while let Some(chunk) = output.next().await {
                            std::io::stderr().write_all(chunk.as_bytes())?;
                            std::io::stderr().flush()?;
                        }
                        Ok::<(), anyhow::Error>(())
                    }
                    .await;
                    if let Err(error) = &result {
                        let _ = errors.send(anyhow!("stderr forwarding failed: {error:#}"));
                    }
                    result
                })
            });

            let terminal_guard = match terminal.then(TerminalGuard::enter).transpose() {
                Ok(guard) => guard,
                Err(error) => {
                    let _ = execution.signal(libc::SIGKILL).await;
                    abort_io_tasks(stdin_task, stdout_task, stderr_task).await;
                    return Err(error.context("could not enter terminal mode"));
                }
            };
            if terminal {
                if let Err(error) = resize_terminal(&execution).await {
                    let _ = execution.signal(libc::SIGKILL).await;
                    drop(terminal_guard);
                    abort_io_tasks(stdin_task, stdout_task, stderr_task).await;
                    return Err(error);
                }
            }
            let signal_task = if terminal {
                match spawn_signal_forwarder(execution.clone(), stream_errors.clone()) {
                    Ok(task) => Some(task),
                    Err(error) => {
                        let _ = execution.signal(libc::SIGKILL).await;
                        drop(terminal_guard);
                        abort_io_tasks(stdin_task, stdout_task, stderr_task).await;
                        return Err(error);
                    }
                }
            } else {
                None
            };
            drop(stream_errors);
            let kill_execution = execution.clone();
            let result = match wait_or_forwarding_error(
                async { execution.wait().await.map_err(anyhow::Error::from) },
                &mut stream_error_rx,
                move || async move {
                    kill_execution
                        .signal(libc::SIGKILL)
                        .await
                        .map_err(anyhow::Error::from)
                },
            )
            .await
            {
                Ok(result) => result,
                Err(error) => {
                    drop(terminal_guard);
                    abort_runtime_tasks(signal_task, stdin_task, stdout_task, stderr_task).await;
                    return Err(error);
                }
            };
            drop(terminal_guard);
            abort_and_join(signal_task).await;
            abort_and_join(stdin_task).await;
            let stdout_result = join_io_task(stdout_task, "stdout").await;
            let stderr_result = join_io_task(stderr_task, "stderr").await;
            stdout_result?;
            stderr_result?;
            Ok(result.code())
        }
    }

    async fn abort_io_tasks(
        stdin: Option<tokio::task::JoinHandle<Result<()>>>,
        stdout: Option<tokio::task::JoinHandle<Result<()>>>,
        stderr: Option<tokio::task::JoinHandle<Result<()>>>,
    ) {
        for task in [stdin, stdout, stderr].into_iter().flatten() {
            task.abort();
            let _ = task.await;
        }
    }

    async fn abort_runtime_tasks(
        signal: Option<tokio::task::JoinHandle<Result<()>>>,
        stdin: Option<tokio::task::JoinHandle<Result<()>>>,
        stdout: Option<tokio::task::JoinHandle<Result<()>>>,
        stderr: Option<tokio::task::JoinHandle<Result<()>>>,
    ) {
        abort_and_join(signal).await;
        abort_io_tasks(stdin, stdout, stderr).await;
    }

    async fn abort_and_join(task: Option<tokio::task::JoinHandle<Result<()>>>) {
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }

    async fn wait_or_forwarding_error<T, W, K, KF>(
        wait: W,
        errors: &mut tokio::sync::mpsc::UnboundedReceiver<anyhow::Error>,
        kill: K,
    ) -> Result<T>
    where
        W: std::future::Future<Output = Result<T>>,
        K: FnOnce() -> KF,
        KF: std::future::Future<Output = Result<()>>,
    {
        tokio::pin!(wait);
        tokio::select! {
            result = &mut wait => result,
            Some(error) = errors.recv() => {
                match kill().await {
                    Ok(()) => Err(error),
                    Err(kill_error) => Err(error.context(format!(
                        "also could not kill failed runtime command: {kill_error:#}"
                    ))),
                }
            }
        }
    }

    async fn join_io_task(
        task: Option<tokio::task::JoinHandle<Result<()>>>,
        stream: &str,
    ) -> Result<()> {
        if let Some(task) = task {
            task.await
                .with_context(|| format!("{stream} task failed"))??;
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn ensure_runtime_processes_stopped(
        processes: Vec<OwnedProcess>,
        identity: &RuntimeIdentity,
        runtime_home: &Path,
        bwrap_executable: &Path,
    ) -> Result<()> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut stable_empty_scans = 0;
        loop {
            let mut alive = Vec::new();
            for process in &processes {
                if process.is_alive()? {
                    alive.push(process);
                }
            }
            if alive.is_empty() {
                let late = associated_runtime_processes(runtime_home, identity, bwrap_executable)?;
                if late.is_empty() {
                    stable_empty_scans += 1;
                    if stable_empty_scans == 2 {
                        return Ok(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
                bail!("BoxLite runtime {} created associated processes after ownership snapshot: {late:?}", identity.as_str());
            }
            if tokio::time::Instant::now() >= deadline {
                for process in &alive {
                    process.signal(libc::SIGKILL)?;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let mut survivors = Vec::new();
                for process in &processes {
                    if process.is_alive()? {
                        survivors.push(process.pid);
                    }
                }
                if survivors.is_empty() {
                    stable_empty_scans = 0;
                    continue;
                }
                return Err(anyhow!(
                    "BoxLite runtime {} left owned processes after stop: {:?}",
                    identity.as_str(),
                    survivors
                ));
            }
            for process in &alive {
                process.signal(libc::SIGTERM)?;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn ensure_runtime_processes_stopped(
        _processes: Vec<OwnedProcess>,
        _identity: &RuntimeIdentity,
    ) -> Result<()> {
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[derive(Debug)]
    struct OwnedProcess {
        pid: i32,
        pidfd: std::os::fd::OwnedFd,
    }

    #[cfg(target_os = "linux")]
    impl OwnedProcess {
        fn is_alive(&self) -> Result<bool> {
            let mut descriptor = libc::pollfd {
                fd: std::os::fd::AsRawFd::as_raw_fd(&self.pidfd),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: descriptor points to one valid pidfd poll descriptor.
            loop {
                let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
                if result >= 0 {
                    return Ok(result == 0);
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error.into());
                }
            }
        }

        fn signal(&self, signal: i32) -> Result<()> {
            // SAFETY: pidfd is a stable kernel reference to the process proven at discovery.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    std::os::fd::AsRawFd::as_raw_fd(&self.pidfd),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if result == 0 {
                Ok(())
            } else {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(())
                } else {
                    Err(error.into())
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[derive(Debug)]
    struct OwnedProcess;

    #[cfg(target_os = "linux")]
    struct ProcessRecord {
        pid: i32,
        parent_pid: i32,
        start_time: u64,
        arguments: Vec<Vec<u8>>,
        executable: PathBuf,
    }

    #[cfg(target_os = "linux")]
    fn associated_runtime_processes(
        runtime_home: &Path,
        identity: &RuntimeIdentity,
        bwrap_executable: &Path,
    ) -> Result<Vec<i32>> {
        let box_home = runtime_home.join("boxes").join(identity.as_str());
        let mut associated = Vec::new();
        for entry in std::fs::read_dir("/proc").context("could not reconcile Linux processes")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse().ok())
            else {
                continue;
            };
            let command_line = match std::fs::read(entry.path().join("cmdline")) {
                Ok(value) => value,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    if proc_entry_is_same_user(&entry.path())? {
                        bail!("could not reconcile unreadable same-user process {pid}");
                    }
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let arguments = nul_arguments(&command_line);
            if !has_boxlite_box_association(&arguments, &box_home) {
                continue;
            }
            let executable = std::fs::read_link(entry.path().join("exe"))
                .with_context(|| format!("could not verify associated BoxLite process {pid}"))?;
            if !has_expected_executable_path(&executable, &arguments, &box_home, bwrap_executable) {
                bail!("associated process {pid} has an untrusted executable");
            }
            associated.push(pid);
        }
        Ok(associated)
    }

    #[cfg(target_os = "linux")]
    fn discover_runtime_processes(
        runtime_home: &Path,
        identity: &RuntimeIdentity,
        bwrap_executable: &Path,
    ) -> Result<Vec<OwnedProcess>> {
        let box_home = runtime_home.join("boxes").join(identity.as_str());
        let pid_file = box_home.join("shim.pid");
        let pid_record = match std::fs::read_to_string(&pid_file) {
            Ok(record) => record,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!(
                    "running BoxLite runtime has no ownership pid file: {}",
                    pid_file.display()
                )
            }
            Err(error) => return Err(error.into()),
        };
        let mut pid_fields = pid_record.lines();
        let root_pid: i32 = pid_fields
            .next()
            .and_then(|field| field.parse().ok())
            .ok_or_else(|| anyhow!("invalid BoxLite pid file: {}", pid_file.display()))?;
        let recorded_start: u64 = pid_fields
            .next()
            .and_then(|field| field.parse().ok())
            .ok_or_else(|| anyhow!("BoxLite pid file lacks process fingerprint"))?;

        let mut records = Vec::new();
        for entry in std::fs::read_dir("/proc").context("could not inspect Linux processes")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse().ok())
            else {
                continue;
            };
            let command_line = match std::fs::read(entry.path().join("cmdline")) {
                Ok(command_line) => command_line,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    if proc_entry_is_same_user(&entry.path())? {
                        bail!("could not inspect same-user process {pid} during BoxLite ownership proof");
                    }
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let Some((parent_pid, start_time)) = process_identity(&entry.path().join("stat"))?
            else {
                continue;
            };
            let executable = match std::fs::read_link(entry.path().join("exe")) {
                Ok(executable) => executable,
                Err(_) => continue,
            };
            records.push(ProcessRecord {
                pid,
                parent_pid,
                start_time,
                arguments: nul_arguments(&command_line)
                    .into_iter()
                    .map(<[u8]>::to_vec)
                    .collect(),
                executable,
            });
        }
        let root = records
            .iter()
            .find(|record| record.pid == root_pid && record.start_time == recorded_start)
            .ok_or_else(|| anyhow!("BoxLite pid fingerprint does not identify a live process"))?;
        let root_arguments: Vec<_> = root.arguments.iter().map(Vec::as_slice).collect();
        if !has_boxlite_box_association(&root_arguments, &box_home)
            || !has_expected_executable_path(
                &root.executable,
                &root_arguments,
                &box_home,
                bwrap_executable,
            )
        {
            bail!("BoxLite pid file points to a process without canonical provider identity");
        }

        let tree = process_tree_pids(root_pid)?;
        let mut processes = Vec::new();
        for pid in tree {
            let record = records
                .iter()
                .find(|record| record.pid == pid)
                .ok_or_else(|| anyhow!("could not inspect proven BoxLite descendant {pid}"))?;
            if !expected_tree_executable(&record.executable, &box_home, bwrap_executable) {
                bail!(
                    "unexpected executable in BoxLite process tree: {}",
                    record.executable.display()
                );
            }
            processes.push(open_owned_process(record)?);
        }
        Ok(processes)
    }

    #[cfg(target_os = "linux")]
    fn proc_entry_is_same_user(path: &Path) -> Result<bool> {
        use std::os::unix::fs::MetadataExt;
        Ok(std::fs::metadata(path)?.uid() == unsafe { libc::geteuid() })
    }

    #[cfg(target_os = "linux")]
    fn process_tree_pids(root: i32) -> Result<Vec<i32>> {
        let mut tree = vec![root];
        let mut index = 0;
        while index < tree.len() {
            let pid = tree[index];
            let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
                .with_context(|| {
                    format!("could not enumerate children of BoxLite process {pid}")
                })?;
            for child in children.split_whitespace() {
                let child = child
                    .parse()
                    .with_context(|| format!("invalid child PID for BoxLite process {pid}"))?;
                if !tree.contains(&child) {
                    tree.push(child);
                }
            }
            index += 1;
        }
        Ok(tree)
    }

    #[cfg(target_os = "linux")]
    fn open_owned_process(record: &ProcessRecord) -> Result<OwnedProcess> {
        let pidfd = open_pidfd(record.pid)?;
        let current = process_identity(&PathBuf::from(format!("/proc/{}/stat", record.pid)))?;
        if current != Some((record.parent_pid, record.start_time)) {
            bail!("BoxLite process changed identity during ownership proof");
        }
        let executable = std::fs::read_link(format!("/proc/{}/exe", record.pid))
            .context("BoxLite process exited during ownership proof")?;
        if executable != record.executable {
            bail!("BoxLite process changed executable during ownership proof");
        }
        Ok(OwnedProcess {
            pid: record.pid,
            pidfd,
        })
    }

    #[cfg(target_os = "linux")]
    fn nul_arguments(command_line: &[u8]) -> Vec<&[u8]> {
        command_line
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .collect()
    }

    #[cfg(not(target_os = "linux"))]
    fn discover_runtime_processes(
        _runtime_home: &Path,
        _identity: &RuntimeIdentity,
    ) -> Result<Vec<OwnedProcess>> {
        Ok(Vec::new())
    }

    #[cfg(target_os = "linux")]
    fn has_boxlite_box_association(arguments: &[&[u8]], box_home: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;

        let Some(executable) = arguments.first() else {
            return false;
        };
        let shim = box_home.join("bin/boxlite-shim");
        if *executable == shim.as_os_str().as_bytes() {
            return true;
        }
        if !(executable.ends_with(b"/bwrap") || *executable == b"bwrap") {
            return false;
        }
        let shared = box_home.join("shared");
        let shared = shared.as_os_str().as_bytes();
        if arguments.len() < 7
            || arguments[1..6]
                != [
                    b"--unshare-user".as_slice(),
                    b"--unshare-pid".as_slice(),
                    b"--unshare-ipc".as_slice(),
                    b"--unshare-uts".as_slice(),
                    b"--new-session".as_slice(),
                ]
            || !arguments.ends_with(&[b"--".as_slice(), shim.as_os_str().as_bytes()])
        {
            return false;
        }
        let mut index = 6;
        let mut shared_bindings = 0;
        while index < arguments.len() - 2 {
            let arity = match arguments[index] {
                b"--ro-bind" | b"--bind" | b"--dev-bind" => 2,
                b"--setenv" => 2,
                b"--dev" | b"--proc" | b"--tmpfs" | b"--chdir" => 1,
                b"--clearenv" => 0,
                _ => return false,
            };
            if index + arity >= arguments.len() - 1 {
                return false;
            }
            if arguments[index] == b"--bind" && arguments[index + 1] == shared {
                if arguments[index + 2] != shared {
                    return false;
                }
                shared_bindings += 1;
            }
            index += arity + 1;
        }
        index == arguments.len() - 2 && shared_bindings == 1
    }

    #[cfg(target_os = "linux")]
    fn has_expected_executable_path(
        executable: &Path,
        arguments: &[&[u8]],
        box_home: &Path,
        bwrap_executable: &Path,
    ) -> bool {
        use std::os::unix::ffi::OsStrExt;

        let Some(argument_zero) = arguments.first() else {
            return false;
        };
        if argument_zero.ends_with(b"/bwrap") || *argument_zero == b"bwrap" {
            return executable == bwrap_executable;
        }
        let expected_shim = box_home.join("bin/boxlite-shim");
        executable.as_os_str().as_bytes() == expected_shim.as_os_str().as_bytes()
    }

    #[cfg(target_os = "linux")]
    fn expected_tree_executable(
        executable: &Path,
        box_home: &Path,
        bwrap_executable: &Path,
    ) -> bool {
        executable == bwrap_executable || executable == box_home.join("bin/boxlite-shim")
    }

    #[cfg(target_os = "linux")]
    fn process_identity(stat_path: &Path) -> Result<Option<(i32, u64)>> {
        let stat = match std::fs::read_to_string(stat_path) {
            Ok(stat) => stat,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let Some(fields) = stat.rsplit_once(')').map(|(_, fields)| fields) else {
            return Ok(None);
        };
        let fields: Vec<_> = fields.split_whitespace().collect();
        let Some(parent_pid) = fields.get(1).and_then(|field| field.parse().ok()) else {
            return Ok(None);
        };
        let Some(start_time) = fields.get(19).and_then(|field| field.parse().ok()) else {
            return Ok(None);
        };
        Ok(Some((parent_pid, start_time)))
    }

    #[cfg(target_os = "linux")]
    fn open_pidfd(pid: i32) -> Result<std::os::fd::OwnedFd> {
        use std::os::fd::FromRawFd;

        // SAFETY: pidfd_open receives a validated positive process id and zero flags.
        let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error())
                .context("pidfd unavailable; refusing uncertain BoxLite cleanup");
        }
        // SAFETY: descriptor is a newly returned owned pidfd.
        Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(descriptor) })
    }

    #[cfg(all(test, target_os = "linux"))]
    mod process_ownership_tests {
        use super::{
            has_boxlite_box_association, has_expected_executable_path, open_pidfd, OwnedProcess,
        };
        use std::{path::Path, process::Command};

        #[test]
        fn requires_exact_known_boxlite_argument_structure() {
            let box_home = Path::new("/tmp/pando/boxes/abc");
            let shim = b"/tmp/pando/boxes/abc/bin/boxlite-shim".as_slice();
            let shared = b"/tmp/pando/boxes/abc/shared".as_slice();
            let valid: &[&[u8]] = &[
                b"bwrap",
                b"--unshare-user",
                b"--unshare-pid",
                b"--unshare-ipc",
                b"--unshare-uts",
                b"--new-session",
                b"--bind",
                shared,
                shared,
                b"--",
                shim,
            ];
            assert!(has_boxlite_box_association(&[shim], box_home));
            assert!(has_boxlite_box_association(valid, box_home));
            assert!(!has_boxlite_box_association(
                &[
                    b"bwrap",
                    b"--unshare-user",
                    b"--unshare-pid",
                    b"--unshare-ipc",
                    b"--unshare-uts",
                    b"--new-session",
                    b"--unshare-all",
                    b"--",
                    b"junk",
                    b"--bind",
                    shared,
                    shared,
                    b"--",
                    shim,
                ],
                box_home
            ));
            assert!(!has_boxlite_box_association(
                &valid[..valid.len() - 2],
                box_home
            ));
            let mut conflicting = valid.to_vec();
            conflicting.splice(
                conflicting.len() - 2..conflicting.len() - 2,
                [b"--bind".as_slice(), shared, b"/tmp/conflict".as_slice()],
            );
            assert!(!has_boxlite_box_association(&conflicting, box_home));
            assert!(!has_boxlite_box_association(
                &[b"tool", b"--note", shim],
                box_home
            ));
            assert!(!has_expected_executable_path(
                Path::new("/tmp/bwrap"),
                valid,
                box_home,
                Path::new("/usr/bin/bwrap"),
            ));
        }

        #[test]
        fn rejects_sibling_box_prefix() {
            assert!(!has_boxlite_box_association(
                &[b"/tmp/pando/boxes/abc-extra/bin/boxlite-shim"],
                Path::new("/tmp/pando/boxes/abc")
            ));
        }

        #[test]
        fn pidfd_cannot_signal_a_replacement_after_target_exit() {
            let mut target = Command::new("sleep").arg("30").spawn().unwrap();
            let owned = OwnedProcess {
                pid: target.id() as i32,
                pidfd: open_pidfd(target.id() as i32).unwrap(),
            };
            target.kill().unwrap();
            target.wait().unwrap();

            let mut unrelated = Command::new("sleep").arg("30").spawn().unwrap();
            owned.signal(libc::SIGKILL).unwrap();
            assert!(unrelated.try_wait().unwrap().is_none());
            unrelated.kill().unwrap();
            unrelated.wait().unwrap();
        }
    }

    #[cfg(unix)]
    struct NonblockingStdin {
        descriptor: std::os::fd::OwnedFd,
        original_flags: libc::c_int,
    }

    #[cfg(unix)]
    impl std::os::fd::AsRawFd for NonblockingStdin {
        fn as_raw_fd(&self) -> std::os::fd::RawFd {
            std::os::fd::AsRawFd::as_raw_fd(&self.descriptor)
        }
    }

    #[cfg(unix)]
    impl Drop for NonblockingStdin {
        fn drop(&mut self) {
            // SAFETY: descriptor remains valid until after Drop returns.
            unsafe {
                libc::fcntl(self.as_raw_fd(), libc::F_SETFL, self.original_flags);
            }
        }
    }

    #[cfg(unix)]
    fn async_stdin() -> Result<tokio::io::unix::AsyncFd<NonblockingStdin>> {
        use std::os::fd::FromRawFd;

        // SAFETY: STDIN_FILENO is borrowed; dup returns a new owned descriptor.
        let descriptor = unsafe { libc::dup(libc::STDIN_FILENO) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: descriptor was returned by dup and ownership transfers here.
        let descriptor = unsafe { std::os::fd::OwnedFd::from_raw_fd(descriptor) };
        // SAFETY: descriptor is valid for both fcntl calls.
        let original_flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
        if original_flags < 0
            || unsafe {
                libc::fcntl(
                    descriptor.as_raw_fd(),
                    libc::F_SETFL,
                    original_flags | libc::O_NONBLOCK,
                )
            } < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(tokio::io::unix::AsyncFd::new(NonblockingStdin {
            descriptor,
            original_flags,
        })?)
    }

    #[cfg(unix)]
    fn is_epoll_unsupported(error: &anyhow::Error) -> bool {
        error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.raw_os_error() == Some(libc::EPERM))
    }

    #[cfg(unix)]
    fn duplicate_nonpollable_stdin() -> Result<Option<std::fs::File>> {
        use std::os::{fd::FromRawFd, unix::fs::MetadataExt};

        // SAFETY: STDIN_FILENO is borrowed; dup returns a new owned descriptor.
        let descriptor = unsafe { libc::dup(libc::STDIN_FILENO) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: descriptor is valid and stat points to writable storage.
        if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } < 0 {
            // SAFETY: descriptor is owned here and has not been transferred.
            unsafe { libc::close(descriptor) };
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: successful fstat initialized stat.
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            let null = std::fs::metadata("/dev/null")?;
            if stat.st_dev == null.dev() as libc::dev_t && stat.st_ino == null.ino() as libc::ino_t
            {
                // SAFETY: descriptor is owned here and has not been transferred.
                unsafe { libc::close(descriptor) };
                return Ok(None);
            }
            // SAFETY: descriptor is owned here and has not been transferred.
            unsafe { libc::close(descriptor) };
            anyhow::bail!("unsupported non-regular stdin descriptor")
        }
        // SAFETY: descriptor is newly duplicated and ownership transfers to File.
        Ok(Some(unsafe { std::fs::File::from_raw_fd(descriptor) }))
    }

    #[cfg(unix)]
    type BlockingReader = (
        tokio::sync::mpsc::Receiver<Vec<u8>>,
        tokio::task::JoinHandle<Result<()>>,
    );

    #[cfg(unix)]
    fn spawn_blocking_reader(mut file: std::fs::File) -> BlockingReader {
        use std::io::Read;

        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let task = tokio::task::spawn_blocking(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                let length = file.read(&mut buffer)?;
                if length == 0 {
                    return Ok(());
                }
                if sender.blocking_send(buffer[..length].to_vec()).is_err() {
                    return Ok(());
                }
            }
        });
        (receiver, task)
    }

    #[cfg(unix)]
    async fn read_stdin(
        stdin: &tokio::io::unix::AsyncFd<NonblockingStdin>,
        buffer: &mut [u8],
    ) -> Result<usize> {
        loop {
            let mut ready = stdin.readable().await?;
            let result = ready.try_io(|descriptor| {
                // SAFETY: buffer is valid for writes and descriptor is valid while borrowed.
                let length = unsafe {
                    libc::read(
                        descriptor.get_ref().as_raw_fd(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                    )
                };
                if length < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(length as usize)
                }
            });
            match result {
                Ok(result) => return Ok(result?),
                Err(_) => continue,
            }
        }
    }

    #[cfg(unix)]
    fn spawn_signal_forwarder(
        execution: boxlite::Execution,
        errors: tokio::sync::mpsc::UnboundedSender<anyhow::Error>,
    ) -> Result<tokio::task::JoinHandle<Result<()>>> {
        use tokio::signal::unix::{signal, SignalKind};
        let mut interrupt = signal(SignalKind::interrupt()).context("could not register SIGINT")?;
        let mut terminate =
            signal(SignalKind::terminate()).context("could not register SIGTERM")?;
        let mut resize =
            signal(SignalKind::window_change()).context("could not register SIGWINCH")?;
        Ok(tokio::spawn(async move {
            loop {
                let result = tokio::select! {
                    _ = interrupt.recv() => execution.signal(libc::SIGINT).await.context("could not forward SIGINT"),
                    _ = terminate.recv() => execution.signal(libc::SIGTERM).await.context("could not forward SIGTERM"),
                    _ = resize.recv() => resize_terminal(&execution).await,
                };
                if let Err(error) = result {
                    let message = format!("terminal signal forwarding failed: {error:#}");
                    let _ = errors.send(anyhow!(message.clone()));
                    return Err(anyhow!(message));
                }
            }
        }))
    }

    #[cfg(not(unix))]
    fn spawn_signal_forwarder(
        _execution: boxlite::Execution,
        _errors: tokio::sync::mpsc::UnboundedSender<anyhow::Error>,
    ) -> Result<tokio::task::JoinHandle<Result<()>>> {
        Ok(tokio::spawn(std::future::pending()))
    }

    async fn resize_terminal(execution: &boxlite::Execution) -> Result<()> {
        #[cfg(unix)]
        {
            let mut size = libc::winsize {
                ws_row: 0,
                ws_col: 0,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // SAFETY: TIOCGWINSZ writes a winsize to the valid pointer supplied here.
            if unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut size) } == 0
                && size.ws_row > 0
                && size.ws_col > 0
            {
                execution
                    .resize_tty(u32::from(size.ws_row), u32::from(size.ws_col))
                    .await
                    .context("could not resize runtime terminal")?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    #[tokio::test]
    async fn stdin_forwarding_failure_kills_instead_of_waiting_forever() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };

        let (errors, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        errors
            .send(anyhow!("stdin forwarding failed: injected broken input"))
            .unwrap();
        let killed = Arc::new(AtomicBool::new(false));
        let killed_by_task = Arc::clone(&killed);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_or_forwarding_error(
                std::future::pending::<Result<()>>(),
                &mut receiver,
                move || async move {
                    killed_by_task.store(true, Ordering::SeqCst);
                    Ok(())
                },
            ),
        )
        .await
        .expect("forwarding failure remained blocked on guest wait")
        .unwrap_err();
        assert!(result.to_string().contains("stdin forwarding failed"));
        assert!(killed.load(Ordering::SeqCst));
    }

    #[cfg(all(test, unix))]
    #[tokio::test]
    async fn blocking_reader_preserves_regular_file_stdin_exactly() {
        use std::io::{Seek, Write};

        let expected = b"first line\nsecond line\0binary tail";
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(expected).unwrap();
        file.rewind().unwrap();
        let (mut chunks, reader) = spawn_blocking_reader(file);
        let mut actual = Vec::new();
        while let Some(chunk) = chunks.recv().await {
            actual.extend_from_slice(&chunk);
        }
        reader.await.unwrap().unwrap();
        assert_eq!(actual, expected);
    }

    struct TerminalGuard {
        #[cfg(unix)]
        original: libc::termios,
        #[cfg(unix)]
        active: bool,
    }

    impl TerminalGuard {
        fn enter() -> Result<Self> {
            #[cfg(unix)]
            {
                // SAFETY: termios is initialized by tcgetattr before it is read.
                let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
                // Non-terminal stdin is useful for automated PTY smoke tests.
                if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut original) } != 0 {
                    return Ok(Self {
                        original,
                        active: false,
                    });
                }
                let mut raw = original;
                // SAFETY: raw and STDIN_FILENO are valid for these termios operations.
                unsafe {
                    libc::cfmakeraw(&mut raw);
                    if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                        return Err(std::io::Error::last_os_error().into());
                    }
                }
                Ok(Self {
                    original,
                    active: true,
                })
            }
            #[cfg(not(unix))]
            Ok(Self {})
        }
    }

    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            #[cfg(unix)]
            if self.active {
                // SAFETY: original came from tcgetattr for STDIN_FILENO.
                unsafe {
                    let _ = libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
                }
            }
        }
    }

    fn runtime_status(status: BoxStatus) -> RuntimeStatus {
        match status {
            BoxStatus::Configured => RuntimeStatus::Configured,
            BoxStatus::Running => RuntimeStatus::Running,
            BoxStatus::Stopping => RuntimeStatus::Stopping,
            BoxStatus::Stopped => RuntimeStatus::Stopped,
            BoxStatus::Paused => RuntimeStatus::Paused,
            BoxStatus::Failed => RuntimeStatus::Failed,
            BoxStatus::Unknown => RuntimeStatus::Unknown,
        }
    }
}

#[cfg(feature = "microvm-boxlite")]
pub use boxlite_backend::BoxLiteRuntimeBackend;

#[cfg(test)]
mod policy_tests {
    use super::{RuntimeNetworkPolicy, RuntimePolicy, RuntimeSeccompPolicy};

    #[test]
    fn new_runtime_policy_enables_networking() {
        assert_eq!(
            RuntimePolicy::default().network,
            RuntimeNetworkPolicy::Enabled
        );
    }

    #[test]
    fn resource_limits_reject_out_of_range_values() {
        assert!(RuntimePolicy {
            cpu_count: 0,
            ..RuntimePolicy::default()
        }
        .validate()
        .is_err());
        assert!(RuntimePolicy {
            memory_mib: 127,
            ..RuntimePolicy::default()
        }
        .validate()
        .is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_requires_explicit_boxlite_seccomp_qualification_override() {
        assert!(RuntimePolicy::default().validate().is_err());
        assert!(RuntimePolicy {
            seccomp: RuntimeSeccompPolicy::AllowUnqualifiedProvider,
            ..RuntimePolicy::default()
        }
        .validate()
        .is_ok());
    }

    #[cfg(all(feature = "microvm-boxlite", target_os = "linux"))]
    #[test]
    fn guest_jj_staging_is_workspace_local_and_executable() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("host-jj");
        let workspace = fixture.path().join("workspace");
        std::fs::write(&source, b"guest jj").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(workspace.join(".jj")).unwrap();

        let stage = super::stage_guest_jj(&source, &workspace).unwrap();
        let staged_jj = stage.join("jj");
        assert_eq!(stage, workspace.join(".jj/pando-tools-stage"));
        assert_eq!(std::fs::read(&staged_jj).unwrap(), b"guest jj");
        assert_eq!(
            std::fs::metadata(staged_jj).unwrap().permissions().mode() & 0o777,
            0o555
        );
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn missing_kvm_has_an_actionable_platform_diagnostic() {
        let directory = tempfile::tempdir().unwrap();
        let error = super::validate_linux_kvm_at(&directory.path().join("missing-kvm"), |_| Ok(12))
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("/dev/kvm"));
        assert!(message.contains("hardware virtualization"));
        assert!(message.contains("KVM access"));
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn incompatible_kvm_api_has_an_actionable_diagnostic() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let error = super::validate_linux_kvm_at(file.path(), |_| Ok(11)).unwrap_err();
        assert!(error.to_string().contains("requires KVM API version 12"));
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn unavailable_hvf_has_an_actionable_diagnostic() {
        let error = super::validate_macos_hvf_with(|| Ok("0".to_owned())).unwrap_err();
        assert!(error.to_string().contains("Hypervisor.framework"));
        assert!(error.to_string().contains("kern.hv_support"));
    }
}
