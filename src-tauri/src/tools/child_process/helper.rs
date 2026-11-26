//! The helper app that tool processes run under, so that macOS attributes a tool to "Lumo
//! Tools" and the user can grant it folders separately from Lumo.
//!
//! It ships inside Lumo's bundle but runs from a copy in the data directory: macOS attributes
//! code located under `Lumo.app` to Lumo, microphone grant included. Tools can write to the data
//! directory, so the copy is checked against the shipped helper before every use.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::{fs, io};
use walkdir::WalkDir;

const BUNDLE: &str = "Lumo Tools.app";
const EXECUTABLE: &str = "Contents/MacOS/lumo-tools";

static HELPER: OnceLock<Helper> = OnceLock::new();

struct Helper {
    shipped: PathBuf,
    installed: PathBuf,
}

/// Keeps the helper's copy in `data_dir`. Does nothing in an unbundled build.
pub(super) fn install(data_dir: &Path) -> io::Result<()> {
    let executable = std::env::current_exe()?;
    let shipped = executable
        .parent()
        .and_then(Path::parent)
        .map(|contents| contents.join("Helpers").join(BUNDLE));
    let Some(shipped) = shipped.filter(|shipped| shipped.is_dir()) else {
        return Ok(());
    };
    let helper = Helper {
        shipped,
        installed: data_dir.join(BUNDLE),
    };
    helper.executable()?;
    let _ = HELPER.set(helper);
    Ok(())
}

/// The helper to run a tool under, or `None` when there is no copy identical to the shipped one.
pub(super) fn executable() -> Option<PathBuf> {
    HELPER
        .get()?
        .executable()
        .inspect_err(|error| log::warn!("tools helper not usable: {:?}", error.kind()))
        .ok()
}

impl Helper {
    /// Restores the copy first if it differs from the shipped helper in any way.
    fn executable(&self) -> io::Result<PathBuf> {
        static RESTORING: Mutex<()> = Mutex::new(());
        let _restoring = RESTORING.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.is_intact() {
            self.restore()?;
        }
        Ok(self.installed.join(EXECUTABLE))
    }

    fn is_intact(&self) -> bool {
        let (Ok(shipped), Ok(installed)) = (entries(&self.shipped), entries(&self.installed))
        else {
            return false;
        };
        shipped == installed
            && shipped.iter().all(|(file, is_file)| {
                !is_file
                    || matches!(
                        (fs::read(self.shipped.join(file)), fs::read(self.installed.join(file))),
                        (Ok(shipped), Ok(installed)) if shipped == installed
                    )
            })
    }

    fn restore(&self) -> io::Result<()> {
        match fs::symlink_metadata(&self.installed) {
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(&self.installed)?,
            Ok(_) => fs::remove_file(&self.installed)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        for (path, is_file) in entries(&self.shipped)? {
            if is_file {
                fs::copy(self.shipped.join(&path), self.installed.join(&path))?;
            } else {
                fs::create_dir_all(self.installed.join(&path))?;
            }
        }
        Ok(())
    }
}

/// Every path under `root`, relative to it, with whether it is a regular file. Links are not
/// followed, so one never passes for the file or directory it points to.
fn entries(root: &Path) -> io::Result<Vec<(PathBuf, bool)>> {
    WalkDir::new(root)
        .follow_root_links(false)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| {
            let entry = entry?;
            let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
            if !entry.file_type().is_file() && !entry.file_type().is_dir() {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            Ok((relative.to_path_buf(), entry.file_type().is_file()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn helper(dir: &Path) -> Helper {
        let shipped = dir.join("shipped");
        fs::create_dir_all(shipped.join("Contents/MacOS")).unwrap();
        fs::write(shipped.join(EXECUTABLE), "shipped").unwrap();
        Helper {
            shipped,
            installed: dir.join("data/installed"),
        }
    }

    #[test]
    fn a_missing_or_altered_copy_is_restored_before_it_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let helper = helper(dir.path());

        let executable = helper.executable().unwrap();
        assert_eq!(fs::read(&executable).unwrap(), b"shipped");

        fs::write(&executable, "replaced").unwrap();
        assert_eq!(fs::read(helper.executable().unwrap()).unwrap(), b"shipped");

        let added = helper.installed.join("Contents/MacOS/added");
        fs::write(&added, "added").unwrap();
        helper.executable().unwrap();
        assert!(!added.exists());

        fs::rename(&helper.installed, dir.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &helper.installed).unwrap();
        helper.executable().unwrap();
        assert!(!helper.installed.is_symlink());

        fs::remove_dir_all(&helper.installed).unwrap();
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &helper.installed).unwrap();
        assert_eq!(fs::read(helper.executable().unwrap()).unwrap(), b"shipped");

        fs::remove_dir_all(&helper.installed).unwrap();
        fs::write(&helper.installed, "a file").unwrap();
        assert_eq!(fs::read(helper.executable().unwrap()).unwrap(), b"shipped");
    }

    #[test]
    fn a_copy_that_cannot_be_restored_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let helper = helper(dir.path());
        let executable = helper.executable().unwrap();
        fs::write(executable, "replaced").unwrap();

        let data = dir.path().join("data");
        fs::set_permissions(&data, fs::Permissions::from_mode(0o555)).unwrap();
        let used = helper.executable();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(used.is_err());
    }
}
