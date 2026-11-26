//! Path access control for tools: paths must be inside a user-granted folder and outside
//! Lumo's own data.

use crate::settings::get_config_dir;
use crate::tools::error::ToolError;
use std::path::{Component, Path, PathBuf, Prefix};
use tauri::{AppHandle, Manager};
use walkdir::{DirEntry, WalkDir};

pub(crate) struct PathGuard {
    roots: Vec<PathBuf>,
    /// Lumo's settings, credential metadata and web storage. Denied even inside a granted folder,
    /// so that tool calls cannot modify connector configuration or read the session.
    protected: Vec<PathBuf>,
}

impl PathGuard {
    pub(crate) fn new(roots: Vec<PathBuf>, protected: Vec<PathBuf>) -> Self {
        Self { roots, protected }
    }

    pub(crate) fn has_roots(&self) -> bool {
        !self.roots.is_empty()
    }

    pub(crate) fn roots_for_prompt(&self) -> Vec<String> {
        self.roots
            .iter()
            .map(|root| {
                root.canonicalize()
                    .unwrap_or_else(|_| root.clone())
                    .display()
                    .to_string()
            })
            .collect()
    }

    /// Returns the canonical path of an existing file or directory. Relative paths are resolved
    /// against the first root.
    pub(crate) fn resolve(&self, path: &str) -> Result<PathBuf, ToolError> {
        self.resolve_within_roots(path, false)
    }

    /// Like [`resolve`](Self::resolve), but the file itself may not exist yet. Its parent
    /// directory must.
    pub(crate) fn resolve_for_create(&self, path: &str) -> Result<PathBuf, ToolError> {
        self.resolve_within_roots(path, true)
    }

    /// Walks `root`, or every granted folder when `root` is `None`, skipping protected paths.
    pub(crate) fn walk(
        &self,
        root: Option<&str>,
    ) -> Result<impl Iterator<Item = DirEntry>, ToolError> {
        let roots = match root {
            Some(sub) => {
                let resolved = self.resolve(sub)?;
                if !resolved.is_dir() {
                    return Err(ToolError::InvalidInput(format!(
                        "search root is not a directory: {}",
                        resolved.display()
                    )));
                }
                vec![resolved]
            }
            None => {
                self.require_roots()?;
                // Protected paths are canonical, so the walk must start from canonical roots.
                self.roots
                    .iter()
                    .map(|root| root.canonicalize().unwrap_or_else(|_| root.clone()))
                    .collect()
            }
        };
        let protected = self.protected_paths();
        Ok(roots.into_iter().flat_map(move |root| {
            let protected = protected.clone();
            WalkDir::new(root)
                .into_iter()
                .filter_entry(move |entry| !is_protected(&protected, entry.path()))
                .filter_map(Result::ok)
        }))
    }

    fn require_roots(&self) -> Result<(), ToolError> {
        if self.roots.is_empty() {
            return Err(ToolError::AccessDenied(
                "no allowed directories configured".into(),
            ));
        }
        Ok(())
    }

    /// Resolved on each call because a directory created after startup canonicalizes differently
    /// from its configured path (on Windows it gains a `\\?\` prefix).
    fn protected_paths(&self) -> Vec<PathBuf> {
        self.protected
            .iter()
            .map(|path| resolve_through_ancestors(path))
            .collect()
    }

    fn resolve_within_roots(
        &self,
        path: &str,
        allow_missing_leaf: bool,
    ) -> Result<PathBuf, ToolError> {
        let candidate = sanitize_user_path(path)?;
        self.require_roots()?;

        let resolved = if candidate.is_absolute() {
            candidate
        } else {
            self.roots[0].join(candidate)
        };

        let leaf_is_symlink = std::fs::symlink_metadata(&resolved)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);

