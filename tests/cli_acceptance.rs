use pando::{
    backend::SimpleCowBackend,
    home::state_dir,
    lifecycle::{create_workspace, destroy_workspace, list_workspaces},
    metadata::read_metadata,
};
use std::{fs, path::Path, process::Command};

#[test]
fn cli_create_list_destroy_lifecycle_is_end_to_end() {
    let source = tempfile::tempdir().unwrap();
    fs::create_dir(source.path().join("nested")).unwrap();
    fs::write(source.path().join("README.md"), "canonical").unwrap();
    fs::write(source.path().join("nested/file.txt"), "nested canonical").unwrap();

    let home = tempfile::tempdir().unwrap();
    let backend = SimpleCowBackend;

    let workspace = create_workspace(home.path(), &backend, "demo", source.path(), None).unwrap();
    let demo_state_dir = state_dir(home.path(), "demo");

    assert_eq!(workspace, demo_state_dir.join("workspace"));
    assert_eq!(
        fs::read_to_string(workspace.join("README.md")).unwrap(),
        "canonical"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("nested/file.txt")).unwrap(),
        "nested canonical"
    );

    fs::write(workspace.join("README.md"), "workspace edit").unwrap();
    assert_eq!(
        fs::read_to_string(source.path().join("README.md")).unwrap(),
        "canonical",
        "workspace edits must not modify the canonical source tree"
    );

    let listed = list_workspaces(home.path()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "demo");
    assert_eq!(listed[0].workspace_path, workspace);
    assert_eq!(
        listed[0].canonical_root,
        source.path().canonicalize().unwrap()
    );

    destroy_workspace(home.path(), &backend, "demo", false).unwrap();
    assert!(
        !demo_state_dir.exists(),
        "destroy should remove the state dir"
    );
    assert!(list_workspaces(home.path()).unwrap().is_empty());
}

#[test]
fn cli_create_ignores_from_revset_outside_jj_and_uses_current_dir() {
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("README.md"), "canonical").unwrap();
    let home = tempfile::tempdir().unwrap();

    let create = Command::new(env!("CARGO_BIN_EXE_pando"))
        .args(["create", "plain", "--from", "not-a-real-revset"])
        .env("PANDO_HOME", home.path())
        .current_dir(source.path())
        .output()
        .unwrap();

    assert!(
        create.status.success(),
        "pando create failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    let workspace = Path::new(std::str::from_utf8(&create.stdout).unwrap().trim()).to_path_buf();
    assert_eq!(
        fs::read_to_string(workspace.join("README.md")).unwrap(),
        "canonical"
    );

    let metadata = read_metadata(&state_dir(home.path(), "plain")).unwrap();
    assert_eq!(
        metadata.canonical_root,
        source.path().canonicalize().unwrap()
    );
    assert!(metadata.jj.is_none());
}

#[test]
fn cli_two_named_workspaces_do_not_interfere() {
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("shared.txt"), "canonical").unwrap();

    let home = tempfile::tempdir().unwrap();
    let backend = SimpleCowBackend;

    let alpha = create_workspace(home.path(), &backend, "alpha", source.path(), None).unwrap();
    let beta = create_workspace(home.path(), &backend, "beta", source.path(), None).unwrap();

    assert_ne!(alpha, beta);
    assert_eq!(
        fs::read_to_string(alpha.join("shared.txt")).unwrap(),
        "canonical"
    );
    assert_eq!(
        fs::read_to_string(beta.join("shared.txt")).unwrap(),
        "canonical"
    );

    fs::write(alpha.join("shared.txt"), "alpha edit").unwrap();
    fs::write(beta.join("shared.txt"), "beta edit").unwrap();

    assert_eq!(
        fs::read_to_string(alpha.join("shared.txt")).unwrap(),
        "alpha edit"
    );
    assert_eq!(
        fs::read_to_string(beta.join("shared.txt")).unwrap(),
        "beta edit"
    );
    assert_eq!(
        fs::read_to_string(source.path().join("shared.txt")).unwrap(),
        "canonical"
    );

    let names: Vec<_> = list_workspaces(home.path())
        .unwrap()
        .into_iter()
        .map(|metadata| metadata.name)
        .collect();
    assert_eq!(names, vec!["alpha", "beta"]);

    destroy_workspace(home.path(), &backend, "alpha", false).unwrap();
    assert!(!state_dir(home.path(), "alpha").exists());
    assert!(state_dir(home.path(), "beta").exists());
    assert_eq!(
        fs::read_to_string(beta.join("shared.txt")).unwrap(),
        "beta edit"
    );
}
