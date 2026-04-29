use pando::{
    backend::SimpleCowBackend,
    home::state_dir,
    lifecycle::{create_workspace, destroy_workspace},
    metadata::read_metadata,
};
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
    assert_eq!(jj_metadata.workspace_name.as_deref(), Some("pando-foo"));
    assert!(jj_metadata.base_commit.is_some());
    assert!(workspace.join(".jj").is_dir());

    assert_workspace_list_contains(source.path(), "pando-foo", true);
}

#[test]
fn destroy_forgets_native_jj_workspace_by_default() {
    if !jj_available() {
        eprintln!("skipping jj destroy integration test: jj binary not found");
        return;
    }

    let source = tempfile::tempdir().unwrap();
    init_jj_repo(source.path());
    let home = tempfile::tempdir().unwrap();

    create_workspace(home.path(), &SimpleCowBackend, "foo", source.path()).unwrap();
    assert_workspace_list_contains(source.path(), "pando-foo", true);

    destroy_workspace(home.path(), &SimpleCowBackend, "foo", false).unwrap();

    assert!(!state_dir(home.path(), "foo").exists());
    assert_workspace_list_contains(source.path(), "pando-foo", false);
}

#[test]
fn destroy_keep_jj_workspace_preserves_native_jj_workspace() {
    if !jj_available() {
        eprintln!("skipping jj destroy keep integration test: jj binary not found");
        return;
    }

    let source = tempfile::tempdir().unwrap();
    init_jj_repo(source.path());
    let home = tempfile::tempdir().unwrap();

    create_workspace(home.path(), &SimpleCowBackend, "foo", source.path()).unwrap();
    destroy_workspace(home.path(), &SimpleCowBackend, "foo", true).unwrap();

    assert!(!state_dir(home.path(), "foo").exists());
    assert_workspace_list_contains(source.path(), "pando-foo", true);
}

fn init_jj_repo(path: &std::path::Path) {
    let init = Command::new("jj")
        .args(["git", "init", "--no-colocate"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "jj git init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );
}

fn assert_workspace_list_contains(repo: &std::path::Path, workspace: &str, expected: bool) {
    let list = Command::new("jj")
        .args(["workspace", "list", "-R"])
        .arg(repo)
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "jj workspace list failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert_eq!(
        stdout.contains(workspace),
        expected,
        "workspace list expectation for {workspace}={expected} failed:\n{stdout}"
    );
}