        let canonical = if allow_missing_leaf && !resolved.exists() && !leaf_is_symlink {
            let parent = resolved
                .parent()
                .ok_or_else(|| ToolError::InvalidInput("path has no parent directory".into()))?;
            let file_name = resolved
                .file_name()
                .ok_or_else(|| ToolError::InvalidInput("path has no file name".into()))?;
            parent
                .canonicalize()
                .map_err(|e| {
                    ToolError::InvalidInput(format!("parent directory does not exist: {e}"))
                })?
                .join(file_name)
        } else {
            resolved
                .canonicalize()
                .map_err(|e| ToolError::AccessDenied(format!("invalid path: {e}")))?
        };

        for root in &self.roots {
            let canonical_root = root
                .canonicalize()
                .map_err(|e| ToolError::AccessDenied(format!("invalid root: {e}")))?;
            if canonical.starts_with(&canonical_root) {
                if is_protected(&self.protected_paths(), &canonical) {
                    return Err(ToolError::AccessDenied(
                        "Lumo's own settings and data are not available to tools".into(),
                    ));
                }
                return Ok(canonical);
            }
        }

        Err(ToolError::AccessDenied(format!(
            "path outside allowed roots: {}",
            canonical.display()
        )))
    }
}

/// Directories where Lumo stores state, including the webview's cookies and storage.
pub(crate) fn app_directories(app_handle: &AppHandle) -> Vec<PathBuf> {
    let path = app_handle.path();
    let mut dirs: Vec<PathBuf> = [
        get_config_dir(app_handle).ok(),
        path.app_data_dir().ok(),
        path.app_local_data_dir().ok(),
        path.app_cache_dir().ok(),
        path.app_log_dir().ok(),
    ]
    .into_iter()
    .flatten()
    .collect();
    #[cfg(target_os = "macos")]
    if let Ok(home) = path.home_dir() {
        let mut names = vec![app_handle.config().identifier.clone()];
        // Unbundled builds have no identifier; WebKit names their storage after the binary.
        if let Some(binary) = std::env::current_exe().ok().and_then(|exe| {
            exe.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        }) {
            names.push(binary);
        }
        dirs.extend(macos_library_paths(&home.join("Library"), &names));
    }
    dirs
}

/// The macOS data volume, which is firmlinked into `/`. `canonicalize` does not resolve
/// firmlinks, so `/System/Volumes/Data/Users/me` and `/Users/me` are returned as different paths
/// to the same folder. Stored case-folded to match [`comparable`].
const MACOS_DATA_VOLUME: &str = "/system/volumes/data";

/// Rejects `..` and any path that is not on a local drive.
///
/// Must run before any OS call on the path. On Windows, resolving `\\host\share` opens an SMB
/// connection that sends the user's NTLM credentials, and the DNS lookup of the host name can
/// itself exfiltrate data.
pub(super) fn sanitize_user_path(path: &str) -> Result<PathBuf, ToolError> {
    let remote = || ToolError::AccessDenied("network and device paths are not allowed".into());
    let path = Path::new(path);
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(ToolError::AccessDenied("path traversal not allowed".into()))
            }
            // Windows returns canonical paths in the `\\?\C:\` form, so it must be accepted.
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(_) | Prefix::VerbatimDisk(_) => {}
                _ => return Err(remote()),
            },
            Component::RootDir | Component::Normal(_) | Component::CurDir => {}
        }
    }
    // Windows treats `\\host` and `//host` as shares. Other platforms parse them as ordinary
    // paths, so the component check above does not catch them there.
    let text = path.as_os_str().to_string_lossy();
    let is_separator = |c: char| matches!(c, '/' | '\\');
    let starts_like_a_share = text.starts_with(is_separator) && text[1..].starts_with(is_separator);
    let has_disk_prefix = matches!(path.components().next(), Some(Component::Prefix(_)));
    if starts_like_a_share && !has_disk_prefix {
        return Err(remote());
    }
    Ok(path.to_path_buf())
}

