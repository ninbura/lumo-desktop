//! Shared process isolation for Shell commands, skill interpreters, and stdio MCP servers.

#[cfg(target_os = "macos")]
mod disclaim;
mod env;
#[cfg(target_os = "macos")]
mod helper;

use process_wrap::tokio::CommandWrap;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
#[cfg(windows)]
use process_wrap::tokio::{CreationFlags, JobObject, KillOnDrop};
use std::ffi::OsStr;
use tokio::process::Command;
#[cfg(windows)]
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

/// Runs first at startup. On macOS, a process Lumo started to launch a tool becomes that tool
/// here, and otherwise later tool processes are set to launch that way, so that none of them
/// can use the privacy permissions the user granted Lumo.
pub(crate) fn prepare_tool_processes() {
    #[cfg(target_os = "macos")]
    disclaim::start();
}

/// What a tool process sees of Lumo's environment.
pub(super) enum Environment {
    Inherited,
    /// Only the variables programs need to run; see [`env`].
    Allowlisted,
}

/// The only way to start a tool: without Lumo's privacy permissions, in its own process tree,
/// and with the environment the caller chose. Callers add arguments, further variables, working
/// directory and stdio through `command_mut`.
pub(super) fn tool_process(program: impl AsRef<OsStr>, environment: Environment) -> CommandWrap {
    let mut command = tool_command(program);
    if let Environment::Allowlisted = environment {
        command.env_clear();
        command.envs(env::inherited_env(std::env::vars_os()));
    }
    background_command(command)
}

/// On macOS, keeps the helper that tool processes run under in `data_dir`.
pub(crate) fn install_tool_helper(data_dir: &std::path::Path) {
    #[cfg(target_os = "macos")]
    if let Err(error) = helper::install(data_dir) {
        log::warn!("tools helper not installed: {:?}", error.kind());
    }
    #[cfg(not(target_os = "macos"))]
    let _ = data_dir;
}

/// A command that runs `program` as a tool, without Lumo's privacy permissions.
fn tool_command(program: impl AsRef<OsStr>) -> Command {
    #[cfg(target_os = "macos")]
    if let Some(trampoline) = disclaim::trampoline() {
        let mut command = Command::new(trampoline);
        command.arg(disclaim::FLAG);
        if let Some(helper) = helper::executable() {
            command.arg(helper);
        }
        command.arg(program);
        return command;
    }
    Command::new(program)
}

/// Keep background tools out of a console window and manage their process trees together.
fn background_command(command: Command) -> CommandWrap {
    let mut command = CommandWrap::from(command);
    #[cfg(unix)]
    command.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    {
        command.wrap(CreationFlags(CREATE_NO_WINDOW));
        command.wrap(KillOnDrop);
        command.wrap(JobObject);
    }
    command
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::time::{sleep, timeout};

    #[tokio::test]
    async fn timing_out_output_kills_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let marker_path = marker.to_string_lossy().replace('\'', "''");
        // cmd is the parent; PowerShell is a descendant in the same Job Object.
        let mut command = Command::new("cmd.exe");
        command.args(["/C", &format!(
            "powershell.exe -NoProfile -NonInteractive -Command \"Write-Output ready; Start-Sleep -Seconds 1; [IO.File]::WriteAllText('{marker_path}', 'alive')\""
        )]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = background_command(command).spawn().unwrap();
        let mut lines = BufReader::new(child.stdout().take().unwrap()).lines();
        assert_eq!(
            timeout(Duration::from_secs(30), lines.next_line())
                .await
                .unwrap()
                .unwrap(),
            Some("ready".into())
        );
        // Dropping the output future must terminate the Job Object's descendants too.
        assert!(timeout(
            Duration::from_millis(10),
            Box::into_pin(child.wait_with_output())
        )
        .await
        .is_err());
        sleep(Duration::from_millis(1500)).await;
        assert!(!marker.exists(), "descendant survived cancellation");
    }

    #[tokio::test]
    async fn background_process_has_no_console() {
        // Check the child's actual console attachment, even when the test runner has a console.
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoProfile", "-NonInteractive", "-Command",
            r#"Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class ConsoleProbe { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'; if ([ConsoleProbe]::GetConsoleWindow() -ne [IntPtr]::Zero) { exit 1 }; Write-Output 'hidden'"#,
        ]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = background_command(command).spawn().unwrap();
        let output = Box::into_pin(child.wait_with_output()).await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hidden");
    }
}
