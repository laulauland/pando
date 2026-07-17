#![cfg(feature = "microvm-boxlite")]

use std::{
    fs,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires KVM/HVF, OCI registry access, and fuse-overlayfs or APFS"]
fn non_jj_workspace_runs_tools_and_preserves_isolation() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    let home = fixture.path().join("home");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("original.txt"), "canonical").unwrap();
    let _cleanup = WorkspaceCleanup {
        home: home.clone(),
        source: source.clone(),
    };

    command(&home, &source)
        .args([
            "create",
            "demo",
            "--runtime",
            "boxlite",
            "--image",
            "alpine:3.22",
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
}

struct WorkspaceCleanup {
    home: std::path::PathBuf,
    source: std::path::PathBuf,
}

impl Drop for WorkspaceCleanup {
    fn drop(&mut self) {
        if self.home.join("state/demo").exists() {
            let _ = command(&self.home, &self.source)
                .args(["remove", "demo"])
                .status();
        }
    }
}

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
    let mut command = Command::new(env!("CARGO_BIN_EXE_pando"));
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

trait CommandResult {
    fn assert_success(&mut self) -> Output;
}

impl CommandResult for Command {
    fn assert_success(&mut self) -> Output {
        let output = self.output().unwrap();
        assert!(
            output.status.success(),
            "command failed with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}
