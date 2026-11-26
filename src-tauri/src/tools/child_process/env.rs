//! Which of Lumo's environment variables a tool process may inherit.

/// Variables an allowlisted tool process inherits from Lumo. Everything else is withheld, so a
/// token exported in the user's shell profile doesn't reach every local server; a server that
/// needs one gets it through its config's `env` or `secret_env`.
const INHERITED_ENV: &[&str] = &[
    // Identity, locale and scratch space.
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "LANG",
    "LANGUAGE",
    "TZ",
    "TMPDIR",
    "__CF_USER_TEXT_ENCODING",
    // Corporate networks.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    // Servers that open a browser or use the keyring on Linux.
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
    "XDG_CURRENT_DESKTOP",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    // Version managers whose shims the resolved PATH can point at.
    "NVM_DIR",
    "VOLTA_HOME",
    "PYENV_ROOT",
    "ASDF_DIR",
    "ASDF_DATA_DIR",
    // Windows programs fail to start, or fail oddly, without these.
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "USERNAME",
    "USERDOMAIN",
    "HOMEDRIVE",
    "HOMEPATH",
    "TEMP",
    "TMP",
    "PROGRAMDATA",
    "ALLUSERSPROFILE",
    "PUBLIC",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "COMMONPROGRAMW6432",
    "COMPUTERNAME",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
];

/// Windows variable names are case-insensitive, and proxy variables are often lowercase.
fn is_inherited(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    INHERITED_ENV.contains(&upper.as_str()) || upper.starts_with("LC_")
}

pub(super) fn inherited_env(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    vars.into_iter()
        .filter(|(name, _)| name.to_str().is_some_and(is_inherited))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_servers_inherit_only_what_programs_need_to_run() {
        let vars = [
            ("HOME", "/Users/me"),
            ("LC_ALL", "en_GB.UTF-8"),
            ("https_proxy", "http://proxy:3128"),
            ("SystemRoot", "C:\\Windows"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
            ("AWS_SECRET_ACCESS_KEY", "secret"),
            ("GITHUB_TOKEN", "ghp_secret"),
            ("OPENAI_API_KEY", "sk-secret"),
            ("NPM_TOKEN", "npm_secret"),
            ("SSH_AUTH_SOCK", "/tmp/agent.sock"),
            ("PATH", "/usr/bin"),
        ]
        .map(|(name, value)| (name.into(), value.into()));

        let mut inherited: Vec<String> = inherited_env(vars)
            .into_iter()
            .map(|(name, _)| name.into_string().unwrap())
            .collect();
        inherited.sort();
        assert_eq!(
            inherited,
            [
                "DBUS_SESSION_BUS_ADDRESS",
                "HOME",
                "LC_ALL",
                "SystemRoot",
                "https_proxy"
            ],
            "PATH is replaced by the resolved search path, and credentials come only from the config"
        );
    }
}
