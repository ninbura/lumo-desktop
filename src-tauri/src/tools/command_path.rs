//! Finds the executable for a stdio MCP server's configured `command`, and on unix supplies the
//! `PATH` for the shell connector.
//!
//! On unix a windowed app inherits launchd's `PATH` rather than the one the user sees in a
//! terminal, so `npx` or `uvx` resolves to nothing. The search path is therefore taken from the
//! user's login shell, probed on the first stdio spawn or shell command. On Windows the process
//! `PATH` is already the user's, but there is no executable bit and `npx` names no file — only
//! `npx.cmd` — so a bare command is matched against `PATHEXT`.

use std::path::PathBuf;

pub struct Resolved {
    pub program: PathBuf,
    pub search_path: String,
}

#[derive(Debug)]
pub enum ResolveError {
    RelativePath(String),
    NotExecutable(PathBuf),
    NotFound {
        command: String,
        searched: Vec<PathBuf>,
    },
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RelativePath(command) => write!(
                f,
                "`{command}` is a relative path, which this app cannot resolve; \
                 use an absolute path or a bare command name like `npx`"
            ),
            Self::NotExecutable(path) => {
                write!(f, "`{}` is not an executable file", path.display())
            }
            // Unix probes a shell that may have silently fallen back, so the directories tried are
            // worth listing. Windows searched the user's own `PATH`; the unobvious part is not which
            // directories, but that it was captured at launch.
            #[cfg(unix)]
            Self::NotFound { command, searched } => {
                let searched = searched
                    .iter()
                    .map(|dir| dir.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "command `{command}` not found; searched: {searched}")
            }
            #[cfg(windows)]
            Self::NotFound {
                command,
                searched: _,
            } => write!(
                f,
                "command `{command}` not found on PATH; install it, then restart Lumo \
                 so the app picks it up"
            ),
        }
    }
}

#[cfg(unix)]
pub use unix::{resolve, search_path};

#[cfg(windows)]
pub use self::windows::resolve;

#[cfg(unix)]
mod unix {
    use super::{ResolveError, Resolved};
    use process_wrap::tokio::{CommandWrap, ProcessSession};
    use std::collections::HashSet;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::OnceLock;
    use std::time::Duration;
    use tokio::process::Command;
    use tokio::sync::Mutex;

    /// Wrap the value so a startup file that prints a banner cannot be mistaken for the `PATH`.
    const BEGIN: &str = "__lumo_path_begin__";
    const END: &str = "__lumo_path_end__";
    const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

    pub async fn resolve(command: &str) -> Result<Resolved, ResolveError> {
        let (dirs, freshly_probed) = search_dirs().await;
        match resolve_in(command, &dirs) {
            Ok(program) => Ok(Resolved {
                program,
                search_path: join(&dirs),
            }),
            // The command may have been installed while the app was running, in which case the
            // cached search path predates it. Worth one more probe before reporting a miss.
            Err(ResolveError::NotFound { .. }) if !freshly_probed => {
                let dirs = refresh_search_dirs().await;
                let program = resolve_in(command, &dirs)?;
                Ok(Resolved {
                    program,
                    search_path: join(&dirs),
                })
            }
            Err(e) => Err(e),
        }
    }

    pub async fn search_path() -> String {
        join(&search_dirs().await.0)
    }

    fn resolve_in(command: &str, dirs: &[PathBuf]) -> Result<PathBuf, ResolveError> {
        if command.contains('/') {
            let path = PathBuf::from(command);
            if !path.is_absolute() {
                return Err(ResolveError::RelativePath(command.to_string()));
            }
            if !is_executable_file(&path) {
                return Err(ResolveError::NotExecutable(path));
            }
            return Ok(path);
        }

        dirs.iter()
            .map(|dir| dir.join(command))
            .find(|candidate| is_executable_file(candidate))
            .ok_or_else(|| ResolveError::NotFound {
                command: command.to_string(),
                searched: dirs.to_vec(),
            })
    }

