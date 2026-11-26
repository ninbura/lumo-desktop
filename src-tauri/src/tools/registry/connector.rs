//! The traits a connector implements, and the shapes the registry and frontend exchange.

use super::super::auth::{
    Activity, AuthMechanism, ConnectorStatus, Interactive, Readiness, Stored,
};
use super::super::connector_state::{ConnectorState, ToolPolicy};
use super::super::error::{AuthError, ToolError};
use super::super::exposure::{ApprovalReason, RequestUrl, ToolRisk};
#[cfg(doc)]
use super::ToolRegistry;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The user's answer to an approval request. A timeout or a dismissed card denies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Deny,
    Once,
    /// Approve, and auto-approve this tool from now on. Untrusted content still makes it ask.
    Always,
}

#[async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn request(
        &self,
        connector_id: &str,
        tool_name: &str,
        input: &Value,
        reason: Option<&ApprovalReason>,
        conversation: Option<&str>,
    ) -> ApprovalDecision;
}

/// Where the registry persists state, and who it tells when a connector's health moves.
pub trait RegistryHost: Send + Sync {
    fn persist(&self, state: &ConnectorState);

    /// Announces that one connector's reported state changed.
    ///
    /// Called on a transition only, not once per successful call.
    fn notify_changed(&self, connector_id: &str);
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    /// Only explicit true permits this tool in a read-only scheduled run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    /// The tool reaches beyond the service it belongs to (MCP `openWorldHint`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world: Option<bool>,
    #[serde(default)]
    pub policy: ToolPolicy,
}

impl ToolDefinition {
    /// Hash of what the model and the gate are told about the tool, the policy aside.
    pub(crate) fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        fn sorted(value: &Value) -> Value {
            match value {
                Value::Object(map) => {
                    let mut entries: Vec<_> = map.iter().collect();
                    entries.sort_by_key(|(key, _)| *key);
                    Value::Object(
                        entries
                            .into_iter()
                            .map(|(key, item)| (key.clone(), sorted(item)))
                            .collect(),
                    )
                }
                Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
                other => other.clone(),
            }
        }
        let described = serde_json::json!([
            self.title,
            self.description,
            sorted(&self.input_schema),
            self.read_only,
            self.open_world,
        ]);
        format!("{:x}", Sha256::digest(described.to_string().as_bytes()))
    }
}

/// Which optional capabilities a connector has, fixed at construction.
///
/// Carried in the manifest so the frontend can gate its buttons on them.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Capabilities {
    pub credentials: bool,
    pub session: bool,
}

impl Capabilities {
    pub fn has_remote(&self) -> bool {
        self.credentials || self.session
    }
}

/// The unchanging half of a connector's identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorDescriptor {
    pub id: String,
    pub display_name: String,
    /// Optional public capability summary, available before tool discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub version: String,
    pub icon: Option<String>,
    pub auth: AuthMechanism,
}

/// A connector as the frontend sees it.
///
/// A display snapshot, assembled only by [`ToolRegistry::manifest_of`]. Nothing dispatches or
/// gates on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorManifest {
    pub id: String,
    pub display_name: String,
    /// Optional public capability summary, available before tool discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub version: String,
    pub icon: Option<String>,
    pub auth: AuthMechanism,
    pub capabilities: Capabilities,
    pub tools: Vec<ToolDefinition>,
    /// Tools the user lets run even after the conversation has read untrusted content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub always_allowed: Vec<String>,
    pub enabled: bool,
    pub readiness: Readiness,
    /// Absent for a connector with no remote.
    pub activity: Option<Activity>,
    pub status: ConnectorStatus,
}

#[async_trait]
pub trait Connector: Send + Sync {
    fn id(&self) -> &str;
    fn descriptor(&self) -> ConnectorDescriptor;
    fn tools(&self) -> Vec<ToolDefinition>;

    /// Whether a call is worth attempting.
    ///
    /// Answered for every connector on every settings render, so it must not touch the network.
    fn readiness(&self) -> Readiness;

    /// Whether the app vouches for what this connector says about its tools: their read-only and
    /// open-world hints and their descriptions. An MCP server added by hand could say anything.
    fn vetted(&self) -> bool {
        true
    }

    /// Inputs to the [`exposure`](super::super::exposure) gate for a tool. The default treats an
    /// unclassified tool as an outside service that can send data.
    fn risk(&self, _tool_name: &str) -> ToolRisk {
        ToolRisk::default()
    }

    /// The URL a call requests, for connectors that fetch pages.
    fn request_url(&self, _tool_name: &str, _input: &Value) -> Option<RequestUrl> {
        None
    }

    async fn invoke(&self, tool_name: &str, input: Value) -> Result<Value, ToolError>;

    fn check_read_only(&self, tool_name: &str) -> Result<(), ToolError> {
        if self
            .tools()
            .iter()
            .any(|tool| tool.name == tool_name && tool.read_only == Some(true))
        {
            Ok(())
        } else {
            Err(ToolError::AccessDenied(
                "Tool is not permitted by this scheduled task's read-only access".into(),
            ))
        }
    }

    /// Session-backed connectors must check again after refreshing their tool catalog.
    async fn invoke_read_only(&self, tool_name: &str, input: Value) -> Result<Value, ToolError> {
        self.check_read_only(tool_name)?;
        self.invoke(tool_name, input).await
    }

    fn as_credentialed(&self) -> Option<&dyn Credentialed> {
        None
    }

    fn as_session(&self) -> Option<&dyn Session> {
        None
    }
}

/// A connector holding durable authorisation that only a person can establish.
#[async_trait]
pub trait Credentialed: Send + Sync {
    /// Leaves the connector holding credentials it believes are usable.
    ///
    /// With input, adopts what the user supplied. Without input and with credentials already
    /// stored, confirms them and never prompts. Without input and with none stored, acquires
    /// them, which requires [`Interactive::Yes`].
    ///
    /// Reports whether it wrote a credential that has not yet been proven.
    async fn authenticate(
        &self,
        input: Option<Value>,
        mode: Interactive,
    ) -> Result<Stored, AuthError>;

    /// Erases the stored credentials
    async fn forget(&self) -> Result<(), String>;
}

/// A connector holding a live channel that dies without warning and is rebuilt in code.
#[async_trait]
pub trait Session: Send + Sync {
    /// Establishes a session if none is live.
    ///
    /// After `Ok(())` a session exists. Idempotent and single-flight.
    async fn ensure(&self) -> Result<(), ToolError>;

    /// Closes the transport and keeps the credentials.
    ///
    /// The next `ensure` rebuilds.
    async fn drop_session(&self);
}
