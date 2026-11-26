use super::auth::{active_secret_store, Interactive, SecureStorageStatus};
use super::connector_state::{save_connector_state, ToolPolicy};
use super::connectors::mcp::{connector_id, on_config_change, on_remove, validate_server_id};
use super::connectors::{load_mcp_servers, save_mcp_servers, McpConnector, McpServerConfig};
use super::registry::{
    ApprovalDecision, CallContext, ConnectorManifest, ToolDefinition, ToolRegistry,
};
use super::{
    emit_connector_changed, emit_connector_lifecycle, emit_connectors_changed, ConnectorKind,
    ConnectorLifecycle, ConnectorLifecycleChange, MCP_CONNECTOR_PREFIX,
};
use crate::account::{record_request_account, AccountId, ActiveAccount};
use crate::schedules::{bridge::ScheduleToolContext, coordinator::ScheduleCoordinator};
use log::{debug, info, warn};
use serde_json::Value;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

const TOOL_RESULT_LOG_LIMIT: usize = 4_000;

/// Whether connecting or disconnecting a catalog connector added or removed it, judged by whether
/// it had ever connected before and after the call. Repeat connects ("Save and test") and
/// disconnecting one that never connected report nothing. MCP servers are counted when they are
/// saved and removed instead, since their connect also runs for "Refresh tools".
fn catalog_lifecycle(
    connector_id: &str,
    was_connected: bool,
    is_connected: bool,
) -> Option<ConnectorLifecycleChange> {
    if connector_id.starts_with(MCP_CONNECTOR_PREFIX) {
        return None;
    }
    let action = match (was_connected, is_connected) {
        (false, true) => ConnectorLifecycle::Added,
        (true, false) => ConnectorLifecycle::Removed,
        _ => return None,
    };
    Some(ConnectorLifecycleChange {
        action,
        kind: ConnectorKind::Catalog,
    })
}

/// Settings marks the servers it saves or removes from the catalog's featured list (Slack, Google
/// Drive, …) as `featured`; those count as catalog connectors, everything else as a custom server.
fn mcp_lifecycle(action: ConnectorLifecycle, featured: bool) -> ConnectorLifecycleChange {
    ConnectorLifecycleChange {
        action,
        kind: if featured {
            ConnectorKind::Catalog
        } else {
            ConnectorKind::Mcp
        },
    }
}

/// Whether two saved configurations point at the same server in the same way.
///
/// Display-name and description edits keep history; changing where or how it connects does not.
fn same_transport(before: &McpServerConfig, after: &McpServerConfig) -> bool {
    serde_json::to_value(&before.transport).ok() == serde_json::to_value(&after.transport).ok()
}

fn require_account(active: &ActiveAccount) -> Result<AccountId, String> {
    active
        .get()
        .ok_or_else(|| "no account is signed in".to_string())
}

fn truncate_for_log(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    format!(
        "{}… [truncated, {} bytes total]",
        &text[..text.floor_char_boundary(max_len)],
        text.len()
    )
}

fn log_tool_result(connector_id: &str, tool_name: &str, result: &Value) {
    if matches!(
        result.get("type").and_then(Value::as_str),
        Some("lumo_file" | "lumo_tool_result" | "lumo_file_image")
    ) || result
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("type").and_then(Value::as_str) == Some("image")
                    || block
                        .get("resource")
                        .is_some_and(|resource| resource.get("blob").is_some())
            })
        })
    {
        debug!("Multimodal tool result contents omitted from logs");
        return;
    }
    match (connector_id, tool_name) {
        ("proton_mail" | "tool_results", _) | ("filesystem", "fs_read") => {
            debug!("Private tool result omitted from logs");
        }
        ("luminance", _) => {
            debug!("Luminance result omitted from logs");
        }
        ("browser", "navigate" | "fetch") => {
            if let Some(url) = result.get("url").and_then(Value::as_str) {
                debug!("page url: {url}");
            }
            if let Some(status) = result.get("status") {
                debug!("HTTP status: {status}");
            }
            if let Some(title) = result.get("title").and_then(Value::as_str) {
                debug!("page title: {title}");
            }
            if let Some(content) = result.get("content") {
                match content {
                    Value::String(text) => {
                        debug!(
                            "page content ({} bytes):\n{}",
                            text.len(),
                            truncate_for_log(text, TOOL_RESULT_LOG_LIMIT)
                        );
                    }
                    other => {
                        let rendered = other.to_string();
                        debug!(
                            "page content (json, {} bytes):\n{}",
                            rendered.len(),
                            truncate_for_log(&rendered, TOOL_RESULT_LOG_LIMIT)
                        );
                    }
                }
            }
            if let Some(links) = result.get("links").and_then(Value::as_array) {
                debug!("outbound links: {}", links.len());
            }
        }
        ("bash", "run") => {
            if let Some(workdir) = result.get("workdir").and_then(Value::as_str) {
                debug!("shell workdir: {workdir}");
            }
            if let Some(exit_code) = result.get("exit_code") {
                debug!("shell exit code: {exit_code}");
            }
            if let Some(stdout) = result.get("stdout").and_then(Value::as_str) {
                debug!(
                    "shell stdout ({} bytes):\n{}",
                    stdout.len(),
                    truncate_for_log(stdout, TOOL_RESULT_LOG_LIMIT)
                );
            }
            if let Some(stderr) = result.get("stderr").and_then(Value::as_str) {
                if !stderr.is_empty() {
                    debug!(
                        "shell stderr ({} bytes):\n{}",
                        stderr.len(),
                        truncate_for_log(stderr, TOOL_RESULT_LOG_LIMIT)
                    );
                }
            }
        }
        ("filesystem", "fs_write" | "fs_edit") => {
            let rendered = result.to_string();
            debug!(
                "file edit result ({} bytes): {}",
                rendered.len(),
                truncate_for_log(&rendered, TOOL_RESULT_LOG_LIMIT)
            );
        }
        _ => {
            let rendered = result.to_string();
            debug!(
                "invoke_tool result ({} bytes): {}",
                rendered.len(),
                truncate_for_log(&rendered, TOOL_RESULT_LOG_LIMIT)
            );
        }
    }
}

