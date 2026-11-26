//! Tools start through Lumo's own executable so that macOS doesn't hold Lumo responsible for
//! them, and so doesn't let them use its microphone and folder permissions.
#![cfg(target_os = "macos")]

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const LUMO: &str = env!("CARGO_BIN_EXE_lumo-desktop");
const FLAG: &str = "--lumo-exec-disclaimed";

extern "C" {
    fn responsibility_get_pid_responsible_for_pid(pid: libc::pid_t) -> libc::pid_t;
}

/// The pid of the shell `command` starts, and the pid macOS holds responsible for it.
fn responsibility(command: &mut Command) -> (libc::pid_t, libc::pid_t) {
    let mut child = command
        .args(["-c", "echo ready; exec sleep 10"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    let pid = child.id() as libc::pid_t;
    // SAFETY: a plain query by pid.
    let responsible = unsafe { responsibility_get_pid_responsible_for_pid(pid) };
    child.kill().unwrap();
    child.wait().unwrap();
    (pid, responsible)
}

#[test]
fn a_tool_started_through_lumo_is_responsible_for_itself() {
    let (pid, responsible) = responsibility(Command::new(LUMO).args([FLAG, "/bin/sh"]));
    assert_eq!(responsible, pid);

    let (pid, responsible) = responsibility(&mut Command::new("/bin/sh"));
    assert_ne!(
        responsible, pid,
        "a child started directly is its parent's responsibility"
    );
}

#[test]
fn the_tool_keeps_its_environment_and_gets_default_signal_handling() {
    // `yes` dies of SIGPIPE (status 141) only if the trampoline didn't pass on Rust's SIG_IGN.
    let output = Command::new(LUMO)
        .args([FLAG, "/bin/bash", "-c"])
        .arg(r#"yes | head -1 >/dev/null; echo "${PIPESTATUS[0]} $GIVEN $(pwd -P)""#)
        .env("GIVEN", "kept")
        .current_dir("/private/tmp")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "141 kept /private/tmp"
    );
}

#[test]
fn a_missing_tool_fails_like_a_shell_would() {
    let output = Command::new(LUMO)
        .args([FLAG, "/nonexistent/tool"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&output.stderr).contains("/nonexistent/tool"));
}

/// The tools helper, built from its own crate.
fn helper() -> &'static Path {
    static HELPER: OnceLock<PathBuf> = OnceLock::new();
    HELPER.get_or_init(|| {
        let helper_crate = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools-helper");
        let built = Command::new(env!("CARGO"))
            .args(["build", "--release", "--locked", "--manifest-path"])
            .arg(helper_crate.join("Cargo.toml"))
            .status()
            .unwrap();
        assert!(built.success());
        helper_crate.join("target/release/lumo-tools")
    })
}

fn is_running(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn a_tool_run_under_the_helper_is_the_helpers_responsibility_and_stops_with_its_process_group() {
    let mut helper = Command::new(LUMO)
        .arg(FLAG)
        .arg(helper())
        .args(["/bin/sh", "-c", "echo $$; exec sleep 30"])
        .current_dir("/")
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(helper.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    let tool: libc::pid_t = line.trim().parse().unwrap();
    let helper_pid = helper.id() as libc::pid_t;
    assert_ne!(
        tool, helper_pid,
        "the helper stays, with the tool as its child"
    );

    // SAFETY: plain queries by pid.
    let responsible = |pid| unsafe { responsibility_get_pid_responsible_for_pid(pid) };
    assert_eq!(responsible(tool), helper_pid);
    assert_eq!(responsible(helper_pid), helper_pid);

    // SAFETY: signals the group this test created.
    unsafe { libc::killpg(helper_pid, libc::SIGKILL) };
    helper.wait().unwrap();
    for _ in 0..50 {
        if !is_running(tool) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!is_running(tool), "the tool outlived its process group");
}

#[test]
fn a_tool_run_under_the_helper_keeps_its_environment_and_reports_its_own_exit_status() {
    let output = Command::new(LUMO)
        .arg(FLAG)
        .arg(helper())
        .args(["/bin/bash", "-c"])
        .arg(r#"yes | head -1 >/dev/null; echo "${PIPESTATUS[0]} $GIVEN $(pwd -P)"; exit 7"#)
        .env("GIVEN", "kept")
        .current_dir("/private/tmp")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "141 kept /private/tmp"
    );
    assert_eq!(output.status.code(), Some(7));

    let missing = Command::new(LUMO)
        .arg(FLAG)
        .arg(helper())
        .arg("/nonexistent/tool")
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(127));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("/nonexistent/tool"));
}