    fn is_executable_file(path: &Path) -> bool {
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    fn join(dirs: &[PathBuf]) -> String {
        dirs.iter()
            .map(|dir| dir.to_string_lossy())
            .collect::<Vec<_>>()
            .join(":")
    }

    fn cache() -> &'static Mutex<Option<Vec<PathBuf>>> {
        static CACHE: OnceLock<Mutex<Option<Vec<PathBuf>>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(None))
    }

    /// The directories to search, and whether the shell probe ran on this call. The lock is held
    /// across the probe so concurrent spawns share one shell rather than starting one each.
    async fn search_dirs() -> (Vec<PathBuf>, bool) {
        let mut guard = cache().lock().await;
        if let Some(dirs) = guard.as_ref() {
            return (dirs.clone(), false);
        }
        let dirs = discover().await;
        *guard = Some(dirs.clone());
        (dirs, true)
    }

    async fn refresh_search_dirs() -> Vec<PathBuf> {
        let mut guard = cache().lock().await;
        let dirs = discover().await;
        *guard = Some(dirs.clone());
        dirs
    }

    async fn discover() -> Vec<PathBuf> {
        match probe_login_shell_path().await {
            Some(dirs) if !dirs.is_empty() => dirs,
            _ => fallback_dirs(),
        }
    }

    /// Runs the user's login shell to read its `PATH`, returning the search directories parsed
    /// from it, or `None` if the shell is unset or its output cannot be parsed.
    async fn probe_login_shell_path() -> Option<Vec<PathBuf>> {
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty())?;
        let script = format!("printf %s '{BEGIN}'; printf %s \"$PATH\"; printf %s '{END}'");
        let mut command = Command::new(shell);
        command
            .args(["-ilc", &script])
            // A startup file that reads stdin would otherwise hang us until the timeout.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        // Without a controlling terminal, an interactive shell cannot SIGTTIN Lumo's process group.
        let mut command = CommandWrap::from(command);
        command.wrap(ProcessSession);
        let child = command.spawn().ok()?;
        let output = tokio::time::timeout(PROBE_TIMEOUT, Box::into_pin(child.wait_with_output()))
            .await
            .ok()?
            .ok()?;
        dirs_from_probe_output(&String::from_utf8_lossy(&output.stdout))
    }

    /// `None` when either marker is missing: the shell printed something we cannot locate a `PATH`
    /// inside, and handing the child directories parsed out of a banner is worse than falling back.
    fn dirs_from_probe_output(stdout: &str) -> Option<Vec<PathBuf>> {
        let (_, rest) = stdout.split_once(BEGIN)?;
        let (path, _) = rest.split_once(END)?;
        Some(search_dirs_from_path(path))
    }

    /// Empty and relative components are dropped. An empty `PATH` entry means the working
    /// directory, which for a windowed app is `/`.
    fn search_dirs_from_path(path: &str) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        path.split(':')
            .filter(|component| !component.is_empty())
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute() && dir.is_dir())
            .filter(|dir| seen.insert(dir.clone()))
            .collect()
    }

    /// Used when the probe fails or times out
    fn fallback_dirs() -> Vec<PathBuf> {
        let mut candidates = vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ];
        // `uv`/`pipx` install to `~/.local/bin`
        if let Some(home) = std::env::var("HOME").ok().filter(|home| !home.is_empty()) {
            candidates.push(Path::new(&home).join(".local/bin"));
        }
        candidates.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from));
        candidates.retain(|dir| dir.is_dir());
        candidates
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn dir_holding_executable(label: &str, name: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("lumo-command-path-{label}"));
            std::fs::create_dir_all(&dir).expect("fixture directory should be creatable");
            let executable = dir.join(name);
            std::fs::write(&executable, "#!/bin/sh\n").expect("fixture should be writable");
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
                .expect("fixture should be markable executable");
            dir
        }

        #[test]
        fn bare_command_resolves_in_search_path_order() {
            let shim_dir = dir_holding_executable("shim", "npx");
            let system_dir = dir_holding_executable("system", "npx");

            let resolved = resolve_in("npx", &[shim_dir.clone(), system_dir])
                .expect("npx is in both directories");

            assert_eq!(
                resolved,
                shim_dir.join("npx"),
                "the shell's own ordering has to win, or a version manager's shim loses to a stale system install"
            );
        }

        #[test]
        fn path_shaped_input_must_be_absolute() {
            let dir = dir_holding_executable("absolute", "server");
            let absolute = dir.join("server");

            assert_eq!(
                resolve_in(&absolute.to_string_lossy(), &[])
                    .expect("an absolute path needs no search"),
                absolute
            );
            assert!(
                matches!(
                    resolve_in("./server", &[dir]),
                    Err(ResolveError::RelativePath(_))
                ),
                "a windowed app's working directory is `/`, so a relative path would resolve somewhere the user never meant"
            );
        }

        #[test]
        fn path_components_that_are_empty_or_relative_are_ignored() {
            let dir = dir_holding_executable("components", "npx");
            let raw = format!("{}::relative/bin", dir.display());

            assert_eq!(
                search_dirs_from_path(&raw),
                vec![dir],
                "an empty component means the working directory, which must never be searched"
            );
        }

        #[test]
        fn shell_banner_output_does_not_become_a_search_path() {
            let dir = dir_holding_executable("banner", "npx");
            let noisy = format!(
                "Now using node v22.3.0 (npm v10.8.1)\n{BEGIN}{}{END}\ndirenv: export ~PATH\n",
                dir.display()
            );

            assert_eq!(dirs_from_probe_output(&noisy), Some(vec![dir]));
            assert_eq!(
                dirs_from_probe_output("Welcome back\n/opt/homebrew/bin:/usr/bin\n"),
                None,
                "without both markers there is no PATH to read, and parsing the banner would hand the child garbage directories"
            );
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::{ResolveError, Resolved};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    /// The rest of `PATHEXT` needs a script host that `CreateProcess` cannot provide.
    const PATHEXT: [&str; 4] = [".COM", ".EXE", ".BAT", ".CMD"];

    pub async fn resolve(command: &str) -> Result<Resolved, ResolveError> {
        let dirs = search_dirs_from_path(&std::env::var("PATH").unwrap_or_default());
        let program = resolve_in(command, &dirs, &PATHEXT)?;
        Ok(Resolved {
            program,
            search_path: join(&dirs),
        })
    }

    fn resolve_in(
        command: &str,
        dirs: &[PathBuf],
        extensions: &[&str],
    ) -> Result<PathBuf, ResolveError> {
        if command.contains(['/', '\\']) {
            let path = PathBuf::from(command);
            if !path.is_absolute() {
                return Err(ResolveError::RelativePath(command.to_string()));
            }
            return first_executable(&path, extensions)
                .ok_or_else(|| ResolveError::NotExecutable(path));
        }

        // Directory order outranks extension order, as it does in Windows' own search.
        dirs.iter()
            .find_map(|dir| first_executable(&dir.join(command), extensions))
            .ok_or_else(|| ResolveError::NotFound {
                command: command.to_string(),
                searched: dirs.to_vec(),
            })
    }

    /// Without an executable bit, a runnable file is one carrying a `PATHEXT` extension. A command
    /// that names none stands for every extension in turn, which is how `npx` reaches `npx.cmd`.
    fn first_executable(base: &Path, extensions: &[&str]) -> Option<PathBuf> {
        if has_executable_extension(base, extensions) && base.is_file() {
            return Some(base.to_path_buf());
        }
        extensions.iter().find_map(|extension| {
            let mut candidate = base.as_os_str().to_os_string();
            // Appended rather than set, so a version in the name is not read as an extension.
            candidate.push(extension);
            let candidate = PathBuf::from(candidate);
            candidate.is_file().then_some(candidate)
        })
    }

    fn has_executable_extension(path: &Path, extensions: &[&str]) -> bool {
        let Some(found) = path.extension().and_then(|found| found.to_str()) else {
            return false;
        };
        extensions
            .iter()
            .any(|known| known[1..].eq_ignore_ascii_case(found))
    }

    /// Empty and relative components are dropped. An empty `PATH` entry means the working
    /// directory, which for a windowed app is not anywhere the user meant.
    fn search_dirs_from_path(path: &str) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        path.split(';')
            .map(|component| component.trim().trim_matches('"'))
            .filter(|component| !component.is_empty())
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute() && dir.is_dir())
            .filter(|dir| seen.insert(dir.clone()))
            .collect()
    }

    fn join(dirs: &[PathBuf]) -> String {
        dirs.iter()
            .map(|dir| dir.to_string_lossy())
            .collect::<Vec<_>>()
            .join(";")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn dir_holding_file(label: &str, name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("lumo-command-path-{label}"));
            std::fs::create_dir_all(&dir).expect("fixture directory should be creatable");
            std::fs::write(dir.join(name), "@echo off\n").expect("fixture should be writable");
            dir
        }

        #[test]
        fn bare_command_resolves_to_its_pathext_variant() {
            let dir = dir_holding_file("bare", "npx.CMD");

            assert_eq!(
                resolve_in("npx", &[dir.clone()], &PATHEXT).expect("npx.CMD is in the directory"),
                dir.join("npx.CMD"),
                "`npx` names no file on Windows, so a search that ignores PATHEXT finds nothing"
            );
        }

        #[test]
        fn earlier_directory_wins_over_earlier_extension() {
            let shim_dir = dir_holding_file("shim", "npx.CMD");
            let system_dir = dir_holding_file("system", "npx.EXE");

            assert_eq!(
                resolve_in("npx", &[shim_dir.clone(), system_dir], &PATHEXT)
                    .expect("npx is in both directories"),
                shim_dir.join("npx.CMD"),
                "PATH order has to win, or a version manager's shim loses to a stale system install"
            );
        }

        #[test]
        fn pathext_order_decides_within_one_directory() {
            let dir = dir_holding_file("within", "npx.CMD");
            std::fs::write(dir.join("npx.EXE"), "MZ").expect("fixture should be writable");

            assert_eq!(
                resolve_in("npx", &[dir.clone()], &PATHEXT).expect("both variants are present"),
                dir.join("npx.EXE"),
                ".EXE precedes .CMD in PATHEXT, and Windows would launch that one"
            );
        }

        #[test]
        fn path_shaped_input_must_be_absolute() {
            let dir = dir_holding_file("absolute", "server.exe");
            let absolute = dir.join("server.exe");

            assert_eq!(
                resolve_in(&absolute.to_string_lossy(), &[], &PATHEXT)
                    .expect("an absolute path needs no search"),
                absolute
            );
            assert!(
                matches!(
                    resolve_in(r".\server.exe", &[dir], &PATHEXT),
                    Err(ResolveError::RelativePath(_))
                ),
                "a windowed app's working directory is not the user's, so a relative path would resolve somewhere they never meant"
            );
        }

        #[test]
        fn path_components_that_are_empty_or_relative_are_ignored() {
            let dir = dir_holding_file("components", "npx.cmd");
            let raw = format!("\"{}\";;relative\\bin", dir.display());

            assert_eq!(
                search_dirs_from_path(&raw),
                vec![dir],
                "an empty component means the working directory, which must never be searched"
            );
        }
    }
}
