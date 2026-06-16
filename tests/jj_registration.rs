use pando::{
    backend::SimpleCowBackend,
    home::state_dir,
    lifecycle::{create_workspace, destroy_workspace},
    metadata::read_metadata,
};
use std::{fs, path::Path, process::Command};

const TEST_USER_NAME: &str = "Pando Test";
const TEST_USER_EMAIL: &str = "pando@example.invalid";

fn skip_if_jj_unavailable(test_name: &str) -> bool {
    let available = Command::new("jj")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if !available {
        eprintln!("skipping {test_name}: jj binary not found");
    }

    !available
}

#[test]
fn cli_create_registers_native_jj_workspace_and_clean_diff() {
    if skip_if_jj_unavailable("jj registration integration test") {
        return;
    }

    let source = tempfile::tempdir().unwrap();
    init_jj_repo_with_base_and_empty_wc(source.path());
    let canonical_parent = jj_commit_id(source.path(), "@-");
    let home = tempfile::tempdir().unwrap();
    let config_home = tempfile::tempdir().unwrap();
    write_jj_user_config(
        config_home.path(),
        "Pando Config User",
        "pando-config@example.invalid",
    );

    let create = Command::new(env!("CARGO_BIN_EXE_pando"))
        .args(["create", "foo"])
        .env("PANDO_HOME", home.path())
        .env("XDG_CONFIG_HOME", config_home.path())
        .current_dir(source.path())
        .output()
        .unwrap();
    assert_success("pando create", &create);
    let workspace = Path::new(std::str::from_utf8(&create.stdout).unwrap().trim()).to_path_buf();

    let metadata = read_metadata(&state_dir(home.path(), "foo")).unwrap();
    let jj_metadata = metadata.jj.expect("jj metadata should be written");
    assert_eq!(jj_metadata.workspace_name.as_deref(), Some("pando-foo"));
    assert!(jj_metadata.base_commit.is_some());
    assert!(jj_metadata.base_revision.is_some());
    assert_eq!(
        jj_metadata.base_revision.as_deref(),
        Some(jj_template(source.path(), "@-", "change_id.shortest()").as_str())
    );
    assert!(workspace.join(".jj").is_dir());

    assert_workspace_list_contains(source.path(), "pando-foo", true);
    assert_eq!(
        jj_commit_id(&workspace, "@-"),
        canonical_parent,
        "pando workspace @ should be based on canonical @- by default"
    );
    assert_eq!(
        jj_template(&workspace, "@", "author.email()"),
        "pando-config@example.invalid",
        "pando-created workspace commit should use jj user config"
    );
    assert_clean_diff(&workspace);

    fs::write(workspace.join("file.txt"), "workspace edit\n").unwrap();
    assert_diff_summary_contains(&workspace, "file.txt");
}

#[test]
fn remove_forgets_native_jj_workspace_by_default() {
    if skip_if_jj_unavailable("jj remove integration test") {
        return;
    }

    let source = tempfile::tempdir().unwrap();
    init_jj_repo(source.path());
    let home = tempfile::tempdir().unwrap();

    create_workspace(home.path(), &SimpleCowBackend, "foo", source.path(), None).unwrap();
    assert_workspace_list_contains(source.path(), "pando-foo", true);

    destroy_workspace(home.path(), &SimpleCowBackend, "foo", false).unwrap();

    assert!(!state_dir(home.path(), "foo").exists());
    assert_workspace_list_contains(source.path(), "pando-foo", false);
}

#[test]
fn remove_keep_jj_workspace_preserves_native_jj_workspace() {
    if skip_if_jj_unavailable("jj remove keep integration test") {
        return;
    }

    let source = tempfile::tempdir().unwrap();
    init_jj_repo(source.path());
    let home = tempfile::tempdir().unwrap();

    create_workspace(home.path(), &SimpleCowBackend, "foo", source.path(), None).unwrap();
    destroy_workspace(home.path(), &SimpleCowBackend, "foo", true).unwrap();

    assert!(!state_dir(home.path(), "foo").exists());
    assert_workspace_list_contains(source.path(), "pando-foo", true);
}

#[test]
fn simple_cow_copies_uncommitted_files_but_base_remains_parent() {
    if skip_if_jj_unavailable("jj dirty canonical integration test") {
        return;
    }

    let source = tempfile::tempdir().unwrap();
    init_jj_repo_with_base_and_empty_wc(source.path());
    fs::write(source.path().join("file.txt"), "canonical dirty edit\n").unwrap();
    let canonical_parent = jj_commit_id(source.path(), "@-");
    let home = tempfile::tempdir().unwrap();

    let workspace =
        create_workspace(home.path(), &SimpleCowBackend, "foo", source.path(), None).unwrap();

    assert_eq!(
        jj_commit_id(&workspace, "@-"),
        canonical_parent,
        "dirty canonical workspaces still base pando @ on canonical @-"
    );
    // Current copy-on-create behavior includes uncommitted source file contents
    // in the Pando workspace, so only the jj base is cleanly anchored at
    // canonical @-; the new workspace diff can still be dirty.
    assert_diff_summary_contains(&workspace, "file.txt");
}

