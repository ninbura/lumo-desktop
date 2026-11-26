pub mod commands;
pub mod window;

use super::reporting::config_failure;
use crate::account::AccountId;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::AppHandle;
#[cfg(not(feature = "e2e-webdriver"))]
use tauri::Manager;

pub const KEYCHAIN_SERVICE: &str = "me.proton.lumo-app";

/// The IPC shape the settings page reads and writes. Persisted as [`DeviceSettings`] plus
/// [`AccountSettings`] in separate files.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AppSettings {
    pub search_shortcut: String,
    #[serde(default = "default_true")]
    pub search_shortcut_enabled: bool,
    pub launch_at_startup: bool,
    #[serde(default)]
    pub filesystem_allowed_roots: Vec<String>,
    #[serde(default)]
    pub filesystem_write_enabled: bool,
    #[serde(default)]
    pub bash_enabled: bool,
    #[serde(default = "default_true")]
    pub settings_hotkey_enabled: bool,
}

/// Global rather than per-account: the shortcut is registered before any account is known, and
/// launch-at-startup is a single OS registration.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeviceSettings {
    pub search_shortcut: String,
    #[serde(default = "default_true")]
    pub search_shortcut_enabled: bool,
    pub launch_at_startup: bool,
    #[serde(default = "default_true")]
    pub settings_hotkey_enabled: bool,
}

/// Capability grants, stored per account. A grant made under one account must not apply under
/// another.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AccountSettings {
    #[serde(default)]
    pub filesystem_allowed_roots: Vec<String>,
    #[serde(default)]
    pub filesystem_write_enabled: bool,
    #[serde(default)]
    pub bash_enabled: bool,
}

fn default_true() -> bool {
    true
}

impl Default for DeviceSettings {
    fn default() -> Self {
        Self {
            search_shortcut: "Ctrl+L".to_string(),
            search_shortcut_enabled: true,
            launch_at_startup: false,
            settings_hotkey_enabled: true,
        }
    }
}

impl AppSettings {
    pub fn merge(device: DeviceSettings, account: AccountSettings) -> Self {
        Self {
            search_shortcut: device.search_shortcut,
            search_shortcut_enabled: device.search_shortcut_enabled,
            launch_at_startup: device.launch_at_startup,
            settings_hotkey_enabled: device.settings_hotkey_enabled,
            filesystem_allowed_roots: account.filesystem_allowed_roots,
            filesystem_write_enabled: account.filesystem_write_enabled,
            bash_enabled: account.bash_enabled,
        }
    }

    pub fn split(&self) -> (DeviceSettings, AccountSettings) {
        (
            DeviceSettings {
                search_shortcut: self.search_shortcut.clone(),
                search_shortcut_enabled: self.search_shortcut_enabled,
                launch_at_startup: self.launch_at_startup,
                settings_hotkey_enabled: self.settings_hotkey_enabled,
            },
            AccountSettings {
                filesystem_allowed_roots: self.filesystem_allowed_roots.clone(),
                filesystem_write_enabled: self.filesystem_write_enabled,
                bash_enabled: self.bash_enabled,
            },
        )
    }
}

fn read_json<T: Default + for<'de> Deserialize<'de>>(path: PathBuf) -> Result<T, String> {
    if !path.exists() {
        return Ok(T::default());
    }
    let contents = fs::read_to_string(path).map_err(|e| format!("Failed to read settings: {e}"))?;
    serde_json::from_str(&contents)
        .inspect_err(|_| config_failure("Settings load failed"))
        .map_err(|e| format!("Failed to parse settings: {e}"))
}

fn write_json<T: Serialize>(path: PathBuf, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create settings directory: {e}"))?;
    }
    let contents = serde_json::to_string_pretty(value)
        .inspect_err(|_| config_failure("Settings save failed"))
        .map_err(|e| format!("Failed to serialize settings: {e}"))?;
    fs::write(path, contents).map_err(|e| format!("Failed to write settings: {e}"))
}

pub fn get_settings_path(app_handle: &AppHandle) -> Result<PathBuf, String> {
    get_config_dir(app_handle).map(|dir| dir.join("settings.json"))
}

fn account_settings_path(app_handle: &AppHandle, account: &AccountId) -> Result<PathBuf, String> {
    account_config_dir(app_handle, account).map(|dir| dir.join("settings.json"))
}

pub fn load_device_settings(app_handle: &AppHandle) -> Result<DeviceSettings, String> {
    read_json(get_settings_path(app_handle)?)
}

pub fn save_device_settings(
    app_handle: &AppHandle,
    settings: &DeviceSettings,
) -> Result<(), String> {
    write_json(get_settings_path(app_handle)?, settings)
}

pub fn load_account_settings(
    app_handle: &AppHandle,
    account: &AccountId,
) -> Result<AccountSettings, String> {
    read_json(account_settings_path(app_handle, account)?)
}

pub fn save_account_settings(
    app_handle: &AppHandle,
    account: &AccountId,
    settings: &AccountSettings,
) -> Result<(), String> {
    write_json(account_settings_path(app_handle, account)?, settings)
}

#[cfg(feature = "e2e-webdriver")]
pub fn get_config_dir(_app_handle: &AppHandle) -> Result<PathBuf, String> {
    std::env::var_os("LUMO_E2E_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .ok_or_else(|| "e2e-webdriver builds require an absolute LUMO_E2E_CONFIG_DIR".into())
}

#[cfg(not(feature = "e2e-webdriver"))]
pub fn get_config_dir(app_handle: &AppHandle) -> Result<PathBuf, String> {
    app_handle
        .path()
        .app_config_dir()
        .map_err(|e| format!("Could not find config directory: {e}"))
}

/// Uses the account id as a path segment, which [`AccountId`] validation makes safe.
pub fn account_config_dir(app_handle: &AppHandle, account: &AccountId) -> Result<PathBuf, String> {
    get_config_dir(app_handle).map(|dir| dir.join("accounts").join(account.as_str()))
}

#[cfg(test)]
mod tests {
    use super::{AccountSettings, AppSettings, DeviceSettings};

    /// A field dropped by either direction would silently reset a capability grant.
    #[test]
    fn splitting_and_merging_preserves_every_field() {
        let merged = AppSettings::merge(
            DeviceSettings {
                search_shortcut: "Cmd+K".to_string(),
                search_shortcut_enabled: false,
                launch_at_startup: true,
                settings_hotkey_enabled: false,
            },
            AccountSettings {
                filesystem_allowed_roots: vec!["/tmp/work".to_string()],
                filesystem_write_enabled: true,
                bash_enabled: true,
            },
        );

        let (device, account) = merged.split();

        assert_eq!(device.search_shortcut, "Cmd+K");
        assert!(!device.search_shortcut_enabled);
        assert!(device.launch_at_startup);
        assert!(!device.settings_hotkey_enabled);
        assert_eq!(
            account.filesystem_allowed_roots,
            vec!["/tmp/work".to_string()]
        );
        assert!(account.filesystem_write_enabled);
        assert!(account.bash_enabled);
    }
}