#[tauri::command]
pub fn is_secure_storage_available(status: State<'_, SecureStorageStatus>) -> bool {
    status.available
}

#[tauri::command]
pub async fn get_tool_registry(
    user_id: Option<String>,
    app_handle: AppHandle,
    registry: State<'_, ToolRegistry>,
) -> Result<Vec<ConnectorManifest>, String> {
    record_request_account(&app_handle, user_id.as_deref()).await?;
    Ok(registry.all_manifests())
}

/// Harness tools are separate from user-managed connector manifests.
#[tauri::command]
pub async fn get_builtin_tools(
    user_id: Option<String>,
    app_handle: AppHandle,
    registry: State<'_, ToolRegistry>,
) -> Result<Vec<ToolDefinition>, String> {
    record_request_account(&app_handle, user_id.as_deref()).await?;
    Ok(registry.builtin_tools())
}

#[tauri::command]
pub async fn set_connector_enabled(
    connector_id: String,
    enabled: bool,
    user_id: Option<String>,
    active: State<'_, ActiveAccount>,
    registry: State<'_, ToolRegistry>,
    app_handle: AppHandle,
) -> Result<(), String> {
    record_request_account(&app_handle, user_id.as_deref()).await?;
    registry.set_connector_enabled(&connector_id, enabled);
    save_connector_state(
        &app_handle,
        &require_account(&active)?,
        &registry.state_snapshot(),
    )?;
    emit_connectors_changed(&app_handle);
    Ok(())
}

#[tauri::command]
pub fn respond_tool_approval(request_id: String, approved: bool, always: Option<bool>) {
    let decision = match (approved, always.unwrap_or(false)) {
        (false, _) => ApprovalDecision::Deny,
        (true, false) => ApprovalDecision::Once,
        (true, true) => ApprovalDecision::Always,
    };
    super::approvals::respond(&request_id, decision);
}

#[tauri::command]
pub async fn set_tool_policy(
    connector_id: String,
    tool_name: String,
    policy: ToolPolicy,
    user_id: Option<String>,
    active: State<'_, ActiveAccount>,
    registry: State<'_, ToolRegistry>,
    app_handle: AppHandle,
) -> Result<(), String> {
    record_request_account(&app_handle, user_id.as_deref()).await?;
    registry.set_tool_policy(&connector_id, &tool_name, policy);
    save_connector_state(
        &app_handle,
        &require_account(&active)?,
        &registry.state_snapshot(),
    )?;
    emit_connectors_changed(&app_handle);
    Ok(())
}

/// Settings' "Always allow": the tool runs without asking, even after the conversation, or a
/// scheduled run, has read untrusted content.
#[tauri::command]
pub async fn always_allow_tool(
    connector_id: String,
    tool_name: String,
    user_id: Option<String>,
    webview: tauri::Webview,
    active: State<'_, ActiveAccount>,
    registry: State<'_, ToolRegistry>,
    app_handle: AppHandle,
) -> Result<(), String> {
    if webview.label() != crate::settings::window::LABEL {
        return Err("tools can only be always allowed from Settings".into());
    }
    record_request_account(&app_handle, user_id.as_deref()).await?;
    registry.always_allow(&connector_id, &tool_name);
    save_connector_state(
        &app_handle,
        &require_account(&active)?,
        &registry.state_snapshot(),
    )?;
    emit_connectors_changed(&app_handle);
    Ok(())
}

