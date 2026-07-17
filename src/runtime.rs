use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::future::Future;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSpec {
    pub image: String,
}

impl RuntimeSpec {
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    fn start(&self, identity: &RuntimeIdentity) -> impl Future<Output = Result<()>> + Send;
    fn inspect(
        &self,
        identity: &RuntimeIdentity,
    ) -> impl Future<Output = Result<RuntimeInfo>> + Send;
    fn stop(&self, identity: &RuntimeIdentity) -> impl Future<Output = Result<()>> + Send;
    fn remove(&self, identity: &RuntimeIdentity) -> impl Future<Output = Result<()>> + Send;
}

#[cfg(feature = "microvm-boxlite")]
mod boxlite_backend {
    use super::{RuntimeBackend, RuntimeIdentity, RuntimeInfo, RuntimeSpec, RuntimeStatus};
    use crate::home::boxlite_runtime_home;
    use anyhow::{anyhow, Context, Result};
    use boxlite::{BoxOptions, BoxStatus, BoxliteOptions, BoxliteRuntime, NetworkSpec, RootfsSpec};
    use std::path::Path;

    const CPU_COUNT: u8 = 2;
    const MEMORY_MIB: u32 = 512;

    pub struct BoxLiteRuntimeBackend {
        runtime: BoxliteRuntime,
    }

    impl BoxLiteRuntimeBackend {
        pub fn new(pando_home: &Path) -> Result<Self> {
            let options = BoxliteOptions {
                home_dir: boxlite_runtime_home(pando_home),
                ..BoxliteOptions::default()
            };
            let runtime = BoxliteRuntime::new(options).context("could not initialize BoxLite")?;
            Ok(Self { runtime })
        }

        async fn get(&self, identity: &RuntimeIdentity) -> Result<boxlite::LiteBox> {
            self.runtime
                .get(identity.as_str())
                .await
                .context("could not query BoxLite runtime")?
                .ok_or_else(|| anyhow!("runtime not found: {}", identity.as_str()))
        }
    }

    impl RuntimeBackend for BoxLiteRuntimeBackend {
        async fn create(&self, spec: RuntimeSpec) -> Result<RuntimeIdentity> {
            let mut advanced = boxlite::AdvancedBoxOptions::default();
            // BoxLite 0.9.7's bundled filter kills libkrun with SIGSYS on gondor's
            // Linux 7.1 kernel. Keep the rest of its jailer enabled while the
            // syscall profile is qualified in stage 5.
            advanced.security.seccomp_enabled = false;
            let options = BoxOptions {
                cpus: Some(CPU_COUNT),
                memory_mib: Some(MEMORY_MIB),
                rootfs: RootfsSpec::Image(spec.image),
                network: NetworkSpec::Disabled,
                auto_remove: false,
                detach: true,
                advanced,
                ..BoxOptions::default()
            };
            let litebox = self
                .runtime
                .create(options, None)
                .await
                .context("could not create BoxLite runtime")?;
            Ok(RuntimeIdentity::new(litebox.id().to_string()))
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
            self.get(identity)
                .await?
                .stop()
                .await
                .context("could not stop BoxLite runtime")
        }

        async fn remove(&self, identity: &RuntimeIdentity) -> Result<()> {
            self.runtime
                .remove(identity.as_str(), false)
                .await
                .context("could not remove BoxLite runtime")
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
