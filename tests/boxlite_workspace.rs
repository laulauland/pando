#![cfg(feature = "microvm-boxlite")]

mod common;

use pando::runtime::{BoxLiteRuntimeBackend, RuntimeBackend, RuntimeIdentity};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::PermissionsExt,
    },
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires KVM/HVF, OCI registry access, and fuse-overlayfs or APFS"]
fn non_jj_and_native_jj_workspaces_run_tools_and_preserve_isolation() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    let home = fixture.path().join("home");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("original.txt"), "canonical").unwrap();
    install_fs_probe(&source);
    write_qualified_runtime_config(&home, "alpine:3.22", 1, 256);
    let _cleanup = WorkspaceCleanup {
        home: home.clone(),
        source: source.clone(),
    };

    let mut create_demo = command(&home, &source);
    create_demo.args(["create", "demo"]);
    create_demo.assert_success();
    let limits = command(&home, &source)
        .args([
            "exec",
            "demo",
            "--",
            "sh",
            "-c",
            "printf 'cpus='; getconf _NPROCESSORS_ONLN; awk '/MemTotal/ { print \"memory_kib=\" $2 }' /proc/meminfo; printf 'interfaces='; ls /sys/class/net | tr '\n' ','; printf '\ndefault_routes='; awk 'NR > 1 && $2 == \"00000000\" { count++ } END { print count+0 }' /proc/net/route",
        ])
        .assert_success();
    let limits = String::from_utf8(limits.stdout).unwrap();
    assert!(limits.contains("cpus=1"), "{limits}");
    let memory_kib: u32 = limits
        .split("memory_kib=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!((200_000..=300_000).contains(&memory_kib), "{limits}");
    assert!(limits.contains("interfaces="), "{limits}");
    assert!(limits.contains("eth0"), "{limits}");
    assert!(limits.contains("default_routes=1"), "{limits}");
    command(&home, &source)
        .args([
            "exec",
            "demo",
            "--",
            "sh",
            "-c",
            "wget -q -O /dev/null https://example.com",
        ])
        .assert_success();
    command(&home, &source)
        .args([
            "exec",
            "demo",
            "--",
            "sh",
            "-c",
            "printf changed > original.txt; printf guest > guest.txt; printf disk > /root/pando-marker",
        ])
        .assert_success();

    let workspace = home.join("workspaces/demo");
    assert_eq!(
        fs::read_to_string(workspace.join("original.txt")).unwrap(),
        "changed"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("guest.txt")).unwrap(),
        "guest"
    );
    assert_eq!(
        fs::read_to_string(source.join("original.txt")).unwrap(),
        "canonical"
    );
    assert!(!source.join("guest.txt").exists());

    command(&home, &source)
        .args([
            "exec", "demo", "--", "sh", "-c",
            "mkdir -p agent-project/tmp; printf '#!/bin/sh\nset -eu\ntest -L linked\ntest -x runner\ncmp large.bin copied.bin\n' > agent-project/test.sh; printf '#!/bin/sh\nprintf built\n' > agent-project/runner; chmod 755 agent-project/test.sh agent-project/runner; ln -s runner agent-project/linked; dd if=/dev/zero of=agent-project/large.bin bs=1M count=8 status=none; cp agent-project/large.bin agent-project/copied.bin; mv agent-project/tmp agent-project/renamed; rmdir agent-project/renamed; ./agent-project/runner | grep built; if test -x /workspace/fs-probe; then /workspace/fs-probe agent-project/mmap.bin; fi; (sleep 300 >/dev/null 2>&1 & echo $! > agent-project/pre-stop.pid); (cd agent-project && ./test.sh)",
        ])
        .assert_success();
    assert!(workspace.join("agent-project/linked").is_symlink());
    assert_eq!(
        fs::metadata(workspace.join("agent-project/large.bin"))
            .unwrap()
            .len(),
        8 * 1024 * 1024
    );
    #[cfg(target_os = "linux")]
    assert_eq!(
        &fs::read(workspace.join("agent-project/mmap.bin")).unwrap()[..4],
        b"mmap"
    );
    #[cfg(target_os = "linux")]
    run_cross_boundary_lock_qualification(&home, &source, &workspace, "demo");

    command(&home, &source)
        .args(["stop", "demo"])
        .assert_success();
    command(&home, &source)
        .args(["stop", "demo"])
        .assert_success();
    let restarted = command(&home, &source)
        .args([
            "exec",
            "demo",
            "--",
            "sh",
            "-c",
            "cat /root/pando-marker; cat guest.txt",
        ])
        .assert_success();
    assert_eq!(String::from_utf8(restarted.stdout).unwrap(), "diskguest");
    command(&home, &source)
        .args([
            "exec",
            "demo",
            "--",
            "sh",
            "-c",
            "pid=$(cat agent-project/pre-stop.pid); test ! -r /proc/$pid/cmdline || ! tr '\\0' ' ' < /proc/$pid/cmdline | grep -q 'sleep 300'; (cd agent-project && ./test.sh)",
        ])
        .assert_success();

    let redirected_input = b"regular-file stdin\nsecond line\0binary tail";
    let redirected_path = fixture.path().join("redirected-input");
    fs::write(&redirected_path, redirected_input).unwrap();
    command(&home, &source)
        .args([
            "exec",
            "demo",
            "--",
            "sh",
            "-c",
            "cat > redirected-stdin.bin",
        ])
        .stdin(fs::File::open(&redirected_path).unwrap())
        .assert_success();
    let redirected = command(&home, &source)
        .args(["exec", "demo", "--", "cat", "redirected-stdin.bin"])
        .assert_success();
    assert_eq!(redirected.stdout, redirected_input);

    let info = command(&home, &source)
        .args(["info", "demo", "--json"])
        .assert_success();
    let info: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
    assert_eq!(info["runtime"]["kind"], "boxlite");
    assert_eq!(info["runtime"]["image"], "alpine:3.22");
    assert_eq!(info["runtime"]["cpu_count"], 1);
    assert_eq!(info["runtime"]["memory_mib"], 256);
    assert_eq!(info["runtime"]["network"], "enabled");
    assert_eq!(info["runtime"]["seccomp"], common::expected_seccomp_json());
    assert_eq!(info["runtime"]["state"], "running");
    let provider_id = info["runtime"]["provider_id"].as_str().unwrap().to_owned();
    assert!(provider_id.len() > 4);
    let box_home = home.join("runtime/boxlite/boxes").join(&provider_id);
    let decoy_name = format!("{}-decoy/bin/boxlite-shim", box_home.display());
    let mut decoy = Command::new("bash")
        .args(["-c", "exec -a \"$0\" sleep 60", &decoy_name])
        .spawn()
        .unwrap();

    let mut shell = command(&home, &source)
        .args(["shell", "demo"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    shell
        .stdin
        .take()
        .unwrap()
        .write_all(b"printf shell-pty-ok; exit\n")
        .unwrap();
    let shell_output = shell.wait_with_output().unwrap();
    assert!(shell_output.status.success());
    assert!(String::from_utf8_lossy(&shell_output.stdout).contains("shell-pty-ok"));

    let terminal_output = run_in_terminal(
        command(&home, &source).args(["shell", "demo"]),
        b"stty size; exit\n",
        false,
    );
    assert!(terminal_output.contains("24 80"), "{terminal_output:?}");

    let terminal_signal_output = run_in_terminal(
        command(&home, &source).args(["shell", "demo"]),
        b"trap 'echo terminal-signal-ok; exit 0' INT; echo ready; while :; do sleep 1; done\n",
        true,
    );
    assert!(terminal_signal_output.contains("terminal-signal-ok"));

    let mut signaled_shell = command(&home, &source)
        .args(["shell", "demo"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    signaled_shell
        .stdin
        .take()
        .unwrap()
        .write_all(b"trap 'echo signal-ok; exit 0' INT; echo ready; while :; do sleep 1; done\n")
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    // SAFETY: the child PID is live and SIGINT is a valid signal.
    assert_eq!(
        unsafe { libc::kill(signaled_shell.id() as i32, libc::SIGINT) },
        0
    );
    let signal_output = signaled_shell.wait_with_output().unwrap();
    assert!(signal_output.status.success());
    assert!(String::from_utf8_lossy(&signal_output.stdout).contains("signal-ok"));

    let failure = command(&home, &source)
        .args(["exec", "demo", "--", "sh", "-c", "exit 23"])
        .output()
        .unwrap();
    assert_eq!(failure.status.code(), Some(23));

    if crash_injection_expected() {
        for (name, point) in [
            ("crash-temp-create", "journal-temp-created"),
            ("crash-temp-write", "journal-temp-written"),
            ("crash-journal", "journal-published"),
            ("crash-intent", "create-intent"),
            ("crash-provider", "provider-created"),
            ("crash-start", "provider-started"),
        ] {
            assert_provisional_create_recovers(&home, &source, name, point, 1);
        }

        for (name, point) in [
            ("crash-clean-file", "journal-cleanup-published-unlinked"),
            ("crash-clean-temp", "journal-cleanup-temp-unlinked"),
            ("crash-clean-dir", "journal-cleanup-dir-unlinked"),
        ] {
            assert_committed_create_recovers(&home, &source, name, point);
        }

        let mut create_crash_commit = command(&home, &source);
        create_crash_commit
            .env("PANDO_TEST_CRASH_POINT", "metadata-published")
            .args([
                "create",
                "crash-commit",
                "--runtime",
                "boxlite",
                "--image",
                "alpine:3.22",
            ]);
        common::add_qualified_runtime_cli_args(&mut create_crash_commit);
        let interrupted_commit = create_crash_commit.output().unwrap();
        assert_sigkill(interrupted_commit);
        command(&home, &source)
            .args(["info", "crash-commit"])
            .assert_success();
        assert!(home.join("state/crash-commit/meta.toml").exists());
        assert!(!home
            .join("transactions/crash-commit/runtime-create.toml")
            .exists());
        command(&home, &source)
            .args(["remove", "crash-commit"])
            .assert_success();
    }

    let async_runtime = tokio::runtime::Runtime::new().unwrap();
    async_runtime.block_on(async {
        let runtime = BoxLiteRuntimeBackend::new(&home).unwrap();
        let identity = RuntimeIdentity::new(provider_id.clone());
        runtime.stop(&identity).await.unwrap();
        runtime.remove(&identity).await.unwrap();
    });
    let drift = command(&home, &source)
        .args(["info", "demo"])
        .output()
        .unwrap();
    assert!(!drift.status.success());
    assert!(
        String::from_utf8_lossy(&drift.stderr).contains("runtime not found"),
        "{}",
        String::from_utf8_lossy(&drift.stderr)
    );
    command(&home, &source)
        .args(["remove", "demo"])
        .assert_success();
    assert!(!home.join("state/demo").exists());
    assert!(!workspace.exists());
    let boxes = home.join("runtime/boxlite/boxes");
    assert_eq!(fs::read_dir(boxes).unwrap().count(), 0);
    assert_no_owned_processes(&box_home);
    assert!(decoy.try_wait().unwrap().is_none());
    decoy.kill().unwrap();
    decoy.wait().unwrap();

    run_jj_workspace_workflow(fixture.path(), &home);
}

fn run_jj_workspace_workflow(fixture: &Path, home: &Path) {
    let source = fixture.join("jj source with spaces");
    let jj = host_jj_binary();
    let initialized = Command::new(&jj)
        .args(["git", "init"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        initialized.status.success(),
        "could not initialize jj fixture: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    fs::write(source.join("tracked.txt"), "canonical\n").unwrap();
    fs::write(
        source.join("jj-config.toml"),
        "[user]\nname = \"Pando Guest\"\nemail = \"guest@example.invalid\"\n",
    )
    .unwrap();
    Command::new(&jj)
        .current_dir(&source)
        .args(["describe", "-m", "canonical base"])
        .assert_success();
    Command::new(&jj)
        .current_dir(&source)
        .args(["new"])
        .assert_success();
    let mut create_jjdemo = command(home, &source);
    create_jjdemo.args([
        "create",
        "jjdemo",
        "--runtime",
        "boxlite",
        "--image",
        "alpine:3.22",
    ]);
    common::add_qualified_runtime_cli_args(&mut create_jjdemo);
    create_jjdemo.assert_success();
    let workspace = home.join("workspaces/jjdemo");
    let runtime_info = command(home, &source)
        .args(["info", "jjdemo", "--json"])
        .assert_success();
    let runtime_info: serde_json::Value = serde_json::from_slice(&runtime_info.stdout).unwrap();
    let box_home = home
        .join("runtime/boxlite/boxes")
        .join(runtime_info["runtime"]["provider_id"].as_str().unwrap());
    fs::write(source.join("canonical-after-create.txt"), "host only\n").unwrap();
    Command::new(&jj)
        .current_dir(&source)
        .args(["status"])
        .assert_success();
    let canonical_head = fs::read(source.join(".git/HEAD")).unwrap();
    let canonical_index = fs::read(source.join(".git/index")).ok();
    let canonical_tracked = fs::read(source.join("tracked.txt")).unwrap();
    let jj_command = "JJ_CONFIG=/workspace/jj-config.toml jj";

    let root = command(home, &source)
        .args([
            "exec",
            "jjdemo",
            "--",
            "sh",
            "-c",
            &format!("{jj_command} root"),
        ])
        .assert_success();
    assert_eq!(String::from_utf8(root.stdout).unwrap().trim(), "/workspace");
    let guest_jj = command(home, &source)
        .args(["exec", "jjdemo", "--", "sh", "-c", "command -v jj"])
        .assert_success();
    assert_eq!(
        String::from_utf8(guest_jj.stdout).unwrap().trim(),
        "/usr/local/bin/jj"
    );
    command(home, &source)
        .args([
            "exec",
            "jjdemo",
            "--",
            "sh",
            "-c",
            "test -e /sys/class/net/eth0 && awk 'NR > 1 && $2 == \"00000000\" { found=1 } END { exit !found }' /proc/net/route && wget -q -O /dev/null https://example.com",
        ])
        .assert_success();
    assert!(!workspace.join("guest-jj").exists());
    assert!(!workspace.join(".jj/pando-tools-stage").exists());
    command(home, &source)
        .args([
            "exec",
            "jjdemo",
            "--",
            "sh",
            "-c",
            &format!(
                "{jj_command} status >/tmp/status && printf 'guest edit\\n' > tracked.txt && {jj_command} diff --summary | grep tracked.txt && {jj_command} describe -m 'guest change' && {jj_command} new && {jj_command} log -r @- --no-graph -T description | grep 'guest change' && test ! -e '/jj source with spaces/canonical-after-create.txt'"
            ),
        ])
        .assert_success();
    command(home, &source)
        .args([
            "exec",
            "jjdemo",
            "--",
            "sh",
            "-c",
            &format!(
                "{jj_command} bookmark create pando-guest-check -r @- && {jj_command} bookmark list pando-guest-check | grep pando-guest-check"
            ),
        ])
        .assert_success();
    let host_bookmark = Command::new(&jj)
        .current_dir(&source)
        .args(["bookmark", "list", "pando-guest-check"])
        .assert_success();
    assert!(
        String::from_utf8_lossy(&host_bookmark.stdout).contains("pando-guest-check"),
        "host did not observe guest bookmark: {}",
        String::from_utf8_lossy(&host_bookmark.stdout)
    );
    assert_eq!(
        fs::read_to_string(workspace.join("tracked.txt")).unwrap(),
        "guest edit\n"
    );
    assert_eq!(
        fs::read_to_string(source.join("tracked.txt")).unwrap(),
        "canonical\n"
    );
    let host_log = Command::new(&jj)
        .current_dir(&workspace)
        .args(["log", "-r", "@-", "--no-graph", "-T", "description"])
        .assert_success();
    assert!(String::from_utf8_lossy(&host_log.stdout).contains("guest change"));
    let qualified_change = run_concurrent_jj_qualification(&jj, home, &source, jj_command);
    run_seeded_concurrent_jj_stress(&jj, home, &source, jj_command);
    run_stale_workspace_recovery(&jj, home, &source, jj_command);
    verify_colocated_repository_integrity(&jj, home, &source, jj_command);
    assert_eq!(fs::read(source.join(".git/HEAD")).unwrap(), canonical_head);
    assert_eq!(fs::read(source.join(".git/index")).ok(), canonical_index);
    assert_eq!(
        fs::read(source.join("tracked.txt")).unwrap(),
        canonical_tracked
    );
    assert_eq!(
        fs::read_to_string(source.join("canonical-after-create.txt")).unwrap(),
        "host only\n"
    );

    command(home, &source)
        .args(["stop", "jjdemo"])
        .assert_success();
    let restarted = command(home, &source)
        .args([
            "exec",
            "jjdemo",
            "--",
            "sh",
            "-c",
            &format!(
                "{jj_command} workspace update-stale && {jj_command} log -r '{qualified_change}' --no-graph -T description"
            ),
        ])
        .assert_success();
    assert_eq!(restarted.stdout, b"concurrent guest write\n");

    if crash_injection_expected() {
        let interrupted_stop = command(home, &source)
            .env("PANDO_TEST_CRASH_POINT", "remove-stopped")
            .args(["remove", "jjdemo"])
            .output()
            .unwrap();
        assert_sigkill(interrupted_stop);
        assert!(home.join("state/jjdemo/meta.toml").exists());
        assert!(workspace.exists());
        let interrupted_remove = command(home, &source)
            .env("PANDO_TEST_CRASH_POINT", "remove-provider-removed")
            .args(["remove", "jjdemo"])
            .output()
            .unwrap();
        assert_sigkill(interrupted_remove);
        assert!(home.join("state/jjdemo/meta.toml").exists());
        assert!(workspace.exists());
        let interrupted_forget = command(home, &source)
            .env("PANDO_TEST_CRASH_POINT", "remove-jj-forgotten")
            .args(["remove", "jjdemo"])
            .output()
            .unwrap();
        assert_sigkill(interrupted_forget);
        assert!(home.join("state/jjdemo/meta.toml").exists());
        assert!(workspace.exists());
    }
    command(home, &source)
        .args(["remove", "jjdemo"])
        .assert_success();
    assert!(!workspace.exists());
    assert_not_mounted(&workspace);
    assert_no_owned_processes(&box_home);
    let workspaces = Command::new(&jj)
        .current_dir(&source)
        .args(["workspace", "list"])
        .assert_success();
    assert!(!String::from_utf8_lossy(&workspaces.stdout).contains("pando-jjdemo"));
    assert_eq!(
        fs::read_to_string(source.join("tracked.txt")).unwrap(),
        "canonical\n"
    );
}

fn run_stale_workspace_recovery(jj: &Path, home: &Path, source: &Path, jj_command: &str) {
    command(home, source)
        .args([
            "exec",
            "jjdemo",
            "--",
            "sh",
            "-c",
            &format!("{jj_command} new 'root()' -m 'stale recovery baseline'"),
        ])
        .assert_success();
    Command::new(jj)
        .current_dir(source)
        .args(["log", "-r", "all()", "--no-graph", "-T", "commit_id"])
        .assert_success();
    let guest_change = command(home, source)
        .args([
            "exec",
            "jjdemo",
            "--",
            "sh",
            "-c",
            &format!("{jj_command} log -r @ --no-graph -T commit_id"),
        ])
        .assert_success();
    let guest_change = String::from_utf8(guest_change.stdout).unwrap();
    let stale_source = source.parent().unwrap().join("stale source workspace");
    Command::new(jj)
        .current_dir(source)
        .args([
            "workspace",
            "add",
            stale_source.to_str().unwrap(),
            "--name",
            "pando-stale-source",
            "--revision",
            "root()",
        ])
        .assert_success();
    fs::write(stale_source.join("host-added-to-stale-target"), "host\n").unwrap();
    Command::new(jj)
        .current_dir(&stale_source)
        .args(["describe", "-m", "stale source tree change"])
        .assert_success();
    let stale_source_commit = Command::new(jj)
        .current_dir(&stale_source)
        .args(["log", "-r", "@", "--no-graph", "-T", "commit_id"])
        .assert_success();
    let stale_source_commit = String::from_utf8(stale_source_commit.stdout).unwrap();
    Command::new(jj)
        .current_dir(source)
        .args([
            "--ignore-working-copy",
            "squash",
            "--from",
            stale_source_commit.trim(),
            "--into",
            guest_change.trim(),
            "-m",
            "host rewrite requiring guest refresh",
        ])
        .assert_success();

    let stale = command(home, source)
        .args([
            "exec",
            "jjdemo",
            "--",
            "sh",
            "-c",
            &format!("{jj_command} --config snapshot.auto-update-stale=false status"),
        ])
        .output()
        .unwrap();
    assert!(
        !stale.status.success()
            && String::from_utf8_lossy(&stale.stderr)
                .to_ascii_lowercase()
                .contains("stale"),
        "expected a classified stale-workspace response, got status {:?}, stdout {}, stderr {}",
        stale.status,
        String::from_utf8_lossy(&stale.stdout),
        String::from_utf8_lossy(&stale.stderr)
    );
    command(home, source)
        .args([
            "exec",
            "jjdemo",
            "--",
            "sh",
            "-c",
            &format!("{jj_command} workspace update-stale && {jj_command} status"),
        ])
        .assert_success();
    Command::new(jj)
        .current_dir(source)
        .args(["workspace", "forget", "pando-stale-source"])
        .assert_success();
    fs::remove_dir_all(stale_source).unwrap();
}

fn verify_colocated_repository_integrity(jj: &Path, home: &Path, source: &Path, jj_command: &str) {
    Command::new("git")
        .current_dir(source)
        .args(["fsck", "--strict"])
        .assert_success();
    Command::new(jj)
        .current_dir(source)
        .args(["debug", "index"])
        .assert_success();
    Command::new(jj)
        .current_dir(source)
        .args(["op", "log", "--no-graph", "-n", "40"])
        .assert_success();
    command(home, source)
        .args([
            "exec",
            "jjdemo",
            "--",
            "sh",
            "-c",
            &format!(
                "{jj_command} debug index && {jj_command} op log --no-graph -n 40 && {jj_command} log -r 'all()' --no-graph -T commit_id"
            ),
        ])
        .assert_success();
}

fn run_seeded_concurrent_jj_stress(jj: &Path, home: &Path, source: &Path, jj_command: &str) {
    const SEED: u64 = 0x5041_4e44_4f43_4f57;
    const ROUNDS: usize = 8;

    let original = Command::new(jj)
        .current_dir(source)
        .args(["log", "-r", "@", "--no-graph", "-T", "commit_id"])
        .assert_success();
    let original = String::from_utf8(original.stdout).unwrap();
    let mut targets = Vec::with_capacity(ROUNDS * 2);
    for index in 0..ROUNDS * 2 {
        Command::new(jj)
            .current_dir(source)
            .args([
                "new",
                "root()",
                "-m",
                &format!("stress target seed={SEED:#x} index={index}"),
            ])
            .assert_success();
        let target = Command::new(jj)
            .current_dir(source)
            .args(["log", "-r", "@", "--no-graph", "-T", "change_id"])
            .assert_success();
        targets.push(String::from_utf8(target.stdout).unwrap().trim().to_owned());
    }
    Command::new(jj)
        .current_dir(source)
        .args(["edit", original.trim()])
        .assert_success();

    let mut state = SEED;
    for round in 0..ROUNDS {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let pair = if state & 1 == 0 {
            [round * 2, round * 2 + 1]
        } else {
            [round * 2 + 1, round * 2]
        };
        let host_target = targets[pair[0]].clone();
        let guest_target = targets[pair[1]].clone();
        let host_jj = jj.to_owned();
        let host_source = source.to_owned();
        let host = std::thread::spawn(move || {
            Command::new(host_jj)
                .current_dir(host_source)
                .args([
                    "describe",
                    "-r",
                    &host_target,
                    "-m",
                    &format!("stress host seed={SEED:#x} round={round}"),
                ])
                .output()
                .unwrap()
        });
        let guest_home = home.to_owned();
        let guest_source = source.to_owned();
        let guest_command = jj_command.to_owned();
        let guest = std::thread::spawn(move || {
            command(&guest_home, &guest_source)
                .args([
                    "exec",
                    "jjdemo",
                    "--",
                    "sh",
                    "-c",
                    &format!(
                        "{guest_command} describe -r '{guest_target}' -m 'stress guest seed={SEED:#x} round={round}'"
                    ),
                ])
                .output()
                .unwrap()
        });
        let host = host.join().unwrap();
        let guest = guest.join().unwrap();
        assert!(
            host.status.success(),
            "host jj stress failure (seed={SEED:#x}, round={round}): {}",
            String::from_utf8_lossy(&host.stderr)
        );
        assert!(
            guest.status.success(),
            "guest jj stress failure (seed={SEED:#x}, round={round}): {}",
            String::from_utf8_lossy(&guest.stderr)
        );
        for (target, expected) in [
            (
                &targets[pair[0]],
                format!("stress host seed={SEED:#x} round={round}\n"),
            ),
            (
                &targets[pair[1]],
                format!("stress guest seed={SEED:#x} round={round}\n"),
            ),
        ] {
            let observed = Command::new(jj)
                .current_dir(source)
                .args(["log", "-r", target, "--no-graph", "-T", "description"])
                .assert_success();
            assert_eq!(
                String::from_utf8(observed.stdout).unwrap(),
                expected,
                "concurrent mutation was lost (seed={SEED:#x}, round={round}, target={target})"
            );
        }
    }

    Command::new(jj)
        .current_dir(source)
        .args(["status"])
        .assert_success();
    Command::new(jj)
        .current_dir(source)
        .args(["log", "-r", "all()", "--no-graph", "-T", "commit_id"])
        .assert_success();
    Command::new(jj)
        .current_dir(source)
        .args(["op", "log", "--no-graph", "-n", "40"])
        .assert_success();
    command(home, source)
        .args([
            "exec",
            "jjdemo",
            "--",
            "sh",
            "-c",
            &format!(
                "{jj_command} workspace update-stale && {jj_command} status && {jj_command} op log --no-graph -n 40"
            ),
        ])
        .assert_success();
}

fn run_concurrent_jj_qualification(
    jj: &Path,
    home: &Path,
    source: &Path,
    jj_command: &str,
) -> String {
    let workspace = home.join("workspaces/jjdemo");
    let shared_change = Command::new(jj)
        .current_dir(&workspace)
        .args(["log", "-r", "@-", "--no-graph", "-T", "change_id"])
        .assert_success();
    let shared_change = String::from_utf8(shared_change.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let read_ready = workspace.join("jj-read-ready");
    let read_release = workspace.join("jj-read-release");
    let guest_home = home.to_owned();
    let guest_source = source.to_owned();
    let guest_command = jj_command.to_owned();
    let guest_target = shared_change.clone();
    let guest = std::thread::spawn(move || {
        command(&guest_home, &guest_source)
            .args([
                "exec",
                "jjdemo",
                "--",
                "sh",
                "-c",
                &format!("touch jj-read-ready; while test ! -e jj-read-release; do sleep 0.01; done; {guest_command} log -r 'all()' --no-graph -T commit_id"),
            ])
            .output()
            .unwrap()
    });
    wait_for_path(&read_ready);
    fs::write(&read_release, "go").unwrap();
    let host = Command::new(jj)
        .current_dir(source)
        .args(["log", "-r", "all()", "--no-graph", "-T", "commit_id"])
        .output()
        .unwrap();
    assert!(host.status.success());
    assert!(guest.join().unwrap().status.success());

    let host_ready = workspace.join("jj-host-editor-ready");
    let guest_ready = workspace.join("jj-guest-editor-ready");
    let editors_release = workspace.join("jj-editors-release");
    let host_editor = workspace.join("jj-editor-host.sh");
    let guest_editor = workspace.join("jj-editor-guest.sh");
    fs::write(
        &host_editor,
        format!(
            "#!/bin/sh\nset -eu\ntouch '{}'\nwhile test ! -e '{}'; do sleep 0.01; done\nprintf 'concurrent host write\\n' > \"$1\"\n",
            host_ready.display(),
            editors_release.display()
        ),
    )
    .unwrap();
    fs::write(
        &guest_editor,
        "#!/bin/sh\nset -eu\ntouch /workspace/jj-guest-editor-ready\nwhile test ! -e /workspace/jj-editors-release; do sleep 0.01; done\nprintf 'concurrent guest write\\n' > \"$1\"\n",
    )
    .unwrap();
    fs::set_permissions(&host_editor, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&guest_editor, fs::Permissions::from_mode(0o755)).unwrap();

    let guest_home = home.to_owned();
    let guest_source = source.to_owned();
    let guest_command = jj_command.to_owned();
    let guest = std::thread::spawn(move || {
        command(&guest_home, &guest_source)
            .args([
                "exec",
                "jjdemo",
                "--",
                "sh",
                "-c",
                &format!(
                    "{guest_command} --config ui.editor=/workspace/jj-editor-guest.sh describe -r '{guest_target}'"
                ),
            ])
            .output()
            .unwrap()
    });
    let host_jj = jj.to_owned();
    let host_source = source.to_owned();
    let host_editor_arg = host_editor.to_string_lossy().into_owned();
    let host_target = shared_change.clone();
    let host = std::thread::spawn(move || {
        Command::new(host_jj)
            .current_dir(host_source)
            .args([
                "--config",
                &format!("ui.editor={host_editor_arg}"),
                "describe",
                "-r",
                &host_target,
            ])
            .output()
            .unwrap()
    });
    // Reaching both editor hooks proves both real jj processes have opened
    // their transactions and are simultaneously paused before committing.
    wait_for_path(&host_ready);
    wait_for_path(&guest_ready);
    fs::write(&editors_release, "go").unwrap();
    let host = host.join().unwrap();
    let guest = guest.join().unwrap();
    assert!(
        host.status.success(),
        "host jj failed during controlled contention: {}",
        String::from_utf8_lossy(&host.stderr)
    );
    assert!(
        guest.status.success(),
        "guest jj failed during controlled contention: {}",
        String::from_utf8_lossy(&guest.stderr)
    );
    let divergent_lookup = Command::new(jj)
        .current_dir(source)
        .args([
            "log",
            "-r",
            &shared_change,
            "--no-graph",
            "-T",
            "description",
        ])
        .output()
        .unwrap();
    assert!(
        !divergent_lookup.status.success()
            && String::from_utf8_lossy(&divergent_lookup.stderr).contains("is divergent"),
        "same-change contention was not diagnosed as divergence: {}",
        String::from_utf8_lossy(&divergent_lookup.stderr)
    );
    let divergent_revisions = Command::new(jj)
        .current_dir(source)
        .args([
            "log",
            "-r",
            &format!("change_id({shared_change})"),
            "--no-graph",
            "-T",
            "commit_id ++ \"\\t\" ++ description",
        ])
        .assert_success();
    let divergent_revisions = String::from_utf8(divergent_revisions.stdout).unwrap();
    assert!(divergent_revisions.contains("\tconcurrent host write\n"));
    assert!(divergent_revisions.contains("\tconcurrent guest write\n"));
    let host_commit = divergent_revisions
        .lines()
        .find_map(|line| line.strip_suffix("\tconcurrent host write"))
        .expect("host side of divergent change was absent");
    Command::new(jj)
        .current_dir(source)
        .args(["abandon", host_commit])
        .assert_success();
    let final_description = Command::new(jj)
        .current_dir(source)
        .args([
            "log",
            "-r",
            &shared_change,
            "--no-graph",
            "-T",
            "description",
        ])
        .assert_success();
    assert_eq!(final_description.stdout, b"concurrent guest write\n");
    Command::new(jj)
        .current_dir(source)
        .args(["status"])
        .assert_success();
    Command::new(jj)
        .current_dir(source)
        .args(["log", "-r", "all()", "--no-graph", "-T", "commit_id"])
        .assert_success();
    Command::new(jj)
        .current_dir(source)
        .args(["op", "log", "--no-graph", "-n", "10"])
        .assert_success();
    shared_change
}

fn host_jj_binary() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("PANDO_TEST_JJ") {
        return path.into();
    }
    let output = Command::new("mise").args(["which", "jj"]).output().unwrap();
    assert!(output.status.success(), "mise could not resolve jj");
    std::path::PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

#[cfg(target_os = "linux")]
fn install_fs_probe(source: &Path) {
    let build = tempfile::tempdir().unwrap();
    let source_file = build.path().join("probe.c");
    fs::write(
        &source_file,
        r#"#include <errno.h>
#include <fcntl.h>
#include <sys/file.h>
#include <sys/mman.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
  if (argc == 5 && !strcmp(argv[1], "hold-exclusive")) {
    int fd = open(argv[2], O_RDWR | O_CREAT | O_EXCL, 0644);
    if (fd < 0) return 6;
    close(open(argv[3], O_WRONLY | O_CREAT, 0644));
    while (access(argv[4], F_OK)) usleep(10000);
    return close(fd) || unlink(argv[2]);
  }
  if (argc == 3 && !strcmp(argv[1], "try-exclusive")) {
    int fd = open(argv[2], O_RDWR | O_CREAT | O_EXCL, 0644);
    if (fd < 0) return errno == EEXIST ? 10 : 8;
    return close(fd) || unlink(argv[2]);
  }
  if (argc == 5 && (!strcmp(argv[1], "hold-flock") || !strcmp(argv[1], "hold-fcntl"))) {
    int fd = open(argv[2], O_RDWR | O_CREAT, 0644);
    struct flock lock = {.l_type=F_WRLCK, .l_whence=SEEK_SET};
    if (fd < 0) return 6;
    if (!strcmp(argv[1], "hold-flock") ? flock(fd, LOCK_EX) : fcntl(fd, F_SETLKW, &lock)) return 6;
    close(open(argv[3], O_WRONLY | O_CREAT, 0644));
    while (access(argv[4], F_OK)) usleep(10000);
    if (!strcmp(argv[1], "hold-flock")) return flock(fd, LOCK_UN) || close(fd);
    lock.l_type=F_UNLCK;
    return fcntl(fd, F_SETLK, &lock) || close(fd);
  }
  if (argc == 3 && (!strcmp(argv[1], "try-flock") || !strcmp(argv[1], "try-fcntl"))) {
    int fd = open(argv[2], O_RDWR | O_CREAT, 0644);
    if (fd < 0) return 7;
    if (!strcmp(argv[1], "try-flock")) {
      if (flock(fd, LOCK_EX | LOCK_NB)) return errno == EWOULDBLOCK ? 10 : 8;
      return flock(fd, LOCK_UN) || close(fd);
    }
    struct flock lock = {.l_type=F_WRLCK, .l_whence=SEEK_SET};
    if (fcntl(fd, F_SETLK, &lock)) return (errno == EACCES || errno == EAGAIN) ? 10 : 8;
    lock.l_type=F_UNLCK;
    return fcntl(fd, F_SETLK, &lock) || close(fd);
  }
  if (argc != 2) return 2;
  int fd = open(argv[1], O_RDWR | O_CREAT | O_TRUNC, 0644);
  if (fd < 0 || flock(fd, LOCK_EX) || ftruncate(fd, 4096)) return 3;
  char *p = mmap(0, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
  if (p == MAP_FAILED) return 4;
  p[0]='m'; p[1]='m'; p[2]='a'; p[3]='p';
  if (msync(p, 4096, MS_SYNC) || munmap(p, 4096) || flock(fd, LOCK_UN)) return 5;
  return close(fd);
}
"#,
    )
    .unwrap();
    let output = Command::new("cc")
        .arg("-static")
        .arg(&source_file)
        .arg("-o")
        .arg(source.join("fs-probe"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "could not build static filesystem probe: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::set_permissions(source.join("fs-probe"), fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(target_os = "linux"))]
fn install_fs_probe(_source: &Path) {}

fn wait_for_path(path: &Path) {
    for _ in 0..500 {
        if path.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("timed out waiting for {}", path.display());
}

#[cfg(target_os = "linux")]
fn run_cross_boundary_lock_qualification(home: &Path, source: &Path, workspace: &Path, name: &str) {
    let guest_ready = workspace.join("guest-lock-ready");
    let guest_release = workspace.join("guest-lock-release");
    let mut guest_holder = command(home, source)
        .args([
            "exec",
            name,
            "--",
            "/workspace/fs-probe",
            "hold-exclusive",
            "/workspace/cross-boundary.lock",
            "/workspace/guest-lock-ready",
            "/workspace/guest-lock-release",
        ])
        .spawn()
        .unwrap();
    wait_for_path(&guest_ready);
    let blocked = Command::new(workspace.join("fs-probe"))
        .args([
            "try-exclusive",
            workspace.join("cross-boundary.lock").to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(
        blocked.code(),
        Some(10),
        "guest exclusive create did not block host"
    );
    fs::write(&guest_release, "release").unwrap();
    assert!(guest_holder.wait().unwrap().success());
    Command::new(workspace.join("fs-probe"))
        .args([
            "try-exclusive",
            workspace.join("cross-boundary.lock").to_str().unwrap(),
        ])
        .assert_success();

    let host_ready = workspace.join("host-lock-ready");
    let host_release = workspace.join("host-lock-release");
    let mut host_holder = Command::new(workspace.join("fs-probe"))
        .args([
            "hold-exclusive",
            workspace.join("cross-boundary.lock").to_str().unwrap(),
            host_ready.to_str().unwrap(),
            host_release.to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    wait_for_path(&host_ready);
    let blocked = command(home, source)
        .args([
            "exec",
            name,
            "--",
            "/workspace/fs-probe",
            "try-exclusive",
            "/workspace/cross-boundary.lock",
        ])
        .output()
        .unwrap();
    assert_eq!(
        blocked.status.code(),
        Some(10),
        "host exclusive create did not block guest"
    );
    fs::write(&host_release, "release").unwrap();
    assert!(host_holder.wait().unwrap().success());
    command(home, source)
        .args([
            "exec",
            name,
            "--",
            "/workspace/fs-probe",
            "try-exclusive",
            "/workspace/cross-boundary.lock",
        ])
        .assert_success();

    // BoxLite 0.9.7's virtiofs path does not propagate advisory locks across
    // the host/guest boundary. Keep this negative qualification live so the
    // documented limitation cannot silently become an overclaim.
    for mechanism in ["flock", "fcntl"] {
        let ready = workspace.join(format!("guest-{mechanism}-ready"));
        let release = workspace.join(format!("guest-{mechanism}-release"));
        let lock = workspace.join(format!("advisory-{mechanism}.lock"));
        let mut guest_holder = command(home, source)
            .args([
                "exec",
                name,
                "--",
                "/workspace/fs-probe",
                &format!("hold-{mechanism}"),
                &format!("/workspace/advisory-{mechanism}.lock"),
                &format!("/workspace/guest-{mechanism}-ready"),
                &format!("/workspace/guest-{mechanism}-release"),
            ])
            .spawn()
            .unwrap();
        wait_for_path(&ready);
        Command::new(workspace.join("fs-probe"))
            .args([
                format!("try-{mechanism}"),
                lock.to_string_lossy().into_owned(),
            ])
            .assert_success();
        fs::write(release, "release").unwrap();
        assert!(guest_holder.wait().unwrap().success());
    }
}

struct WorkspaceCleanup {
    home: std::path::PathBuf,
    source: std::path::PathBuf,
}

impl Drop for WorkspaceCleanup {
    fn drop(&mut self) {
        for name in [
            "demo",
            "jjdemo",
            "crash-commit",
            "crash-temp-create",
            "crash-temp-write",
            "crash-journal",
            "crash-intent",
            "crash-provider",
            "crash-start",
            "crash-clean-file",
            "crash-clean-temp",
            "crash-clean-dir",
        ] {
            if self.home.join("state").join(name).exists() {
                let _ = command(&self.home, &self.source)
                    .args(["remove", name])
                    .status();
            }
        }
    }
}

#[cfg(unix)]
fn assert_sigkill(output: Output) {
    use std::os::unix::process::ExitStatusExt;

    assert_eq!(
        output.status.signal(),
        Some(libc::SIGKILL),
        "expected injected SIGKILL, got {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn crash_injection_expected() -> bool {
    std::env::var_os("PANDO_TEST_CRASH_INJECTION").as_deref() != Some(std::ffi::OsStr::new("0"))
}

fn assert_provisional_create_recovers(
    home: &Path,
    source: &Path,
    name: &str,
    point: &str,
    expected_boxes: usize,
) {
    let mut create = command(home, source);
    create.env("PANDO_TEST_CRASH_POINT", point).args([
        "create",
        name,
        "--runtime",
        "boxlite",
        "--image",
        "alpine:3.22",
    ]);
    common::add_qualified_runtime_cli_args(&mut create);
    let interrupted = create.output().unwrap();
    assert_sigkill(interrupted);
    let boxes_root = home.join("runtime/boxlite/boxes");
    let boxes_before_recovery = box_directories(&boxes_root);
    let listed = command(home, source).args(["list"]).assert_success();
    assert!(!String::from_utf8_lossy(&listed.stdout).contains(name));
    assert!(!home.join("state").join(name).exists());
    assert!(!home.join("workspaces").join(name).exists());
    assert!(!home.join("transactions").join(name).exists());
    assert_eq!(fs::read_dir(&boxes_root).unwrap().count(), expected_boxes);
    let boxes_after_recovery = box_directories(&boxes_root);
    for removed in boxes_before_recovery.difference(&boxes_after_recovery) {
        assert_no_owned_processes(removed);
    }
    assert_not_mounted(&home.join("workspaces").join(name));
}

fn assert_committed_create_recovers(home: &Path, source: &Path, name: &str, point: &str) {
    let mut create = command(home, source);
    create.env("PANDO_TEST_CRASH_POINT", point).args([
        "create",
        name,
        "--runtime",
        "boxlite",
        "--image",
        "alpine:3.22",
    ]);
    common::add_qualified_runtime_cli_args(&mut create);
    let interrupted = create.output().unwrap();
    assert_sigkill(interrupted);
    let listed = command(home, source).args(["list"]).assert_success();
    assert!(String::from_utf8_lossy(&listed.stdout).contains(name));
    assert!(home.join("state").join(name).join("meta.toml").exists());
    assert!(!home.join("transactions").join(name).exists());
    let info = command(home, source)
        .args(["info", name, "--json"])
        .assert_success();
    let info: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
    let box_home = home
        .join("runtime/boxlite/boxes")
        .join(info["runtime"]["provider_id"].as_str().unwrap());
    command(home, source)
        .args(["remove", name])
        .assert_success();
    assert_no_owned_processes(&box_home);
    assert_not_mounted(&home.join("workspaces").join(name));
}

fn box_directories(root: &Path) -> BTreeSet<std::path::PathBuf> {
    fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect()
}

#[cfg(target_os = "linux")]
fn assert_not_mounted(path: &Path) {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").unwrap();
    let escaped = path.to_string_lossy().replace(' ', "\\040");
    assert!(
        !mountinfo
            .lines()
            .any(|line| line.split_whitespace().nth(4) == Some(escaped.as_str())),
        "mount still exists at {}",
        path.display()
    );
}

#[cfg(not(target_os = "linux"))]
fn assert_not_mounted(_path: &Path) {}

#[cfg(target_os = "linux")]
fn assert_no_owned_processes(box_home: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let processes = owned_processes(box_home);
        if processes.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "BoxLite processes still reference {} after removal: {processes:?}",
            box_home.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(not(target_os = "linux"))]
fn assert_no_owned_processes(_box_home: &Path) {}

#[cfg(target_os = "linux")]
fn owned_processes(box_home: &Path) -> Vec<i32> {
    use std::os::unix::ffi::OsStrExt;

    let shim = box_home.join("bin/boxlite-shim");
    let shared = box_home.join("shared");
    fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse().ok()?;
            let command_line = fs::read(entry.path().join("cmdline")).ok()?;
            let arguments: Vec<_> = command_line
                .split(|byte| *byte == 0)
                .filter(|argument| !argument.is_empty())
                .collect();
            let executable = arguments.first()?;
            let exact_shim = shim.as_os_str().as_bytes();
            let exact_shared = shared.as_os_str().as_bytes();
            let associated = *executable == exact_shim
                || ((executable.ends_with(b"/bwrap") || *executable == b"bwrap")
                    && arguments.windows(3).any(|triple| {
                        triple[0] == b"--bind"
                            && triple[1] == exact_shared
                            && triple[2] == exact_shared
                    })
                    && arguments.last() == Some(&exact_shim));
            associated.then_some(pid)
        })
        .collect()
}

#[cfg(unix)]
fn run_in_terminal(command: &mut Command, input: &[u8], send_interrupt: bool) -> String {
    use std::os::unix::process::CommandExt;
    let mut master = 0;
    let mut slave = 0;
    let size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty initializes both descriptors and only reads the supplied winsize.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        },
        0
    );
    // SAFETY: openpty returned owned, valid file descriptors.
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    // SAFETY: openpty returned an owned, valid file descriptor.
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    command
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave);
    // SAFETY: this hook runs after fork and before exec; fd 0 is the PTY slave installed above.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpgrp()) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    master.write_all(input).unwrap();
    if send_interrupt {
        std::thread::sleep(Duration::from_secs(1));
        master.write_all(&[0x03]).unwrap();
    }

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "terminal child failed: {status:?}");
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("terminal child did not exit within 15 seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // BoxLite descendants may retain the PTY slave briefly after the client exits, so drain
    // already-buffered output without waiting for an EOF that is unrelated to Pando's lifetime.
    // SAFETY: master is a valid descriptor and F_GETFL/F_SETFL do not outlive it.
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    // SAFETY: master remains valid for this call.
    assert_eq!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let mut output = Vec::new();
    loop {
        let mut buffer = [0_u8; 4096];
        match master.read(&mut buffer) {
            Ok(0) => break,
            Ok(length) => output.extend_from_slice(&buffer[..length]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::UnexpectedEof
                ) =>
            {
                break;
            }
            // Linux PTY masters report EIO once every slave descriptor is closed.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => panic!("could not read terminal output: {error}"),
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn command(home: &Path, current_dir: &Path) -> Command {
    let binary =
        std::env::var_os("PANDO_TEST_BINARY").unwrap_or_else(|| env!("CARGO_BIN_EXE_pando").into());
    let mut command = Command::new(binary);
    let home_argument = if home.parent() == current_dir.parent() {
        Path::new("..").join(home.file_name().unwrap())
    } else {
        home.to_owned()
    };
    command
        .env("PANDO_HOME", home_argument)
        .current_dir(current_dir);
    command
}

fn write_qualified_runtime_config(home: &Path, image: &str, cpus: u8, memory_mib: u32) {
    fs::create_dir_all(home).unwrap();
    let mut config = format!(
        "[runtime]\nruntime = \"boxlite\"\nimage = {image:?}\ncpus = {cpus}\nmemory_mib = {memory_mib}\n"
    );
    #[cfg(target_os = "linux")]
    config.push_str("allow_unqualified_seccomp = true\n");
    fs::write(home.join("config.toml"), config).unwrap();
}

trait CommandResult {
    fn assert_success(&mut self) -> Output;
}

impl CommandResult for Command {
    fn assert_success(&mut self) -> Output {
        let command = format!("{self:?}");
        let output = self.output().unwrap();
        assert!(
            output.status.success(),
            "command failed with {:?}: {command}\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}
