use crate::account::AccountId;
use crate::reporting::config_failure;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tauri::AppHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicy {
    #[default]
    AutoApprove,
    Ask,
    Disable,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectorState {
    #[serde(default)]
    pub disabled_connectors: HashSet<String>,
    /// connector_id -> tool_name -> policy
    #[serde(default)]
    pub tool_policies: HashMap<String, HashMap<String, ToolPolicy>>,
    /// connector_id -> tool_name -> [`fingerprint`](super::registry::ToolDefinition::fingerprint)
    /// of an MCP tool as it was when the user chose to auto-approve it. The choice lapses back
    /// to asking when the server changes what it says the tool does.
    #[serde(default)]
    pub approved_tools: HashMap<String, HashMap<String, String>>,
    /// connector_id -> tool_name -> fingerprint (empty for built-in tools): tools the user lets
    /// run even after the conversation has read untrusted content, scheduled runs included.
    /// Like an approval, it lapses when an MCP server changes the tool.
    #[serde(default)]
    pub always_allowed: HashMap<String, HashMap<String, String>>,
    /// Connectors that have reached their remote at least once.
    ///
    /// Outlives the app, unlike the per-run activity: it separates a connector that is
    /// temporarily broken from one that has never functioned.
    #[serde(default)]
    pub succeeded_once: HashSet<String>,
    /// Conversations that have read private data or untrusted content, by conversation id.
    /// Kept so a conversation resumed after a restart is still gated; see `tools::exposure`.
    #[serde(default)]
    pub exposed_conversations: super::exposure::Conversations,
}

/// A saved tool policy, validated against the tool's current fingerprint.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Choice {
    /// No saved policy, or an auto-approval whose pinned fingerprint no longer matches.
    Unset,
    Stands(ToolPolicy),
    /// An auto-approval with no pinned fingerprint. The caller must pin it to this fingerprint
    /// and persist the state; otherwise the approval would follow later changes to the tool.
    Unpinned(ToolPolicy, String),
}

impl ConnectorState {
    /// Stores every choice explicitly, AutoApprove included: an absent entry means the tool's
    /// declared default, which may be Ask. `fingerprint` pins an auto-approval and is `None` for
    /// tools whose definition cannot change. Withdraws always-allow.
    pub(super) fn choose_policy(
        &mut self,
        connector_id: &str,
        tool_name: &str,
        policy: ToolPolicy,
        fingerprint: Option<String>,
    ) {
        self.tool_policies
            .entry(connector_id.to_string())
            .or_default()
            .insert(tool_name.to_string(), policy);
        let pins = self
            .approved_tools
            .entry(connector_id.to_string())
            .or_default();
        match fingerprint.filter(|_| policy == ToolPolicy::AutoApprove) {
            Some(fingerprint) => pins.insert(tool_name.to_string(), fingerprint),
            None => pins.remove(tool_name),
        };
        if let Some(tools) = self.always_allowed.get_mut(connector_id) {
            tools.remove(tool_name);
        }
    }

    pub(super) fn always_allow(
        &mut self,
        connector_id: &str,
        tool_name: &str,
        fingerprint: Option<String>,
    ) {
        self.choose_policy(
            connector_id,
            tool_name,
            ToolPolicy::AutoApprove,
            fingerprint.clone(),
        );
        self.always_allowed
            .entry(connector_id.to_string())
            .or_default()
            .insert(tool_name.to_string(), fingerprint.unwrap_or_default());
    }

    pub(super) fn is_always_allowed(
        &self,
        connector_id: &str,
        tool_name: &str,
        fingerprint: impl FnOnce() -> Option<String>,
    ) -> bool {
        let Some(pinned) = self
            .always_allowed
            .get(connector_id)
            .and_then(|tools| tools.get(tool_name))
        else {
            return false;
        };
        fingerprint().is_none_or(|current| current == *pinned)
    }

    pub(super) fn tool_choice(
        &self,
        connector_id: &str,
        tool_name: &str,
        fingerprint: impl FnOnce() -> Option<String>,
    ) -> Choice {
        let Some(saved) = self
            .tool_policies
            .get(connector_id)
            .and_then(|tools| tools.get(tool_name))
            .copied()
        else {
            return Choice::Unset;
        };
        if saved != ToolPolicy::AutoApprove {
            return Choice::Stands(saved);
        }
        let Some(current) = fingerprint() else {
            return Choice::Stands(saved);
        };
        let pinned = self
            .approved_tools
            .get(connector_id)
            .and_then(|tools| tools.get(tool_name));
        match pinned {
            Some(pinned) if *pinned != current => Choice::Unset,
            Some(_) => Choice::Stands(saved),
            None => Choice::Unpinned(saved, current),
        }
    }

    pub(super) fn pin_approval(
        &mut self,
        connector_id: &str,
        tool_name: &str,
        fingerprint: String,
    ) {
        self.approved_tools
            .entry(connector_id.to_string())
            .or_default()
            .insert(tool_name.to_string(), fingerprint);
    }

    pub(super) fn forget_tool_choices(&mut self, connector_id: &str) {
        self.tool_policies.remove(connector_id);
        self.approved_tools.remove(connector_id);
        self.always_allowed.remove(connector_id);
    }
}

pub fn connector_state_path(
    app_handle: &AppHandle,
    account: &AccountId,
) -> Result<std::path::PathBuf, String> {
    crate::settings::account_config_dir(app_handle, account)
        .map(|dir| dir.join("connector_state.json"))
}

pub fn load_connector_state(
    app_handle: &AppHandle,
    account: &AccountId,
) -> Result<ConnectorState, String> {
    let path = connector_state_path(app_handle, account)?;
    if !path.exists() {
        return Ok(ConnectorState::default());
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content)
        .inspect_err(|_| config_failure("Connector state load failed"))
        .map_err(|e| e.to_string())
}

pub fn save_connector_state(
    app_handle: &AppHandle,
    account: &AccountId,
    state: &ConnectorState,
) -> Result<(), String> {
    let path = connector_state_path(app_handle, account)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = serde_json::to_string_pretty(state)
        .inspect_err(|_| config_failure("Connector state save failed"))
        .map_err(|e| e.to_string())?;
    std::fs::write(path, content).map_err(|e| e.to_string())
}
