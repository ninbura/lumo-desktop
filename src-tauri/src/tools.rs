pub mod approvals;
pub mod auth;
pub(crate) mod child_process;
pub mod command_path;
pub mod commands;
pub mod connector_state;
pub mod connectors;
pub mod error;
mod exposure;
mod hidden_text;
pub mod icons;
mod keychain;
pub mod oauth;
mod path_guard;
pub mod registry;
pub mod result_reader;
pub mod result_store;

use crate::account::{AccountId, ActiveAccount};
use connector_state::{save_connector_state, ConnectorState};
use connectors::{
    build_mcp_connectors, BashConnector, FilesystemConnector, LuminanceConnector,
    ProtonMailConnector,
};
use error::{AuthError, ConnectError};
use log::{error, info, warn};
use registry::{Connector, RegistryHost, ToolRegistry};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const CONNECTORS_CHANGED_DEBOUNCE: Duration = Duration::from_millis(300);

static CONNECTORS_CHANGED_TOKEN: AtomicU64 = AtomicU64::new(0);

/// The sole place connectors are registered.
///
/// Connectors leave the registry before their sessions close, so nothing dispatches to the
/// outgoing account mid-shutdown. Pending approvals are denied before the incoming account's
/// connectors are registered, so an approval granted under one account cannot resolve a call
/// under another.
///
/// With no account, the filesystem and bash connectors are left unregistered as well,
/// despite storing no credentials. Their capability grants are per-account.
pub async fn swap_account(app_handle: &AppHandle, account: Option<AccountId>) {
    app_handle
        .state::<ToolRegistry>()
        .result_store
        .activate(account.is_some());
    let outgoing = {
        let registry = app_handle.state::<ToolRegistry>();
        let outgoing = registry.take_connectors();
        approvals::cancel_all();
        registry.reset_exposure();
        // A question typed under one account must never be sent under another.
        if let Some(quick_ask) = app_handle.try_state::<super::quick_ask::PendingQuickAsk>() {
            quick_ask.clear();
        }
        registry.set_state(connector_state::ConnectorState::default());
        outgoing
    };

    for connector in outgoing {
        if let Some(session) = connector.as_session() {
            session.drop_session().await;
        }
    }

    match account {
        Some(account) => {
            let proton_mail =
                ProtonMailConnector::from_account(app_handle.clone(), account.clone()).await;
            let luminance =
                LuminanceConnector::from_account(app_handle.clone(), account.clone()).await;
            let mcp = build_mcp_connectors(app_handle, &account)
                .await
                .map_err(|e| error!("failed to build MCP connectors: {e}"))
                .unwrap_or_default();
            {
                let registry = app_handle.state::<ToolRegistry>();
                registry.register(FilesystemConnector::from_settings(app_handle, &account));
                registry.register(BashConnector::from_settings(app_handle, &account));
                registry.register(proton_mail);
                registry.register(luminance);
                if crate::e2e_testing_enabled() {
                    connectors::echo::register(&registry);
                }
                match connector_state::load_connector_state(app_handle, &account) {
                    Ok(state) => registry.set_state(state),
                    Err(e) => error!("failed to load connector state: {e}"),
                }
                for connector in mcp {
                    registry.register(connector);
                }
            }
            info!("connectors now serving account {}", account.as_str());
            warm_up_all(app_handle);
        }
        None => info!("no account signed in; no connectors registered"),
    }

    emit_connectors_changed(app_handle);
}

