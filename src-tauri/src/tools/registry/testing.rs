use super::*;
use crate::tools::auth::AuthMechanism;
use crate::tools::connectors::{bash, browser, filesystem, luminance, mcp, proton_mail};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// What a test watches after handing its connector to the registry, which owns it from then on.
#[derive(Default)]
pub struct FakeState {
    pub forgot: AtomicUsize,
    pub dropped_sessions: AtomicUsize,
    pub last_mode: Mutex<Option<Interactive>>,
}

impl FakeState {
    pub fn forget_count(&self) -> usize {
        self.forgot.load(Ordering::SeqCst)
    }

    pub fn dropped_session_count(&self) -> usize {
        self.dropped_sessions.load(Ordering::SeqCst)
    }
}

/// Classification of the built-in connector with this id, so that a fake registered under it is
/// gated the same way.
pub fn builtin_risk(
    connector_id: &str,
    tool_name: &str,
    tool: Option<&ToolDefinition>,
    vetted: bool,
) -> ToolRisk {
    match connector_id {
        "proton_mail" => proton_mail::risk(tool_name),
        "browser" => browser::risk(),
        "bash" => bash::risk(),
        "filesystem" => filesystem::risk(tool_name),
        "luminance" => luminance::risk(tool.and_then(|tool| tool.read_only) == Some(true)),
        id if id.starts_with("mcp-") => mcp::risk(tool, vetted),
        _ => ToolRisk::default(),
    }
}

pub fn builtin_request_url(
    connector_id: &str,
    tool_name: &str,
    input: &Value,
) -> Option<RequestUrl> {
    (connector_id == "browser")
        .then(|| browser::request_url(tool_name, input))
        .flatten()
}

/// A connector whose capabilities and per-call outcomes are chosen by the test.
pub struct FakeConnector {
    pub id: String,
    pub readiness: Mutex<Readiness>,
    pub credentialed: bool,
    pub session: bool,
    pub authenticate_result: Mutex<Vec<Result<Stored, AuthError>>>,
    pub auth_wait: Option<Arc<tokio::sync::Notify>>,
    pub auth_listener: Mutex<Option<tokio::net::TcpListener>>,
    pub ensure_result: Mutex<Vec<Result<(), ToolError>>>,
    pub invoke_result: Mutex<Vec<Result<Value, ToolError>>>,
    /// Listed alongside `probe`; any other name is unknown to the registry.
    pub listed: Vec<String>,
    pub vetted: bool,
    /// The policy every tool declares.
    pub declared: ToolPolicy,
    /// Appended to every tool's description, for a server that changes its tools.
    pub revision: Arc<Mutex<String>>,
    pub state: Arc<FakeState>,
}

impl FakeConnector {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            readiness: Mutex::new(Readiness::Ready),
            credentialed: false,
            session: false,
            authenticate_result: Mutex::new(Vec::new()),
            auth_wait: None,
            auth_listener: Mutex::new(None),
            ensure_result: Mutex::new(Vec::new()),
            invoke_result: Mutex::new(Vec::new()),
            listed: Vec::new(),
            vetted: true,
            declared: ToolPolicy::AutoApprove,
            revision: Arc::default(),
            state: Arc::new(FakeState::default()),
        }
    }

    pub fn state(&self) -> Arc<FakeState> {
        self.state.clone()
    }

    pub fn credentialed(mut self) -> Self {
        self.credentialed = true;
        self
    }

    pub fn with_session(mut self) -> Self {
        self.session = true;
        self
    }

    pub fn blocked(self, reason: &str) -> Self {
        *self.readiness.lock().unwrap() = Readiness::blocked(reason);
        self
    }

    pub fn authenticating(self, results: Vec<Result<Stored, AuthError>>) -> Self {
        *self.authenticate_result.lock().unwrap() = results;
        self
    }

    pub fn ensuring(self, results: Vec<Result<(), ToolError>>) -> Self {
        *self.ensure_result.lock().unwrap() = results;
        self
    }

    pub fn listing(mut self, tools: &[&str]) -> Self {
        self.listed = tools.iter().map(|name| (*name).to_string()).collect();
        self
    }

    pub fn unvetted(mut self) -> Self {
        self.vetted = false;
        self.declared = ToolPolicy::Ask;
        self
    }

    pub fn invoking(self, results: Vec<Result<Value, ToolError>>) -> Self {
        *self.invoke_result.lock().unwrap() = results;
        self
    }
}

fn next<T>(queue: &Mutex<Vec<T>>, exhausted: T) -> T {
    let mut queue = queue.lock().unwrap();
    if queue.is_empty() {
        exhausted
    } else {
        queue.remove(0)
    }
}

#[async_trait]
impl Connector for FakeConnector {
    fn id(&self) -> &str {
        &self.id
    }

    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor {
            id: self.id.clone(),
            display_name: self.id.clone(),
            description: None,
            version: "1.0.0".into(),
            icon: None,
            auth: AuthMechanism::None,
        }
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        std::iter::once("probe")
            .chain(self.listed.iter().map(String::as_str))
            .map(|name| ToolDefinition {
                name: name.into(),
                description: format!("{name}{}", self.revision.lock().unwrap()),
                policy: self.declared,
                ..Default::default()
            })
            .collect()
    }

    fn readiness(&self) -> Readiness {
        self.readiness.lock().unwrap().clone()
    }

    fn vetted(&self) -> bool {
        self.vetted
    }

    fn risk(&self, tool_name: &str) -> ToolRisk {
        let tools = self.tools();
        let tool = tools.iter().find(|tool| tool.name == tool_name);
        builtin_risk(&self.id, tool_name, tool, self.vetted)
    }

    fn request_url(&self, tool_name: &str, input: &Value) -> Option<RequestUrl> {
        builtin_request_url(&self.id, tool_name, input)
    }

    async fn invoke(&self, _tool_name: &str, _input: Value) -> Result<Value, ToolError> {
        next(&self.invoke_result, Ok(Value::Null))
    }

    fn as_credentialed(&self) -> Option<&dyn Credentialed> {
        self.credentialed.then_some(self as &dyn Credentialed)
    }

    fn as_session(&self) -> Option<&dyn Session> {
        self.session.then_some(self as &dyn Session)
    }
}

#[async_trait]
impl Credentialed for FakeConnector {
    async fn authenticate(
        &self,
        _input: Option<Value>,
        mode: Interactive,
    ) -> Result<Stored, AuthError> {
        *self
            .state
            .last_mode
            .lock()
            .map_err(|_| AuthError::Failed("test authentication state lock poisoned".into()))? =
            Some(mode);
        let listener = self
            .auth_listener
            .lock()
            .map_err(|_| AuthError::Failed("test OAuth listener lock poisoned".into()))?
            .take();
        if let Some(listener) = listener {
            super::super::oauth::wait_for_oauth_callback(listener).await?;
        }
        if let Some(wait) = &self.auth_wait {
            wait.notified().await;
        }
        next(&self.authenticate_result, Ok(Stored::Nothing))
    }

    async fn forget(&self) -> Result<(), String> {
        self.state.forgot.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[async_trait]
impl Session for FakeConnector {
    async fn ensure(&self) -> Result<(), ToolError> {
        next(&self.ensure_result, Ok(()))
    }

    async fn drop_session(&self) {
        self.state.dropped_sessions.fetch_add(1, Ordering::SeqCst);
    }
}
