//! Runs a tool as a child process so that macOS attributes the tool's privacy checks to this
//! helper's bundle instead of to Lumo.

use std::os::unix::process::ExitStatusExt;
use std::process::{exit, Command};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let Some(program) = args.next() else {
        eprintln!("lumo: no tool given");
        exit(127);
    };
    match Command::new(&program).args(args).status() {
        // Termination by signal maps to 128 + signal, following the shell convention.
        Ok(status) => exit(
            status
                .code()
                .or(status.signal().map(|signal| 128 + signal))
                .unwrap_or(1),
        ),
        Err(error) => {
            eprintln!("lumo: cannot run {program:?}: {error}");
            exit(127);
        }
    }
}
