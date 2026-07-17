#![cfg(feature = "microvm-boxlite")]

use pando::runtime::{BoxLiteRuntimeBackend, RuntimeBackend, RuntimeSpec, RuntimeStatus};

#[test]
#[ignore = "requires KVM on Linux or Hypervisor.framework on macOS and network access"]
fn alpine_box_survives_stop_and_restart_before_removal() {
    let pando_home = tempfile::tempdir().unwrap();
    let async_runtime = tokio::runtime::Runtime::new().unwrap();

    async_runtime.block_on(async {
        let backend = BoxLiteRuntimeBackend::new(pando_home.path()).unwrap();
        let identity = backend
            .create(RuntimeSpec::new("alpine:3.22"))
            .await
            .unwrap();

        backend.start(&identity).await.unwrap();
        let running = backend.inspect(&identity).await.unwrap();
        eprintln!(
            "created {} from {}: {:?}, {} CPUs, {} MiB",
            running.identity.as_str(),
            running.image,
            running.status,
            running.cpu_count,
            running.memory_mib
        );
        assert_eq!(running.status, RuntimeStatus::Running);
        assert_eq!(running.image, "alpine:3.22");
        assert_eq!(running.cpu_count, 2);
        assert_eq!(running.memory_mib, 512);

        backend.stop(&identity).await.unwrap();
        assert_eq!(
            backend.inspect(&identity).await.unwrap().status,
            RuntimeStatus::Stopped
        );
        eprintln!("stopped {}", identity.as_str());

        backend.start(&identity).await.unwrap();
        assert_eq!(
            backend.inspect(&identity).await.unwrap().status,
            RuntimeStatus::Running
        );
        eprintln!("restarted {}", identity.as_str());
        backend.stop(&identity).await.unwrap();
        backend.remove(&identity).await.unwrap();
        assert!(backend.inspect(&identity).await.is_err());
        eprintln!("removed {}", identity.as_str());
    });
}
