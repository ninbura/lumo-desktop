//! macOS checks privacy grants (microphone, camera, protected folders) against the app
//! responsible for a process, and a child is the responsibility of whoever started it: every
//! tool command would record through Lumo's microphone grant without a prompt. A child disclaims
//! that by being spawned with `responsibility_spawnattrs_setdisclaim`, which only `posix_spawn`
//! honours, while `Command` forks. So tools start as Lumo itself with `FLAG`, and that process
//! replaces itself with the tool, disclaimed, before anything else runs. A tool judged alone
//! holds no grants at all, so where the helper is installed it runs in between; see `helper`.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub(super) const FLAG: &str = "--lumo-exec-disclaimed";

static TRAMPOLINE: OnceLock<PathBuf> = OnceLock::new();

extern "C" {
    fn responsibility_spawnattrs_setdisclaim(
        attrs: *mut libc::posix_spawnattr_t,
        disclaim: bool,
    ) -> libc::c_int;
}

/// Becomes the requested tool if this process is a trampoline; otherwise routes later tool
/// processes through this executable.
pub(super) fn start() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(OsStr::new(FLAG)) {
        let argv: Vec<OsString> = args.collect();
        let error = exec_disclaimed(&argv);
        eprintln!(
            "lumo: cannot run {:?}: {error}",
            argv.first().unwrap_or(&OsString::new())
        );
        std::process::exit(127);
    }
    if let Ok(executable) = std::env::current_exe() {
        let _ = TRAMPOLINE.set(executable);
    }
}

pub(super) fn trampoline() -> Option<&'static Path> {
    TRAMPOLINE.get().map(PathBuf::as_path)
}

/// Replaces this process with `argv`, keeping its pid, process group, working directory and
/// environment, and returns only if that fails. Signals go back to their defaults, since the
/// Rust runtime ignores `SIGPIPE` and the tool would inherit that.
fn exec_disclaimed(argv: &[OsString]) -> io::Error {
    let Ok(argv) = argv
        .iter()
        .map(|arg| CString::new(arg.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
    else {
        return io::Error::new(io::ErrorKind::InvalidInput, "argument contains NUL");
    };
    let Some(program) = argv.first() else {
        return io::Error::new(io::ErrorKind::InvalidInput, "no program given");
    };
    let pointers: Vec<*mut libc::c_char> = argv
        .iter()
        .map(|arg| arg.as_ptr().cast_mut())
        .chain([std::ptr::null_mut()])
        .collect();
    // SAFETY: `attr` is initialized before use and destroyed after; the argument pointers
    // outlive the call and end in NULL, as does the process's own `environ`.
    unsafe {
        let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
        let result = libc::posix_spawnattr_init(&mut attr);
        if result != 0 {
            return io::Error::from_raw_os_error(result);
        }
        let mut every_signal: libc::sigset_t = 0;
        let mut no_signal: libc::sigset_t = 0;
        libc::sigfillset(&mut every_signal);
        libc::sigemptyset(&mut no_signal);
        let flags =
            libc::POSIX_SPAWN_SETEXEC | libc::POSIX_SPAWN_SETSIGDEF | libc::POSIX_SPAWN_SETSIGMASK;
        let result = [
            libc::posix_spawnattr_setsigdefault(&mut attr, &every_signal),
            libc::posix_spawnattr_setsigmask(&mut attr, &no_signal),
            libc::posix_spawnattr_setflags(&mut attr, flags as libc::c_short),
            responsibility_spawnattrs_setdisclaim(&mut attr, true),
        ]
        .into_iter()
        .find(|&result| result != 0)
        .unwrap_or_else(|| {
            let mut pid = 0;
            libc::posix_spawnp(
                &mut pid,
                program.as_ptr(),
                std::ptr::null(),
                &attr,
                pointers.as_ptr(),
                *libc::_NSGetEnviron(),
            )
        });
        libc::posix_spawnattr_destroy(&mut attr);
        io::Error::from_raw_os_error(result)
    }
}
