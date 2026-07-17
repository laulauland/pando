use pando::{
    backend::SimpleCowBackend,
    home::{state_dir, workspace_dir},
    lifecycle::{create_workspace, destroy_workspace, list_workspaces},
    metadata::read_metadata,
};
use serde_json::Value;
use std::{fs, path::Path, process::Command};

#[test]
fn pando_and_pd_help_use_invoked_binary_names() {
    let pando_help = Command::new(env!("CARGO_BIN_EXE_pando"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(pando_help.status.success());
    let pando_stdout = String::from_utf8(pando_help.stdout).unwrap();
    assert!(pando_stdout.contains("Usage: pando"));

    let pd_help = Command::new(env!("CARGO_BIN_EXE_pd"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(pd_help.status.success());
    let pd_stdout = String::from_utf8(pd_help.stdout).unwrap();
    assert!(pd_stdout.contains("Usage: pd"));
    assert!(!pd_stdout.contains("Usage: pando"));
}

#[test]
fn pando_and_pd_version_use_invoked_binary_names() {
    for (binary, name) in [
        (env!("CARGO_BIN_EXE_pando"), "pando"),
        (env!("CARGO_BIN_EXE_pd"), "pd"),
    ] {
        let output = Command::new(binary).arg("--version").output().unwrap();

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{name} {}\n", env!("CARGO_PKG_VERSION"))
        );
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn pando_and_pd_completions_use_invoked_binary_names() {
    let pando = Command::new(env!("CARGO_BIN_EXE_pando"))
        .args(["completions", "bash"])
        .output()
        .unwrap();
    assert!(pando.status.success());
    let pando_stdout = String::from_utf8(pando.stdout).unwrap();
    assert!(pando_stdout.contains("_pando"));

    let pd = Command::new(env!("CARGO_BIN_EXE_pd"))
        .args(["completions", "bash"])
        .output()
        .unwrap();
    assert!(pd.status.success());
    let pd_stdout = String::from_utf8(pd.stdout).unwrap();
    assert!(pd_stdout.contains("_pd"));
    assert!(!pd_stdout.contains("_pando"));
}

#[test]
fn cli_create_list_remove_lifecycle_is_end_to_end() {
    let source = tempfile::tempdir().unwrap();
    fs::create_dir(source.path().join("nested")).unwrap();
    fs::write(source.path().join("README.md"), "canonical").unwrap();
    fs::write(source.path().join("nested/file.txt"), "nested canonical").unwrap();

    let home = tempfile::tempdir().unwrap();
    let backend = SimpleCowBackend;

    let workspace = create_workspace(home.path(), &backend, "demo", source.path(), None).unwrap();
    let demo_state_dir = state_dir(home.path(), "demo");

    assert_eq!(workspace, workspace_dir(home.path(), "demo"));
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
        "remove should delete the state dir"
    );
    assert!(!workspace_dir(home.path(), "demo").exists());
    assert!(list_workspaces(home.path()).unwrap().is_empty());
}

#[test]
fn cli_info_prints_workspace_facts_and_pd_get_alias_matches() {
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("README.md"), "canonical").unwrap();
    let home = tempfile::tempdir().unwrap();
    let backend = SimpleCowBackend;

    let workspace = create_workspace(home.path(), &backend, "plain", source.path(), None).unwrap();
    let plain_state_dir = state_dir(home.path(), "plain");

    for (binary, args) in [
        (env!("CARGO_BIN_EXE_pando"), ["info", "plain"]),
        (env!("CARGO_BIN_EXE_pd"), ["get", "plain"]),
    ] {
        let output = Command::new(binary)
            .args(args)
            .env("PANDO_HOME", home.path())
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "{binary} {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let table = String::from_utf8(output.stdout).unwrap();
        assert!(table.contains("FIELD"));
        assert!(table.contains("VALUE"));
        assert!(table.contains("plain"));
        assert!(table.contains(workspace.to_string_lossy().as_ref()));
        assert!(!table.trim_start().starts_with('{'));
    }

    let json_output = Command::new(env!("CARGO_BIN_EXE_pd"))
        .args(["get", "plain", "--json"])
        .env("PANDO_HOME", home.path())
        .output()
        .unwrap();

    assert!(
        json_output.status.success(),
        "pd get --json failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&json_output.stdout),
        String::from_utf8_lossy(&json_output.stderr)
    );

    let info: Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert_eq!(info["name"], "plain");
    assert_eq!(
        info["state_dir"],
        plain_state_dir.to_string_lossy().as_ref()
    );
    assert_eq!(info["workspace_path"], workspace.to_string_lossy().as_ref());
    assert_eq!(
        info["canonical_root"],
        source
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert!(info["created_at"].is_string());
    assert!(info.get("jj").is_none());

    let cd = Command::new(env!("CARGO_BIN_EXE_pd"))
        .args(["cd", "plain", "--print"])
        .env("PANDO_HOME", home.path())
        .output()
        .unwrap();

    assert!(
        cd.status.success(),
        "pd cd --print failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&cd.stdout),
        String::from_utf8_lossy(&cd.stderr)
    );
    assert_eq!(
        String::from_utf8(cd.stdout).unwrap(),
        format!("{}\n", workspace.display())
    );
}

#[test]
fn cli_info_missing_workspace_is_clear_nonzero_error() {
    let home = tempfile::tempdir().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_pando"))
        .args(["info", "missing"])
        .env("PANDO_HOME", home.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("workspace not found: missing"));
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
    assert!(!workspace_dir(home.path(), "alpha").exists());
    assert!(state_dir(home.path(), "beta").exists());
    assert!(workspace_dir(home.path(), "beta").exists());
    assert_eq!(
        fs::read_to_string(beta.join("shared.txt")).unwrap(),
        "beta edit"
    );
}