/// Attempts every connector that has a remote, each on its own task, so an unreachable server
/// delays neither the others nor the swap that started them.
///
/// Nothing depends on this succeeding: a cold connector establishes itself on its first tool
/// call. It exists to have the session already open, and to record a failure before the user
/// reads the connector list rather than by their first tool call failing.
fn warm_up_all(app_handle: &AppHandle) {
    let storage_available = app_handle
        .try_state::<auth::SecureStorageStatus>()
        .map(|status| status.available)
        .unwrap_or(false);

    for connector in app_handle.state::<ToolRegistry>().connectors() {
        // Without secure storage a credentialed connector cannot read what it needs, but a stdio
        // MCP server that declares no secrets has nothing to read and would otherwise be left
        // looking broken.
        if !storage_available && connector.as_credentialed().is_some() {
            warn!(
                "secure storage is unavailable; not connecting {}",
                connector.id()
            );
            continue;
        }
        let handle = app_handle.clone();
        let connector_id = connector.id().to_string();
        tauri::async_runtime::spawn(async move {
            let registry = handle.state::<ToolRegistry>();
            match registry
                .connect(&connector_id, None, auth::Interactive::No)
                .await
            {
                Ok(()) => emit_connector_changed(&handle, &connector_id),
                // A connector the user has not set up, and one with nothing to connect, are both
                // ordinary outcomes of sweeping every connector at startup.
                Err(ConnectError::NothingToConnect(_))
                | Err(ConnectError::Auth(AuthError::RequiresSetup)) => {}
                Err(e) => warn!("warm-up failed for {connector_id}: {}", e.kind()),
            }
        });
    }
}

/// Persists connector health to whichever account is signed in when the write happens, and
/// announces the change to the windows showing it.
///
/// Resolved per write rather than captured, so a record landing after an account switch is
/// dropped instead of being written into the incoming account's file.
pub struct AccountStateFile {
    app_handle: AppHandle,
}

impl AccountStateFile {
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }
}

impl RegistryHost for AccountStateFile {
    fn persist(&self, state: &ConnectorState) {
        let Some(account) = self.app_handle.state::<ActiveAccount>().get() else {
            return;
        };
        if let Err(e) = save_connector_state(&self.app_handle, &account, state) {
            error!("failed to persist connector state: {e}");
        }
    }

    fn notify_changed(&self, connector_id: &str) {
        emit_connector_changed(&self.app_handle, connector_id);
    }
}

/// Announces that one connector's state moved, undebounced, so the caller that awaited the
/// change sees it immediately. Distinct from the debounced [`emit_connectors_changed`], which
/// says the set of connectors or their shared state moved.
pub fn emit_connector_changed(app_handle: &AppHandle, connector_id: &str) {
    let _ = app_handle.emit("connector:changed", connector_id.to_string());
}

/// Prefix of the registry id of every custom MCP server (`mcp-<server id>`).
pub const MCP_CONNECTOR_PREFIX: &str = "mcp-";

/// Tauri event the main window's connectors bridge turns into the web app's
/// `lumo:connector-added` / `lumo:connector-removed` DOM events, which Lumo counts in telemetry.
pub const CONNECTOR_LIFECYCLE_EVENT: &str = "telemetry:connector";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectorLifecycle {
    Added,
    Removed,
}

/// 'catalog': any connector from the Settings catalog, built in (Proton Mail, Luminance) or a
/// featured MCP server (Slack, Google Drive, …); 'mcp': a custom MCP server the user defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectorKind {
    Catalog,
    Mcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ConnectorLifecycleChange {
    pub action: ConnectorLifecycle,
    pub kind: ConnectorKind,
}

/// Reports that the user added or removed a connector. Carries only the action and the kind,
/// never the connector's id, name or configuration.
pub fn emit_connector_lifecycle(app_handle: &AppHandle, change: ConnectorLifecycleChange) {
    let _ = app_handle.emit(CONNECTOR_LIFECYCLE_EVENT, change);
}

pub fn emit_connectors_changed(app_handle: &AppHandle) {
    let token = CONNECTORS_CHANGED_TOKEN.fetch_add(1, Ordering::SeqCst) + 1;
    let app_handle = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(CONNECTORS_CHANGED_DEBOUNCE).await;
        if CONNECTORS_CHANGED_TOKEN.load(Ordering::SeqCst) == token {
            let _ = app_handle.emit("connectors:changed", ());
        }
    });
}
