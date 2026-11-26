use crate::tools::auth::{AuthMechanism, Readiness};
use crate::tools::connector_state::ToolPolicy;
use crate::tools::error::ToolError;
use crate::tools::exposure::{Exposure, ToolRisk};
use crate::tools::registry::{Connector, ConnectorDescriptor, ToolDefinition, ToolRegistry};
use async_trait::async_trait;
use log::info;
use serde_json::{json, Value};

pub const CONNECTOR_ID: &str = "echo";
pub const AUTO_TOOL: &str = "auto";
pub const ASK_TOOL: &str = "ask";

/// Deterministic connector for the bridge E2E harness: both tools return their
/// input, `ask` routes through the approval gate.
pub struct EchoConnector;

fn echo_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "message": { "type": "string" } },
    })
}

#[async_trait]
impl Connector for EchoConnector {
    fn id(&self) -> &str {
        CONNECTOR_ID
    }

    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor {
            id: CONNECTOR_ID.to_string(),
            display_name: "Echo (E2E)".to_string(),
            description: None,
            version: "1.0.0".to_string(),
            icon: None,
            auth: AuthMechanism::None,
        }
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: AUTO_TOOL.to_string(),
                title: None,
                description: "Echo the input back without asking for approval.".to_string(),
                input_schema: echo_schema(),
                read_only: Some(true),
                open_world: None,
                policy: ToolPolicy::AutoApprove,
            },
            ToolDefinition {
                name: ASK_TOOL.to_string(),
                title: None,
                description: "Echo the input back after the user approves.".to_string(),
                input_schema: echo_schema(),
                read_only: Some(true),
                open_world: None,
                policy: ToolPolicy::Ask,
            },
        ]
    }

    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    fn risk(&self, _tool_name: &str) -> ToolRisk {
        ToolRisk {
            exposes: Exposure::NONE,
            sends_data: false,
            changes_files: false,
        }
    }

    async fn invoke(&self, tool_name: &str, input: Value) -> Result<Value, ToolError> {
        match tool_name {
            AUTO_TOOL | ASK_TOOL => Ok(json!({ "echo": input })),
            other => Err(ToolError::UnknownTool(other.to_string())),
        }
    }
}

/// The `policy` in `tools()` above is not what `dispatch` enforces — it reads
/// `ConnectorState` instead. `ask` reaches the approval gate only because
/// `e2e/support/fixtures.js` writes that policy into the state file loaded at startup.
pub fn register(registry: &ToolRegistry) {
    registry.register(EchoConnector);
    info!("registered echo connector: echo__auto, echo__ask");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::{ApprovalDecision, ApprovalGate};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct FixedAnswerGate {
        answer: bool,
        calls: AtomicUsize,
    }

    impl FixedAnswerGate {
        fn new(answer: bool) -> Arc<Self> {
            Arc::new(Self {
                answer,
                calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl ApprovalGate for FixedAnswerGate {
        async fn request(
            &self,
            _connector_id: &str,
            _tool_name: &str,
            _input: &Value,
            _reason: Option<&crate::tools::exposure::ApprovalReason>,
            _conversation: Option<&str>,
        ) -> ApprovalDecision {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.answer {
                ApprovalDecision::Once
            } else {
                ApprovalDecision::Deny
            }
        }
    }

    fn registry_with_gate(gate: Arc<FixedAnswerGate>) -> ToolRegistry {
        let registry = ToolRegistry::new();
        register(&registry);
        registry.set_tool_policy(CONNECTOR_ID, ASK_TOOL, ToolPolicy::Ask);
        registry.set_approval_gate(gate);
        registry
    }

    #[tokio::test]
    async fn auto_tool_echoes_input_without_consulting_the_gate() {
        let gate = FixedAnswerGate::new(false);
        let registry = registry_with_gate(gate.clone());

        let result = registry
            .dispatch(CONNECTOR_ID, AUTO_TOOL, json!({ "message": "hi" }))
            .await
            .expect("auto tool should dispatch");

        assert_eq!(result, json!({ "echo": { "message": "hi" } }));
        assert_eq!(gate.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn ask_tool_echoes_input_when_the_gate_approves() {
        let gate = FixedAnswerGate::new(true);
        let registry = registry_with_gate(gate.clone());

        let result = registry
            .dispatch(CONNECTOR_ID, ASK_TOOL, json!({ "message": "hi" }))
            .await
            .expect("approved ask tool should dispatch");

        assert_eq!(result, json!({ "echo": { "message": "hi" } }));
        assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ask_tool_is_denied_when_the_gate_refuses() {
        let gate = FixedAnswerGate::new(false);
        let registry = registry_with_gate(gate.clone());

        let error = registry
            .dispatch(CONNECTOR_ID, ASK_TOOL, json!({ "message": "hi" }))
            .await
            .expect_err("denied ask tool should not dispatch");

        assert!(matches!(error, ToolError::ApprovalDenied));
        assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
    }
}