/// Register permissions supplied by the signed-in schedule executor for one unsettled run.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Parameter names are the IPC contract with the web app.
pub async fn register_schedule_execution_context(
    run_id: String,
    connector_ids: Vec<String>,
    auto_approve: bool,
    read_only_connector_ids: Option<Vec<String>>,
    user_id: String,
    webview: tauri::Webview,
    app_handle: AppHandle,
    coordinator: State<'_, ScheduleCoordinator>,
) -> Result<ScheduleToolContext, String> {
    if webview.label() != crate::main_window::LABEL {
        return Err("schedule execution requires the main webview".into());
    }
    record_request_account(&app_handle, Some(&user_id)).await?;
    coordinator
        .register_execution_context(
            run_id,
            connector_ids,
            auto_approve,
            read_only_connector_ids.unwrap_or_default(),
        )
        .await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // Parameter names are the IPC contract with the web app.
pub async fn invoke_tool(
    connector_id: String,
    tool_name: String,
    input: Value,
    schedule_context: Option<ScheduleToolContext>,
    file_transport: Option<bool>,
    conversation_id: Option<String>,
    webview: tauri::Webview,
    coordinator: State<'_, ScheduleCoordinator>,
    user_id: Option<String>,
    app_handle: AppHandle,
    registry: State<'_, ToolRegistry>,
) -> Result<Value, String> {
    record_request_account(&app_handle, user_id.as_deref()).await?;
    let run_id = schedule_context
        .as_ref()
        .map(|context| context.run_id.clone());
    let authorization = match schedule_context {
        Some(context) => {
            if webview.label() != crate::main_window::LABEL {
                return Err("schedule execution requires the main webview".into());
            }
            let authorization_connector = if connector_id == "tool_results"
                && matches!(tool_name.as_str(), "read" | "complete_file")
            {
                registry
                    .result_store
                    .source_connector(registry.result_store.scope(), &input)
                    .map_err(|e| e.to_ipc_string())?
            } else {
                connector_id.clone()
            };
            coordinator
                .authorize_tool(context, authorization_connector)
                .await?
        }
        None => crate::schedules::bridge::ToolAuthorization {
            auto_approve: false,
            read_only: false,
            connector_ids: None,
        },
    };
    info!("invoke_tool connector={connector_id} tool={tool_name}");
    if matches!(
        connector_id.as_str(),
        "proton_mail" | "filesystem" | "tool_results" | "luminance"
    ) {
        debug!("invoke_tool input=[omitted]");
    } else {
        debug!("invoke_tool input={input}");
    }
    let result = registry
        .dispatch_with_transport(
            &connector_id,
            &tool_name,
            input,
            authorization.auto_approve,
            authorization.read_only,
            file_transport.unwrap_or(false),
            CallContext {
                run: run_id.as_deref(),
                conversation: conversation_id.as_deref(),
                offered: authorization.connector_ids.as_deref(),
            },
        )
        .await
        .map_err(|e| {
            warn!(
                "invoke_tool {connector_id}.{tool_name} failed: {}",
                e.kind()
            );
            e.to_ipc_string()
        });
    if let Ok(ref value) = result {
        debug!("invoke_tool succeeded");
        if file_transport.unwrap_or(false) {
            if let Some(inner) = value.get("value") {
                log_tool_result(&connector_id, &tool_name, inner);
            }
        } else {
            log_tool_result(&connector_id, &tool_name, value);
        }
    }
    result
}

#[tauri::command]
pub async fn connect_connector(
    connector_id: String,
    credentials: Option<Value>,
    attempt_id: Option<String>,
    user_id: Option<String>,
    registry: State<'_, ToolRegistry>,
    app_handle: AppHandle,
) -> Result<(), String> {
    record_request_account(&app_handle, user_id.as_deref()).await?;
    let was_connected = registry.has_ever_succeeded(&connector_id);
    registry
        .connect_attempt(
            &connector_id,
            credentials,
            Interactive::Yes,
            attempt_id.as_deref(),
        )
        .await
        .map_err(|e| {
            warn!("connect_connector {connector_id} failed: {}", e.kind());
            e.to_ipc_string()
        })?;

    emit_connector_changed(&app_handle, &connector_id);
    if let Some(change) = catalog_lifecycle(
        &connector_id,
        was_connected,
        registry.has_ever_succeeded(&connector_id),
    ) {
        emit_connector_lifecycle(&app_handle, change);
    }
    Ok(())
}

#[tauri::command]
pub async fn cancel_connector_connection(
    connector_id: String,
    attempt_id: String,
    user_id: Option<String>,
    registry: State<'_, ToolRegistry>,
    app_handle: AppHandle,
) -> Result<(), String> {
    record_request_account(&app_handle, user_id.as_deref()).await?;
    registry.cancel_connection(&connector_id, &attempt_id);
    Ok(())
}

#[tauri::command]
pub async fn disconnect_connector(
    connector_id: String,
    user_id: Option<String>,
    registry: State<'_, ToolRegistry>,
    app_handle: AppHandle,
) -> Result<(), String> {
    record_request_account(&app_handle, user_id.as_deref()).await?;
    let was_connected = registry.has_ever_succeeded(&connector_id);
    registry
        .disconnect(&connector_id)
        .await
        .map_err(|e| e.to_ipc_string())?;
    emit_connector_changed(&app_handle, &connector_id);
    if let Some(change) = catalog_lifecycle(
        &connector_id,
        was_connected,
        registry.has_ever_succeeded(&connector_id),
    ) {
        emit_connector_lifecycle(&app_handle, change);
    }
    Ok(())
}

#[tauri::command]
pub async fn save_mcp_server(
    server: McpServerConfig,
    featured: Option<bool>,
    app_handle: AppHandle,
    active: State<'_, ActiveAccount>,
    registry: State<'_, ToolRegistry>,
) -> Result<(), String> {
    validate_server_id(&server.id)?;
    let connector_id = connector_id(&server.id);
    let _operation = registry
        .try_connector_operation(&connector_id)
        .map_err(|e| e.to_ipc_string())?;
    let account = require_account(&active)?;
    let mut servers = super::connectors::load_mcp_servers(&app_handle, &account)?;
    let existing = servers.iter().position(|s| s.id == server.id);
    let previous = existing.map(|index| &servers[index]);
    let transport_changed = previous.is_none_or(|previous| !same_transport(previous, &server));

    // Must precede the save.
    let secrets = active_secret_store(&active)?;
    on_config_change(secrets.as_ref(), previous, &server).await?;

    match existing {
        Some(index) => servers[index] = server.clone(),
        None => servers.push(server.clone()),
    }
    save_mcp_servers(&app_handle, &account, &servers)?;

    if transport_changed {
        registry.forget_success_history(&connector_id);
    }
    registry
        .replace(
            McpConnector::from_config(server, active_secret_store(&active)?, &app_handle, &account)
                .await,
        )
        .await;
    if existing.is_none() {
        emit_connector_lifecycle(
            &app_handle,
            mcp_lifecycle(ConnectorLifecycle::Added, featured.unwrap_or(false)),
        );
    }
    Ok(())
}

#[tauri::command]
pub fn list_mcp_servers(
    app_handle: AppHandle,
    active: State<'_, ActiveAccount>,
) -> Result<Vec<McpServerConfig>, String> {
    load_mcp_servers(&app_handle, &require_account(&active)?)
}

#[tauri::command]
pub async fn remove_mcp_server(
    server_id: String,
    featured: Option<bool>,
    app_handle: AppHandle,
    active: State<'_, ActiveAccount>,
    registry: State<'_, ToolRegistry>,
) -> Result<(), String> {
    let connector_id = connector_id(&server_id);
    let _operation = registry
        .try_connector_operation(&connector_id)
        .map_err(|e| e.to_ipc_string())?;
    let account = require_account(&active)?;
    let mut servers = load_mcp_servers(&app_handle, &account)?;
    let removed: Vec<&McpServerConfig> = servers.iter().filter(|s| s.id == server_id).collect();

    // Must precede the save.
    let secrets = active_secret_store(&active)?;
    let was_listed = !removed.is_empty();
    on_remove(secrets.as_ref(), &server_id, &removed).await?;

    servers.retain(|s| s.id != server_id);
    save_mcp_servers(&app_handle, &account, &servers)?;

    let _ = registry.disconnect_locked(&connector_id).await;
    registry.retire(&connector_id).await;
    if let Ok(path) = super::connectors::mcp::mcp_tools_path(&app_handle, &account, &server_id) {
        let _ = std::fs::remove_file(path);
    }

    // Prune persisted disabled connector flag and per-tool policies so when a server is
    // later re-added it doesn't silently come back disabled or with stale policies.
    registry.set_connector_enabled(&connector_id, true);
    registry.clear_tool_policies(&connector_id);
    let _ = save_connector_state(&app_handle, &account, &registry.state_snapshot());

    emit_connector_changed(&app_handle, &connector_id);
    if was_listed {
        emit_connector_lifecycle(
            &app_handle,
            mcp_lifecycle(ConnectorLifecycle::Removed, featured.unwrap_or(false)),
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn pick_directory(app: AppHandle) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Select folder")
            .blocking_pick_folder()
            .map(|path| path.to_string())
    })
    .await
    .map_err(|e| e.to_string())
}

/// Rebuilds the connectors whose capabilities come from settings, so a saved grant takes effect
/// without a restart. Called only with an account signed in.
pub async fn on_settings_saved(
    app_handle: &AppHandle,
    registry: &ToolRegistry,
    account: &AccountId,
) {
    registry
        .replace(super::connectors::FilesystemConnector::from_settings(
            app_handle, account,
        ))
        .await;
    registry
        .replace(super::connectors::BashConnector::from_settings(
            app_handle, account,
        ))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::connectors::mcp::McpTransport;

    #[test]
    fn only_a_first_connect_or_a_real_disconnect_of_a_catalog_connector_counts() {
        let added = Some(ConnectorLifecycleChange {
            action: ConnectorLifecycle::Added,
            kind: ConnectorKind::Catalog,
        });
        let removed = Some(ConnectorLifecycleChange {
            action: ConnectorLifecycle::Removed,
            kind: ConnectorKind::Catalog,
        });

        assert_eq!(catalog_lifecycle("luminance", false, true), added);
        assert_eq!(catalog_lifecycle("luminance", true, false), removed);
        // "Save and test" on a connected connector, and a failed or repeated disconnect.
        assert_eq!(catalog_lifecycle("luminance", true, true), None);
        assert_eq!(catalog_lifecycle("luminance", false, false), None);
        // MCP servers are counted on save and removal; "Refresh tools" reconnects them.
        assert_eq!(catalog_lifecycle("mcp-github", false, true), None);
        assert_eq!(catalog_lifecycle("mcp-github", true, false), None);
    }

    #[test]
    fn lifecycle_changes_serialize_to_what_the_web_app_accepts() {
        assert_eq!(
            serde_json::to_value(mcp_lifecycle(ConnectorLifecycle::Added, false)).unwrap(),
            serde_json::json!({ "action": "added", "kind": "mcp" })
        );
        // A featured catalog server such as Slack is a catalog connector, not a custom one.
        assert_eq!(
            serde_json::to_value(mcp_lifecycle(ConnectorLifecycle::Removed, true)).unwrap(),
            serde_json::json!({ "action": "removed", "kind": "catalog" })
        );
        assert_eq!(
            serde_json::to_value(catalog_lifecycle("proton-mail", true, false).unwrap()).unwrap(),
            serde_json::json!({ "action": "removed", "kind": "catalog" })
        );
    }

    #[test]
    fn log_preview_preserves_unicode_at_the_byte_limit() {
        let text = "a".repeat(3_999) + "é🦀";
        let preview = truncate_for_log(&text, 4_000);
        assert!(preview.starts_with(&"a".repeat(3_999)));
        assert!(preview.contains("4005 bytes total"));
        assert_eq!(truncate_for_log("🦀", 3), "… [truncated, 4 bytes total]");
        assert_eq!(truncate_for_log("é", 2), "é");
    }

    fn stdio(id: &str, command: &str) -> McpServerConfig {
        McpServerConfig {
            id: id.into(),
            display_name: id.into(),
            description: None,
            transport: McpTransport::Stdio {
                command: command.into(),
                args: vec![],
                env: Default::default(),
                secret_env: vec![],
            },
        }
    }

    #[test]
    fn renaming_a_server_keeps_its_history_but_repointing_it_does_not() {
        let before = stdio("notion", "npx");
        let renamed = McpServerConfig {
            display_name: "Notion Workspace".into(),
            ..stdio("notion", "npx")
        };
        let repointed = stdio("notion", "npix");

        assert!(same_transport(&before, &renamed));
        assert!(!same_transport(&before, &repointed));
    }

    #[test]
    fn server_ids_that_escape_the_tool_cache_or_keychain_namespace_are_rejected() {
        for id in [
            "chrome-devtools",
            "alpha-vantage",
            "my_server.v2",
            "Ünïcode",
        ] {
            assert!(validate_server_id(id).is_ok(), "{id}");
        }
        for id in ["", "../../x", "a/b", "a\\b", "a:token", "a\nb"] {
            assert!(validate_server_id(id).is_err(), "{id:?}");
        }
    }
}