/// Canonicalizes `path`. If it does not exist, canonicalizes its nearest existing ancestor and
/// appends the remaining components, which matches how [`PathGuard::resolve_for_create`] resolves
/// a new file.
fn resolve_through_ancestors(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(resolved) = existing.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(resolved, |acc, name| acc.join(name));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Whether canonical `path` refers to one of `protected` or to something inside one.
fn is_protected(protected: &[PathBuf], path: &Path) -> bool {
    let path = comparable(path);
    protected
        .iter()
        .any(|protected| path.starts_with(comparable(protected)))
}

/// Normalizes `path` for comparison on case-insensitive filesystems (macOS and Windows).
///
/// `canonicalize` corrects the case only of components that exist. A file being created keeps
/// the case it was given, so `ME.PROTON.LUMO-APP.plist` would otherwise not match the protected
/// `me.proton.lumo-app.plist`.
fn comparable(path: &Path) -> PathBuf {
    if !cfg!(any(target_os = "macos", windows)) {
        return path.to_path_buf();
    }
    let mut folded = PathBuf::from(fold_case(&path.to_string_lossy()));
    while let Ok(rest) = folded.strip_prefix(MACOS_DATA_VOLUME) {
        folded = Path::new("/").join(rest);
    }
    folded
}

/// Lowercases `text` and applies the extra folds of a case-insensitive APFS volume (`ſ` to `s`,
/// `ß` to `ss`, ligatures to their letters). Sufficient because every protected name is ASCII.
fn fold_case(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        match c {
            'ſ' => folded.push('s'),
            'ß' => folded.push_str("ss"),
            'ﬀ' => folded.push_str("ff"),
            'ﬁ' => folded.push_str("fi"),
            'ﬂ' => folded.push_str("fl"),
            'ﬃ' => folded.push_str("ffi"),
            'ﬄ' => folded.push_str("ffl"),
            'ﬅ' | 'ﬆ' => folded.push_str("st"),
            other => folded.push(other),
        }
    }
    folded
}

/// Locations where macOS and WebKit store an app's data outside its own directories: web
/// storage, cookies, preferences and saved window state.
#[cfg(any(target_os = "macos", test))]
pub(super) fn macos_library_paths(library: &Path, names: &[String]) -> Vec<PathBuf> {
    names
        .iter()
        .flat_map(|name| {
            [
                library.join("WebKit").join(name),
                library.join("HTTPStorages").join(name),
                library
                    .join("HTTPStorages")
                    .join(format!("{name}.binarycookies")),
                library
                    .join("Cookies")
                    .join(format!("{name}.binarycookies")),
                library.join("Preferences").join(format!("{name}.plist")),
                library
                    .join("Saved Application State")
                    .join(format!("{name}.savedState")),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_folding_matches_what_apfs_treats_as_the_same_name() {
        assert_eq!(fold_case("ME.PROTON.LUMO-APP"), "me.proton.lumo-app");
        assert_eq!(fold_case("\u{212A}ookie"), "kookie");
        assert_eq!(fold_case("cookieſ"), "cookies");
        assert_eq!(fold_case("ẞ ß"), "ss ss");
        assert_eq!(fold_case("ﬁle ﬆate"), "file state");
    }

    #[test]
    fn local_paths_still_pass_the_path_check() {
        assert!(sanitize_user_path("notes/today.txt").is_ok());
        assert!(sanitize_user_path("/Users/me/notes.txt").is_ok());
        if cfg!(windows) {
            assert!(sanitize_user_path(r"C:\Users\me\notes.txt").is_ok());
            assert!(sanitize_user_path(r"\\?\C:\Users\me\notes.txt").is_ok());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_data_volume_alias_names_the_same_protected_file() {
        let protected = [PathBuf::from("/Users/me/Library/Preferences/app.plist")];
        for alias in [
            "/System/Volumes/Data/Users/me/Library/Preferences/app.plist",
            "/system/volumes/DATA/Users/me/Library/Preferences/app.plist",
            "/System/Volumes/Data/System/Volumes/Data/Users/me/Library/Preferences/app.plist",
        ] {
            assert!(is_protected(&protected, Path::new(alias)), "{alias}");
        }
        assert!(!is_protected(
            &protected,
            Path::new("/System/Volumes/Data/Users/me/Library/Preferences/other.plist")
        ));
        assert!(!is_protected(
            &protected,
            Path::new("/System/Volumes/Database/Users/me/Library/Preferences/app.plist")
        ));
    }
}
