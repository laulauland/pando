use pando::{
    backend::SimpleCowBackend,
    home::state_dir,
    lifecycle::{create_workspace, destroy_workspace},
    metadata::read_metadata,
};
use std::{fs, path::Path, process::Command};

fn jj_available() -> bool {
    Command::new("jj")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[test]
fn cli_create_registers_native_jj_workspace_and_clean_status() {
    if !jj_available() {
        eprintln!("skipping jj registration integration test: jj binary not found");
        return;
    }

    let source = tempfile::tempdir().unwrap();
    init_jj_repo_with_base_and_empty_wc(source.path());
    let canonical_parent = jj_stdout(
        source.path(),
        &["log", "--no-graph", "-r", "@-", "-T", "commit_id"],
    );
    let home = tempfile::tempdir().unwrap();

    let create = Command::new(env!("CARGO_BIN_EXE_pando"))
        .args(["create", "foo"])
        .env("PANDO_HOME", home.path())
        .current_dir(source.path())
        .output()
        .unwrap();
    assert_success("pando create", &create);
    let workspace = Path::new(std::str::from_utf8(&create.stdout).unwrap().trim()).to_path_buf();

    let metadata = read_metadata(&state_dir(home.path(), "foo")).unwrap();
    let jj_metadata = metadata.jj.expect("jj metadata should be written");
    assert_eq!(jj_metadata.workspace_name.as_deref(), Some("pando-foo"));
    assert!(jj_metadata.base_commit.is_some());
    assert!(workspace.join(".jj").is_dir());

    assert_workspace_list_contains(source.path(), "pando-foo", true);
    assert_eq!(
        jj_stdout(
            &workspace,
            &["log", "--no-graph", "-r", "@-", "-T", "commit_id"]
        ),
        canonical_parent,
        "pando workspace @ should be based on canonical @- by default"
    );
    assert_clean_status(&workspace);

    fs::write(workspace.join("file.txt"), "workspace edit\n").unwrap();
    let dirty_status = jj_stdout(&workspace, &["st"]);
    assert!(
        dirty_status.contains("Working copy changes") && dirty_status.contains("file.txt"),
        "editing files in pando workspace should be visible to jj st:\n{dirty_status}"
    );
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

#[test]
fn canonical_uncommitted_changes_are_copied_but_base_remains_parent() {
    if !jj_available() {
        eprintln!("skipping jj dirty canonical integration test: jj binary not found");
        return;
    }

    let source = tempfile::tempdir().unwrap();
    init_jj_repo_with_base_and_empty_wc(source.path());
    fs::write(source.path().join("file.txt"), "canonical dirty edit\n").unwrap();
    let canonical_parent = jj_stdout(
        source.path(),
        &["log", "--no-graph", "-r", "@-", "-T", "commit_id"],
    );
    let home = tempfile::tempdir().unwrap();

    let workspace = create_workspace(home.path(), &SimpleCowBackend, "foo", source.path()).unwrap();

    assert_eq!(
        jj_stdout(
            &workspace,
            &["log", "--no-graph", "-r", "@-", "-T", "commit_id"]
        ),
        canonical_parent,
        "dirty canonical workspaces still base pando @ on canonical @-"
    );
    let status = jj_stdout(&workspace, &["st"]);
    assert!(
        status.contains("Working copy changes") && status.contains("file.txt"),
        "documented limitation: SimpleCowBackend copies canonical uncommitted file contents, so jj st is not clean:\n{status}"
    );
}

#[test]
#[ignore = "TODO(PANDO-kgnuhuxw): --from currently accepts a filesystem path only, not a jj revset"]
fn create_from_revset_bases_workspace_at_requested_revision() {
    // Once `pando create --from <REVSET>` is implemented for jj repos, assert
    // that the created workspace's `@-` equals the requested revision.
}

fn init_jj_repo(path: &Path) {
    let init = Command::new("jj")
        .args(["git", "init", "--no-colocate"])
        .arg(path)
        .output()
        .unwrap();
    assert_success("jj git init", &init);
}

fn init_jj_repo_with_base_and_empty_wc(path: &Path) {
    init_jj_repo(path);
    fs::write(path.join("file.txt"), "base\n").unwrap();
    jj_success(path, &["file", "track", "file.txt"]);
    jj_success(path, &["describe", "-m", "base"]);
    jj_success(path, &["new"]);
}

fn assert_workspace_list_contains(repo: &Path, workspace: &str, expected: bool) {
    let stdout = jj_stdout(repo, &["workspace", "list"]);
    assert_eq!(
        stdout.contains(workspace),
        expected,
        "workspace list expectation for {workspace}={expected} failed:\n{stdout}"
    );
}

fn assert_clean_status(repo: &Path) {
    let stdout = jj_stdout(repo, &["st"]);
    assert!(
        stdout.contains("The working copy has no changes."),
        "expected clean jj status in {}, got:\n{stdout}",
        repo.display()
    );
}

fn jj_success(repo: &Path, args: &[&str]) {
    let output = Command::new("jj")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert_success(&format!("jj {}", args.join(" ")), &output);
}

fn jj_stdout(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("jj")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert_success(&format!("jj {}", args.join(" ")), &output);
    String::from_utf8(output.stdout).unwrap()
}

fn assert_success(command: &str, output: &std::process::Output) {
    assert!(
        output.status.success(),
        "{command} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