#[test]
fn create_from_revset_bases_workspace_at_requested_revision() {
    if skip_if_jj_unavailable("jj create --from integration test") {
        return;
    }

    let source = tempfile::tempdir().unwrap();
    init_jj_repo_with_two_commits_and_empty_wc(source.path());
    let requested_base = jj_commit_id(source.path(), "@--");
    let default_base = jj_commit_id(source.path(), "@-");
    assert_ne!(
        requested_base, default_base,
        "test setup should have two candidate bases"
    );
    let home = tempfile::tempdir().unwrap();

    let create = Command::new(env!("CARGO_BIN_EXE_pando"))
        .args(["create", "from-base", "--from", "@--"])
        .env("PANDO_HOME", home.path())
        .current_dir(source.path())
        .output()
        .unwrap();
    assert_success("pando create --from @--", &create);
    let workspace = Path::new(std::str::from_utf8(&create.stdout).unwrap().trim()).to_path_buf();

    assert_workspace_list_contains(source.path(), "pando-from-base", true);
    assert_eq!(
        jj_commit_id(&workspace, "@-"),
        requested_base,
        "pando create --from should base pando @ on the requested revset"
    );
}

#[test]
fn create_from_multi_commit_revset_fails_without_registering_workspace() {
    if skip_if_jj_unavailable("jj create --from failure integration test") {
        return;
    }

    let source = tempfile::tempdir().unwrap();
    init_jj_repo_with_two_commits_and_empty_wc(source.path());
    let home = tempfile::tempdir().unwrap();

    let create = Command::new(env!("CARGO_BIN_EXE_pando"))
        .args(["create", "bad-from", "--from", "all()"])
        .env("PANDO_HOME", home.path())
        .current_dir(source.path())
        .output()
        .unwrap();

    assert!(
        !create.status.success(),
        "pando create --from all() should fail because it resolves multiple commits"
    );
    assert!(!state_dir(home.path(), "bad-from").exists());
    assert_workspace_list_contains(source.path(), "pando-bad-from", false);
}

fn init_jj_repo(path: &Path) {
    let output = jj_command(&["git", "init", "--no-colocate"])
        .arg(path)
        .output()
        .unwrap();
    assert_success("jj git init", &output);
}

fn init_jj_repo_with_base_and_empty_wc(path: &Path) {
    init_jj_repo(path);
    fs::write(path.join("file.txt"), "base\n").unwrap();
    jj_success(path, &["file", "track", "file.txt"]);
    jj_success(path, &["describe", "-m", "base"]);
    jj_success(path, &["new"]);
}

fn init_jj_repo_with_two_commits_and_empty_wc(path: &Path) {
    init_jj_repo_with_base_and_empty_wc(path);
    fs::write(path.join("file.txt"), "second\n").unwrap();
    jj_success(path, &["describe", "-m", "second"]);
    jj_success(path, &["new"]);
}

fn assert_workspace_list_contains(repo: &Path, workspace: &str, expected: bool) {
    let stdout = jj_stdout(repo, &["workspace", "list"]);
    let found = stdout.lines().any(|line| {
        line.strip_prefix(workspace)
            .is_some_and(|rest| rest.starts_with(':'))
    });

    assert_eq!(
        found, expected,
        "workspace list expectation for {workspace}={expected} failed:\n{stdout}"
    );
}

fn assert_clean_diff(repo: &Path) {
    let summary = jj_stdout(repo, &["diff", "--summary"]);
    assert!(
        summary.trim().is_empty(),
        "expected clean jj diff in {}, got:\n{summary}",
        repo.display()
    );
}

fn assert_diff_summary_contains(repo: &Path, path: &str) {
    let summary = jj_stdout(repo, &["diff", "--summary"]);
    assert!(
        summary.lines().any(|line| line.ends_with(path)),
        "expected jj diff summary in {} to mention {path}, got:\n{summary}",
        repo.display()
    );
}

fn jj_success(repo: &Path, args: &[&str]) {
    let output = jj_command(args).current_dir(repo).output().unwrap();
    assert_success(&format!("jj {}", args.join(" ")), &output);
}

fn jj_commit_id(repo: &Path, revset: &str) -> String {
    jj_template(repo, revset, "commit_id")
}

fn jj_template(repo: &Path, revset: &str, template: &str) -> String {
    jj_stdout(repo, &["log", "--no-graph", "-r", revset, "-T", template])
}

fn jj_stdout(repo: &Path, args: &[&str]) -> String {
    let output = jj_command(args).current_dir(repo).output().unwrap();
    assert_success(&format!("jj {}", args.join(" ")), &output);
    String::from_utf8(output.stdout).unwrap()
}

fn write_jj_user_config(config_home: &Path, name: &str, email: &str) {
    let jj_config_dir = config_home.join("jj");
    fs::create_dir_all(&jj_config_dir).unwrap();
    fs::write(
        jj_config_dir.join("config.toml"),
        format!("user.name = {name:?}\nuser.email = {email:?}\n"),
    )
    .unwrap();
}

fn jj_command(args: &[&str]) -> Command {
    let mut command = Command::new("jj");
    command
        .arg("--config")
        .arg(format!("user.name={TEST_USER_NAME}"))
        .arg("--config")
        .arg(format!("user.email={TEST_USER_EMAIL}"))
        .args(args);
    command
}

fn assert_success(command: &str, output: &std::process::Output) {
    assert!(
        output.status.success(),
        "{command} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
