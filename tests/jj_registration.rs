use pando::{backend::SimpleCowBackend, home::state_dir, lifecycle::create_workspace, metadata::read_metadata};
use std::process::Command;

fn jj_available() -> bool {
    Command::new("jj")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[test]
fn create_registers_native_jj_workspace_when_source_is_jj_repo() {
    if !jj_available() {
        eprintln!("skipping jj registration integration test: jj binary not found");
        return;
    }

    let source = tempfile::tempdir().unwrap();
    let init = Command::new("jj")
        .args(["git", "init", "--no-colocate"])
        .arg(source.path())
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "jj git init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );

    let home = tempfile::tempdir().unwrap();
    let workspace = create_workspace(home.path(), &SimpleCowBackend, "foo", source.path()).unwrap();

    let metadata = read_metadata(&state_dir(home.path(), "foo")).unwrap();
    let jj_metadata = metadata.jj.expect("jj metadata should be written");
    assert_eq!(jj_metadata.workspace_id.as_deref(), Some("pando-foo"));
    assert!(workspace.join(".jj").is_dir());

    let list = Command::new("jj")
        .args(["workspace", "list", "-R"])
        .arg(source.path())
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "jj workspace list failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("pando-foo"),
        "workspace list did not contain pando-foo:\n{stdout}"
    );
}
