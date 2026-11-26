use log::info;
use tauri::{AppHandle, Manager, State};
#[cfg(not(feature = "e2e-webdriver"))]
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

use super::{
    load_account_settings, load_device_settings, save_account_settings, save_device_settings,
    AccountSettings, AppSettings,
};
use crate::account::ActiveAccount;
use crate::tools::commands::on_settings_saved;
use crate::tools::registry::ToolRegistry;

#[tauri::command]
pub fn get_app_settings(
    app_handle: AppHandle,
    active: State<ActiveAccount>,
) -> Result<AppSettings, String> {
    let device = load_device_settings(&app_handle)?;
    let account = match active.get() {
        Some(account) => load_account_settings(&app_handle, &account)?,
        None => AccountSettings::default(),
    };
    Ok(AppSettings::merge(device, account))
}

#[tauri::command]
pub async fn save_app_settings(
    app_handle: AppHandle,
    settings: AppSettings,
    active: State<'_, ActiveAccount>,
    registry: State<'_, ToolRegistry>,
) -> Result<(), String> {
    let new_shortcut: Shortcut = settings
        .search_shortcut
        .parse()
        .map_err(|e| format!("Invalid shortcut: {}", e))?;
    let new_active = settings.search_shortcut_enabled.then_some(new_shortcut);

    let current_settings = load_device_settings(&app_handle)?;
    let old_active = current_settings
        .search_shortcut_enabled
        .then(|| current_settings.search_shortcut.parse::<Shortcut>().ok())
        .flatten();

    // `global_shortcut()` panics when the plugin failed to load at startup (see `app.rs`).
    let shortcuts_available = app_handle
        .try_state::<tauri_plugin_global_shortcut::GlobalShortcut<tauri::Wry>>()
        .is_some();
    if old_active != new_active && new_active.is_some() && !shortcuts_available {
        return Err("Global shortcuts are not supported on this system".to_string());
    }

    if shortcuts_available && old_active != new_active {
        if let Some(old) = old_active {
            let _ = app_handle.global_shortcut().unregister(old);
            // global-hotkey 0.8 on X11 buffers UngrabKey without flushing; the second call's
            // keyboard-mapping round trip sends it.
            #[cfg(target_os = "linux")]
            let _ = app_handle.global_shortcut().unregister(old);
        }

        if let Some(new) = new_active {
            if let Err(e) = app_handle.global_shortcut().register(new) {
                if let Some(old) = old_active {
                    let _ = app_handle.global_shortcut().register(old);
                }
                return Err(format!("Failed to register new shortcut: {}", e));
            }
        }
    }

    // The e2e-webdriver build never registers the autostart plugin.
    #[cfg(not(feature = "e2e-webdriver"))]
    if current_settings.launch_at_startup != settings.launch_at_startup {
        let autostart_manager = app_handle.autolaunch();
        if settings.launch_at_startup {
            autostart_manager
                .enable()
                .map_err(|e| format!("Failed to enable autostart: {}", e))?;
        } else {
            autostart_manager
                .disable()
                .map_err(|e| format!("Failed to disable autostart: {}", e))?;
        }
    }

    let (device, account_settings) = settings.split();
    save_device_settings(&app_handle, &device)?;

    match active.get() {
        Some(account) => {
            save_account_settings(&app_handle, &account, &account_settings)?;
            on_settings_saved(&app_handle, registry.inner(), &account).await;
        }
        None => info!("signed out; kept device settings only"),
    }

    Ok(())
}
