//! Frontend-evaluated flags, kept in memory for this app session.

use crate::account::ActiveAccount;
use crate::tools::auth::keychain_secrets;
use crate::tools::connectors::{load_mcp_servers, McpConnector, McpServerConfig};
use crate::tools::registry::ToolRegistry;
use serde::{Deserialize, Serialize};
use std::sync::{LazyLock, RwLock};
use tauri::{AppHandle, Manager, State, Webview};

// Shared with the catalog so registration and preset visibility use the same list.
static EXPERIMENTAL_CONNECTOR_PRESETS: LazyLock<Vec<String>> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../webview/shared/experimental-connector-presets.json"
    ))
    .expect("experimental connector presets must be a JSON array of server IDs")
});

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureFlags {
    #[serde(default)]
    pub experimental_connectors: bool,
}

#[derive(Default)]
pub struct DesktopFeatureFlags(RwLock<FeatureFlags>);

impl DesktopFeatureFlags {
    pub fn get(&self) -> FeatureFlags {
        *self.0.read().expect("feature flags lock poisoned")
    }
}

/// Keep experimental preset filtering separate from registration.
pub fn filter_mcp_presets(
    app_handle: &AppHandle,
    mut servers: Vec<McpServerConfig>,
) -> Vec<McpServerConfig> {
    let experimental_enabled = app_handle
        .state::<DesktopFeatureFlags>()
        .get()
        .experimental_connectors;
    servers.retain(|server| {
        experimental_enabled || !EXPERIMENTAL_CONNECTOR_PRESETS.contains(&server.id)
    });
    servers
}

#[tauri::command]
pub async fn set_desktop_feature_flags(
    flags: FeatureFlags,
    webview: Webview,
    app_handle: AppHandle,
    state: State<'_, DesktopFeatureFlags>,
) -> Result<(), String> {
    if webview.label() != crate::main_window::LABEL {
        return Err("only the main webview may report feature flags".into());
    }
    if state.get() == flags {
        return Ok(());
    }

    let active = app_handle.state::<ActiveAccount>();
    let account = active.get();
    // Read configuration before committing the flag, so a load failure can be retried.
    let experimental_configs = match account.as_ref().filter(|_| flags.experimental_connectors) {
        Some(account) => load_mcp_servers(&app_handle, account)?
            .into_iter()
            .filter(|server| EXPERIMENTAL_CONNECTOR_PRESETS.contains(&server.id))
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    *state.0.write().map_err(|_| "feature flags lock poisoned")? = flags;

    let registry = app_handle.state::<ToolRegistry>();
    if flags.experimental_connectors {
        if let Some(account) = account {
            for config in experimental_configs {
                let connector = McpConnector::from_config(
                    config,
                    keychain_secrets(account.clone()),
                    &app_handle,
                    &account,
                )
                .await;
                // Discard credentials loaded for an account that has since changed.
                if active.get().as_ref() != Some(&account) {
                    break;
                }
                registry.replace(connector).await;
            }
        }
    } else {
        for server_id in EXPERIMENTAL_CONNECTOR_PRESETS.iter() {
            registry
                .retire(&crate::tools::connectors::mcp::connector_id(server_id))
                .await;
        }
    }
    crate::tools::emit_connectors_changed(&app_handle);
    Ok(())
}

#[tauri::command]
pub fn get_desktop_feature_flags(state: State<'_, DesktopFeatureFlags>) -> FeatureFlags {
    state.get()
}
