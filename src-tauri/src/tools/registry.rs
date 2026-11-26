mod connector;
#[cfg(test)]
pub mod testing;

pub use connector::{
    ApprovalDecision, ApprovalGate, Capabilities, Connector, ConnectorDescriptor,
    ConnectorManifest, Credentialed, RegistryHost, Session, ToolDefinition,
};

use super::auth::{Activity, ConnectorStatus, Interactive, Readiness, Stored};
use super::connector_state::{Choice, ConnectorState, ToolPolicy};
use super::error::{AuthError, ConnectError, ToolError};
use super::exposure::{ApprovalReason, Exposure, Gate, RequestUrl, Scope, ToolRisk};
use super::hidden_text;
use super::result_store::MAX_PREVIEW_BYTES;
use crate::reporting::approval_failure;
use async_trait::async_trait;
use log::{error, info, warn};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

/// A registered connector, and whether the registry has since evicted it.
///
/// What the registry stores in place of the connector itself, and where eviction is enforced:
/// `invoke` returns [`ToolError::Retired`] instead of delegating, so no caller can skip the check.
pub struct ConnectorHandle {
    inner: Box<dyn Connector>,
    retired: std::sync::atomic::AtomicBool,
}

impl ConnectorHandle {
    fn new(connector: impl Connector + 'static) -> Self {
        Self {
            inner: Box::new(connector),
            retired: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn retire(&self) {
        self.retired
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn is_retired(&self) -> bool {
        self.retired.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl Connector for ConnectorHandle {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn descriptor(&self) -> ConnectorDescriptor {
        self.inner.descriptor()
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        self.inner.tools()
    }

    fn readiness(&self) -> Readiness {
        self.inner.readiness()
    }

    fn vetted(&self) -> bool {
        self.inner.vetted()
    }

    fn risk(&self, tool_name: &str) -> ToolRisk {
        self.inner.risk(tool_name)
    }

    fn request_url(&self, tool_name: &str, input: &Value) -> Option<RequestUrl> {
        self.inner.request_url(tool_name, input)
    }

    async fn invoke(&self, tool_name: &str, input: Value) -> Result<Value, ToolError> {
        if self.is_retired() {
            return Err(ToolError::Retired);
        }
        self.inner.invoke(tool_name, input).await
    }

    async fn invoke_read_only(&self, tool_name: &str, input: Value) -> Result<Value, ToolError> {
        if self.is_retired() {
            return Err(ToolError::Retired);
        }
        self.inner.invoke_read_only(tool_name, input).await
    }

    fn as_credentialed(&self) -> Option<&dyn Credentialed> {
        self.inner.as_credentialed()
    }

    fn as_session(&self) -> Option<&dyn Session> {
        self.inner.as_session()
    }
}

#[derive(Default)]
struct ConnectorOperation {
    busy: Arc<tokio::sync::Mutex<()>>,
    cancel: tokio::sync::Notify,
    cancelled_attempts: RwLock<HashSet<String>>,
}

impl ConnectorOperation {
    async fn cancelled(&self, attempt_id: Option<&str>) {
        let Some(attempt_id) = attempt_id else {
            return std::future::pending().await;
        };
        loop {
            let notified = self.cancel.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .cancelled_attempts
                .read()
                .expect("cancellation lock poisoned")
                .contains(attempt_id)
            {
                return;
            }
            notified.await;
        }
    }
}

/// Who is making a call, for [`ToolRegistry::dispatch_with_transport`].
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CallContext<'a> {
    /// The scheduled run making the call.
    pub run: Option<&'a str>,
    /// The conversation the call belongs to, if the web app named one.
    pub conversation: Option<&'a str>,
    /// The connectors whose tools the model was offered, where narrower than every enabled one.
    pub offered: Option<&'a [String]>,
}

pub struct ToolRegistry {
    pub(super) result_store: Arc<super::result_store::ResultStore>,
    connectors: RwLock<HashMap<String, Arc<ConnectorHandle>>>,
    state: RwLock<ConnectorState>,
    activity: RwLock<HashMap<String, Activity>>,
    operations: RwLock<HashMap<String, Arc<ConnectorOperation>>>,
    approval_gate: RwLock<Option<Arc<dyn ApprovalGate>>>,
    host: RwLock<Option<Arc<dyn RegistryHost>>>,
    exposure: std::sync::Mutex<super::exposure::Tracker>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            result_store: Arc::new(super::result_store::ResultStore::default()),
            connectors: RwLock::new(HashMap::new()),
            state: RwLock::new(ConnectorState::default()),
            activity: RwLock::new(HashMap::new()),
            operations: RwLock::new(HashMap::new()),
            approval_gate: RwLock::new(None),
            host: RwLock::new(None),
            exposure: Default::default(),
        }
    }

    pub fn set_approval_gate(&self, gate: Arc<dyn ApprovalGate>) {
        *self
            .approval_gate
            .write()
            .expect("approval gate lock poisoned") = Some(gate);
    }

    /// Shared by connection attempts and configuration changes, including replacement instances.
    fn connector_operation(&self, connector_id: &str) -> Arc<ConnectorOperation> {
        self.operations
            .write()
            .expect("operations lock poisoned")
            .entry(connector_id.to_string())
            .or_default()
            .clone()
    }

    pub fn cancel_connection(&self, connector_id: &str, attempt_id: &str) {
        let operation = self.connector_operation(connector_id);
        operation
            .cancelled_attempts
            .write()
            .expect("cancellation lock poisoned")
            .insert(attempt_id.to_string());
        operation.cancel.notify_waiters();
    }

    pub(super) fn try_connector_operation(
        &self,
        connector_id: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, ConnectError> {
        self.connector_operation(connector_id).busy.clone().try_lock_owned().map_err(|_| {
            ConnectError::Failed(
                "A connection or configuration change is already in progress for this connector".into(),
            )
        })
    }

    pub fn set_host(&self, host: Arc<dyn RegistryHost>) {
        *self.host.write().expect("host lock poisoned") = Some(host);
    }

    fn host(&self) -> Option<Arc<dyn RegistryHost>> {
        self.host.read().expect("host lock poisoned").clone()
    }

    pub fn set_connector_enabled(&self, connector_id: &str, enabled: bool) {
        let mut state = self.state.write().expect("state lock poisoned");
        if enabled {
            state.disabled_connectors.remove(connector_id);
        } else {
            state.disabled_connectors.insert(connector_id.to_string());
        }
    }

    pub fn is_connector_enabled(&self, connector_id: &str) -> bool {
        !self
            .state
            .read()
            .expect("state lock poisoned")
            .disabled_connectors
            .contains(connector_id)
    }

    pub fn set_tool_policy(&self, connector_id: &str, tool_name: &str, policy: ToolPolicy) {
        let pin = (policy == ToolPolicy::AutoApprove)
            .then(|| self.pinnable(connector_id, tool_name))
            .flatten();
        self.state
            .write()
            .expect("state lock poisoned")
            .choose_policy(connector_id, tool_name, policy, pin);
    }

    /// Auto-approves the tool and lets it run even after the conversation has read untrusted
    /// content. Choosing any policy afterwards withdraws this.
    pub fn always_allow(&self, connector_id: &str, tool_name: &str) {
        let pin = self.pinnable(connector_id, tool_name);
        self.state
            .write()
            .expect("state lock poisoned")
            .always_allow(connector_id, tool_name, pin);
    }

    /// Whether [`always_allow`](Self::always_allow) holds: chosen, and the tool unchanged since.
    pub fn is_always_allowed(&self, connector_id: &str, tool_name: &str) -> bool {
        self.state
            .read()
            .expect("state lock poisoned")
            .is_always_allowed(connector_id, tool_name, || {
                self.pinnable(connector_id, tool_name)
            })
    }

    /// The fingerprint to pin an approval to: MCP tools only, whose servers can change them
    /// under the user's feet.
    fn pinnable(&self, connector_id: &str, tool_name: &str) -> Option<String> {
        if !connector_id.starts_with("mcp-") {
            return None;
        }
        self.get(connector_id)?
            .tools()
            .into_iter()
            .find(|tool| tool.name == tool_name)
            .map(|tool| tool.fingerprint())
    }

    /// The user's choice if they made one, else the policy the connector declares for the tool.
    /// A tool the connector doesn't list (not yet discovered, or an unreadable cache) asks.
    pub fn get_tool_policy(&self, connector_id: &str, tool_name: &str) -> ToolPolicy {
        self.saved_tool_policy(connector_id, tool_name)
            .unwrap_or_else(|| {
                self.get(connector_id)
                    .and_then(|connector| {
                        connector
                            .tools()
                            .into_iter()
                            .find(|tool| tool.name == tool_name)
                    })
                    .map(|tool| tool.policy)
                    .unwrap_or(ToolPolicy::Ask)
            })
    }

    /// An auto-approval of an MCP tool counts only while the tool is as it was when the user
    /// approved it.
    fn saved_tool_policy(&self, connector_id: &str, tool_name: &str) -> Option<ToolPolicy> {
        let choice = self.state.read().expect("state lock poisoned").tool_choice(
            connector_id,
            tool_name,
            || self.pinnable(connector_id, tool_name),
        );
        match choice {
            Choice::Unset => None,
            Choice::Stands(policy) => Some(policy),
            Choice::Unpinned(policy, fingerprint) => {
                let pinned = {
                    let mut state = self.state.write().expect("state lock poisoned");
                    state.pin_approval(connector_id, tool_name, fingerprint);
                    state.clone()
                };
                self.persist(&pinned);
                Some(policy)
            }
        }
    }

    pub fn clear_tool_policies(&self, connector_id: &str) {
        self.state
            .write()
            .expect("state lock poisoned")
            .forget_tool_choices(connector_id);
    }

    pub fn set_state(&self, state: ConnectorState) {
        *self.state.write().expect("state lock poisoned") = state;
    }

    pub fn state_snapshot(&self) -> ConnectorState {
        self.state.read().expect("state lock poisoned").clone()
    }

    pub fn register<C: Connector + 'static>(&self, connector: C) {
        let id = connector.id().to_string();
        let mut map = self.connectors.write().expect("registry lock poisoned");
        map.insert(id, Arc::new(ConnectorHandle::new(connector)));
    }

    /// Evicts a connector and marks it retired, returning it so its session can be closed.
    #[must_use = "a connector with a live session must have it dropped; see ToolRegistry::retire"]
    fn unregister(&self, connector_id: &str) -> Option<Arc<ConnectorHandle>> {
        let evicted = self
            .connectors
            .write()
            .expect("registry lock poisoned")
            .remove(connector_id);
        if let Some(handle) = &evicted {
            handle.retire();
        }
        self.activity
            .write()
            .expect("activity lock poisoned")
            .remove(connector_id);
        evicted
    }

    /// Swaps in a connector built from configuration that has just changed.
    ///
    /// The outgoing one is retired and its session closed before the replacement is registered,
    /// so a call already in flight cannot reach the configuration it was built from.
    pub async fn replace<C: Connector + 'static>(&self, connector: C) {
        let id = connector.id().to_string();
        self.retire(&id).await;
        self.register(connector);
        if let Some(host) = self.host() {
            host.notify_changed(&id);
        }
    }

    /// Evicts a connector and closes whatever session it held.
    pub async fn retire(&self, connector_id: &str) {
        if let Some(handle) = self.unregister(connector_id) {
            if let Some(session) = handle.as_session() {
                session.drop_session().await;
            }
        }
    }

    /// Nothing is dispatchable once this returns, which is what prevents the outgoing account's
    /// connectors being reached while their sessions close.
    pub fn take_connectors(&self) -> Vec<Arc<ConnectorHandle>> {
        let mut map = self.connectors.write().expect("registry lock poisoned");
        self.activity
            .write()
            .expect("activity lock poisoned")
            .clear();
        map.drain()
            .map(|(_, handle)| {
                handle.retire();
                handle
            })
            .collect()
    }

    /// Snapshot of all registered connectors, for iteration outside the lock.
    pub fn connectors(&self) -> Vec<Arc<ConnectorHandle>> {
        self.connectors
            .read()
            .expect("registry lock poisoned")
            .values()
            .cloned()
            .collect()
    }

    fn get(&self, connector_id: &str) -> Option<Arc<ConnectorHandle>> {
        self.connectors
            .read()
            .expect("registry lock poisoned")
            .get(connector_id)
            .cloned()
    }

    fn activity_of(&self, connector_id: &str) -> Activity {
        self.activity
            .read()
            .expect("activity lock poisoned")
            .get(connector_id)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn has_ever_succeeded(&self, connector_id: &str) -> bool {
        self.state
            .read()
            .expect("state lock poisoned")
            .succeeded_once
            .contains(connector_id)
    }

    fn record_success(&self, connector_id: &str) {
        self.set_activity(connector_id, Activity::Succeeded);
        let first_time = {
            let mut state = self.state.write().expect("state lock poisoned");
            state
                .succeeded_once
                .insert(connector_id.to_string())
                .then(|| state.clone())
        };
        // Only the first success carries information, so a run of calls writes nothing.
        if let Some(state) = first_time {
            self.persist(&state);
        }
    }

    /// `error` is shown in Settings; only `kind` is logged.
    fn record_failure(&self, connector_id: &str, kind: &str, error: String) {
        warn!("connector {connector_id} failed: {kind}");
        self.set_activity(connector_id, Activity::Failed { error });
    }

    pub fn forget_success_history(&self, connector_id: &str) {
        let cleared = {
            let mut state = self.state.write().expect("state lock poisoned");
            state.succeeded_once.remove(connector_id);
            state.clone()
        };
        self.persist(&cleared);
    }

    fn persist(&self, state: &ConnectorState) {
        if let Some(host) = self.host() {
            host.persist(state);
        }
    }

    fn set_activity(&self, connector_id: &str, activity: Activity) {
        let previous = self
            .activity
            .write()
            .expect("activity lock poisoned")
            .insert(connector_id.to_string(), activity.clone());
        if previous.as_ref() != Some(&activity) {
            if let Some(host) = self.host() {
                host.notify_changed(connector_id);
            }
        }
    }

    fn manifest_of(&self, connector: &Arc<ConnectorHandle>) -> ConnectorManifest {
        let descriptor = connector.descriptor();
        let capabilities = Capabilities {
            credentials: connector.as_credentialed().is_some(),
            session: connector.as_session().is_some(),
        };
        let readiness = connector.readiness();
        let activity = self.activity_of(&descriptor.id);
        let status = ConnectorStatus::derive(
            &readiness,
            capabilities.has_remote(),
            &activity,
            self.has_ever_succeeded(&descriptor.id),
        );
        let tools: Vec<ToolDefinition> = connector
            .tools()
            .into_iter()
            .map(|mut tool| {
                if let Some(policy) = self.saved_tool_policy(&descriptor.id, &tool.name) {
                    tool.policy = policy;
                }
                tool
            })
            .collect();
        let always_allowed = tools
            .iter()
            .filter(|tool| self.is_always_allowed(&descriptor.id, &tool.name))
            .map(|tool| tool.name.clone())
            .collect();

        ConnectorManifest {
            enabled: self.is_connector_enabled(&descriptor.id),
            activity: capabilities.has_remote().then_some(activity),
            capabilities,
            readiness,
            status,
            tools,
            always_allowed,
            id: descriptor.id,
            display_name: descriptor.display_name,
            description: descriptor.description,
            version: descriptor.version,
            icon: descriptor.icon,
            auth: descriptor.auth,
        }
    }

    /// Builtins are discovered independently of connector manifests and preferences.
    pub fn builtin_tools(&self) -> Vec<ToolDefinition> {
        if !self.result_store.is_active() {
            return vec![];
        }
        vec![super::result_reader::definition()]
    }

    pub fn all_manifests(&self) -> Vec<ConnectorManifest> {
        self.connectors()
            .iter()
            .map(|connector| self.manifest_of(connector))
            .collect()
    }

    #[allow(dead_code)]
    pub fn get_manifest(&self, connector_id: &str) -> Option<ConnectorManifest> {
        self.get(connector_id)
            .map(|connector| self.manifest_of(&connector))
    }

    pub async fn dispatch(
        &self,
        connector_id: &str,
        tool_name: &str,
        input: Value,
    ) -> Result<Value, ToolError> {
        self.dispatch_with_approval(connector_id, tool_name, input, false)
            .await
    }

    /// The override is resolved from a registered schedule run, never a global policy mutation.
    pub(crate) async fn dispatch_with_approval(
        &self,
        connector_id: &str,
        tool_name: &str,
        input: Value,
        auto_approve: bool,
    ) -> Result<Value, ToolError> {
        self.dispatch_with_access(connector_id, tool_name, input, auto_approve, false)
            .await
    }

    /// Schedule access is enforced independently of global approval preferences.
    pub(crate) async fn dispatch_with_access(
        &self,
        connector_id: &str,
        tool_name: &str,
        input: Value,
        auto_approve: bool,
        read_only: bool,
    ) -> Result<Value, ToolError> {
        self.dispatch_with_transport(
            connector_id,
            tool_name,
            input,
            auto_approve,
            read_only,
            false,
            CallContext::default(),
        )
        .await
    }

    /// `context` decides where [`exposure`](super::exposure) is tracked and enforced.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn dispatch_with_transport(
        &self,
        connector_id: &str,
        tool_name: &str,
        input: Value,
        auto_approve: bool,
        read_only: bool,
        file_transport: bool,
        context: CallContext<'_>,
    ) -> Result<Value, ToolError> {
        let exposure_scope = Scope::of(context.run, context.conversation);
        if connector_id == "tool_results" {
            let result = match tool_name {
                "read" => self.result_store.read(self.result_store.scope(), &input),
                "complete_file" => self
                    .result_store
                    .complete_file(self.result_store.scope(), &input),
                _ => Err(ToolError::UnknownTool(tool_name.into())),
            };
            return result.map(|value| {
                if file_transport {
                    serde_json::json!({"value": value, "file_transport": false})
                } else {
                    value
                }
            });
        }
        let connector = self
            .get(connector_id)
            .ok_or_else(|| ToolError::UnknownConnector(connector_id.into()))?;

        let risk = connector.risk(tool_name);
        self.authorize(
            &connector,
            tool_name,
            &input,
            risk,
            auto_approve,
            read_only,
            context,
        )
        .await?;

        let scope = self.result_store.scope();
        let allow_file = connector_id == "filesystem"
            && tool_name == "fs_read"
            && input.get("format").and_then(Value::as_str) == Some("file");
        let result = self
            .invoke_once_retrying(&connector, tool_name, input, read_only)
            .await;
        let result = self.sanitize_and_record(risk, exposure_scope, result);
        let has_remote = connector.as_credentialed().is_some() || connector.as_session().is_some();
        match &result {
            _ if !has_remote => {}
            Ok(_) => self.record_success(connector_id),
            Err(e) if e.is_transport() => {
                self.record_failure(connector_id, e.kind(), e.to_string())
            }
            Err(_) => {}
        }
        // Epoch zero is an uninitialized standalone registry. Production initializes
        // the account scope before registering connectors. Preserve the captured
        // epoch across the await so late results from an outgoing account are rejected.
        if scope == 0 {
            return result.map(|value| {
                if file_transport {
                    serde_json::json!({"value": value, "file_transport": false})
                } else {
                    value
                }
            });
        }
        match result {
            Ok(value) if file_transport => {
                self.result_store
                    .preview_with_transport(scope, connector_id, value, allow_file)
            }
            Ok(value) => self.result_store.preview(scope, connector_id, value),
            Err(error) => Err(error),
        }
    }

    /// Runs every check that can reject a call before it reaches the connector: schedule access,
    /// connector and tool settings, the exposure gate and user approval.
    #[allow(clippy::too_many_arguments)]
    async fn authorize(
        &self,
        connector: &ConnectorHandle,
        tool_name: &str,
        input: &Value,
        risk: ToolRisk,
        auto_approve: bool,
        read_only: bool,
        context: CallContext<'_>,
    ) -> Result<(), ToolError> {
        let connector_id = connector.id();
        if read_only {
            connector.check_read_only(tool_name)?;
        }

        if !self.is_connector_enabled(connector_id) {
            return Err(ToolError::ConnectorDisabled);
        }

        if let Readiness::Blocked { reason } = connector.readiness() {
            return Err(ToolError::NotReady(reason));
        }

        let policy = self.get_tool_policy(connector_id, tool_name);
        if policy == ToolPolicy::Disable {
            return Err(ToolError::ToolDisabled);
        }
        let scope = Scope::of(context.run, context.conversation);
        let exposure = self
            .exposure_of(scope)
            .with_unvetted_tools(self.offers_unvetted_tools(context.offered));
        let gate = self.exposure().gate(
            risk,
            connector.request_url(tool_name, input).as_ref(),
            input,
            scope,
            exposure,
        );
        let gate = if self.is_always_allowed(connector_id, tool_name) {
            gate.always_allowed()
        } else {
            gate
        };
        let (needs_approval, reason) = match gate {
            Gate::Refuse(why) => {
                info!("refused {connector_id}.{tool_name}: {why}");
                return Err(ToolError::AccessDenied(why.into()));
            }
            Gate::Ask(reason) => (true, Some(reason)),
            Gate::Allow => (policy == ToolPolicy::Ask && !auto_approve, None),
        };
        if needs_approval {
            match self
                .request_approval(
                    connector_id,
                    tool_name,
                    input,
                    reason.as_ref(),
                    context.conversation,
                )
                .await
            {
                ApprovalDecision::Deny => return Err(ToolError::ApprovalDenied),
                ApprovalDecision::Once => {}
                ApprovalDecision::Always => {
                    self.remember_always(connector_id, tool_name, reason.as_ref())
                }
            }
        }
        Ok(())
    }

    /// Applied to every result and error before it is stored or returned. Strips hidden text,
    /// then records exposure. Failed calls are recorded as well, because an error can carry
    /// remote text.
    fn sanitize_and_record(
        &self,
        risk: ToolRisk,
        scope: Scope,
        result: Result<Value, ToolError>,
    ) -> Result<Value, ToolError> {
        let result = result
            .map_err(|error| error.for_model(MAX_PREVIEW_BYTES))
            .map(|mut value| {
                hidden_text::strip_smuggled_value(&mut value);
                value
            });
        let exposed = self
            .exposure()
            .record(risk.exposes, scope, result.as_ref().ok());
        if let Scope::Conversation(id) = scope {
            self.expose_conversation(id, exposed);
        }
        result
    }

    /// Re-establishes a session that turned out to be dead and retries the call once.
    ///
    /// Applies to a transport failure on a connector with a session; every other failure is
    /// returned as it came.
    async fn invoke_once_retrying(
        &self,
        connector: &Arc<ConnectorHandle>,
        tool_name: &str,
        input: Value,
        read_only: bool,
    ) -> Result<Value, ToolError> {
        let invoke = |input| async move {
            if read_only {
                connector.invoke_read_only(tool_name, input).await
            } else {
                connector.invoke(tool_name, input).await
            }
        };
        let first = invoke(input.clone()).await;
        let Err(error) = &first else {
            return first;
        };
        if !error.is_transport() {
            return first;
        }
        let Some(session) = connector.as_session() else {
            return first;
        };

        session.drop_session().await;
        invoke(input).await
    }

    /// Whether the model is offered tools from a server added by hand: among `offered`, or among
    /// every enabled connector when the caller doesn't say.
    fn offers_unvetted_tools(&self, offered: Option<&[String]>) -> bool {
        let unvetted = |connector: &ConnectorHandle| {
            !connector.vetted() && self.is_connector_enabled(connector.id())
        };
        match offered {
            Some(ids) => ids
                .iter()
                .filter_map(|id| self.get(id))
                .any(|connector| unvetted(&connector)),
            None => self
                .connectors()
                .iter()
                .any(|connector| unvetted(connector)),
        }
    }

    fn exposure(&self) -> std::sync::MutexGuard<'_, super::exposure::Tracker> {
        self.exposure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Forgets what the outgoing account's tools returned. Conversations' exposure goes with the
    /// account's state.
    pub fn reset_exposure(&self) {
        *self.exposure() = Default::default();
    }

    fn exposure_of(&self, scope: Scope) -> Exposure {
        match scope {
            Scope::Conversation(id) => self
                .state
                .read()
                .expect("state lock poisoned")
                .exposed_conversations
                .exposure(id),
            other => self.exposure().exposure(other),
        }
    }

    /// Persists only when a conversation's exposure grows, which happens a few times at most.
    fn expose_conversation(&self, id: &str, exposed: Exposure) {
        let changed = {
            let mut state = self.state.write().expect("state lock poisoned");
            state
                .exposed_conversations
                .expose(id, exposed)
                .then(|| state.clone())
        };
        if let Some(state) = changed {
            self.persist(&state);
        }
    }

    /// "Don't ask again" on an approval card: the tool stops asking for its own sake, as the
    /// Auto-approve setting does. It never becomes [`always_allow`](Self::always_allow)ed: the
    /// hosted web app can answer approval cards, so overriding untrusted content needs Settings.
    fn remember_always(
        &self,
        connector_id: &str,
        tool_name: &str,
        reason: Option<&ApprovalReason>,
    ) {
        if reason.is_some_and(|reason| reason.kind == "hidden_text") {
            return;
        }
        self.set_tool_policy(connector_id, tool_name, ToolPolicy::AutoApprove);
        let state = self.state_snapshot();
        self.persist(&state);
        if let Some(host) = self.host() {
            host.notify_changed(connector_id);
        }
    }

    async fn request_approval(
        &self,
        connector_id: &str,
        tool_name: &str,
        input: &Value,
        reason: Option<&ApprovalReason>,
        conversation: Option<&str>,
    ) -> ApprovalDecision {
        let gate = self
            .approval_gate
            .read()
            .expect("approval gate lock poisoned")
            .clone();
        match gate {
            Some(gate) => {
                gate.request(connector_id, tool_name, input, reason, conversation)
                    .await
            }
            None => {
                approval_failure("Approval gate missing");
                error!("no approval gate wired; denying 'ask' tool call");
                ApprovalDecision::Deny
            }
        }
    }

    /// Obtains credentials where the connector has any, then opens a session where it has one.
    ///
    /// `authenticate` is called unconditionally and relies on its own idempotence. A credential
    /// written by this call is withdrawn if the session then fails to open.
    pub async fn connect(
        &self,
        connector_id: &str,
        input: Option<Value>,
        mode: Interactive,
    ) -> Result<(), ConnectError> {
        self.connect_attempt(connector_id, input, mode, None).await
    }

    pub async fn connect_attempt(
        &self,
        connector_id: &str,
        input: Option<Value>,
        mode: Interactive,
        attempt_id: Option<&str>,
    ) -> Result<(), ConnectError> {
        let _operation = self.try_connector_operation(connector_id)?;
        let connector = self
            .get(connector_id)
            .ok_or_else(|| ConnectError::UnknownConnector(connector_id.into()))?;

        let credentialed = connector.as_credentialed();
        let session = connector.as_session();
        if credentialed.is_none() && session.is_none() {
            return Err(ConnectError::NothingToConnect(connector_id.into()));
        }

        let mut wrote = Stored::Nothing;
        let operation = self.connector_operation(connector_id);
        let mut cancelled = false;
        let outcome: Result<(), ConnectError> = tokio::select! {
            biased;
            _ = operation.cancelled(attempt_id) => {
                cancelled = true;
                Err(AuthError::Cancelled.into())
            }
            result = async {
                if let Some(credentialed) = credentialed {
                    wrote = credentialed.authenticate(input, mode).await?;
                }
                if let Some(session) = session {
                    session.ensure().await?;
                }
                Ok(())
            } => result,
        };

        match &outcome {
            Ok(()) => self.record_success(connector_id),
            // Startup cannot prompt for missing credentials; unfinished setup is not a failure.
            Err(ConnectError::Auth(AuthError::RequiresSetup)) if mode == Interactive::No => {}
            Err(e) => {
                if wrote == Stored::Something {
                    if let Some(credentialed) = credentialed {
                        let _ = credentialed.forget().await;
                    }
                }
                if cancelled {
                    self.set_activity(connector_id, Activity::Untried);
                } else {
                    self.record_failure(connector_id, e.kind(), e.to_string());
                }
            }
        }
        if let Some(attempt_id) = attempt_id {
            operation
                .cancelled_attempts
                .write()
                .map_err(|_| ConnectError::Failed("cancellation lock poisoned".into()))?
                .remove(attempt_id);
        }
        outcome
    }

    /// Drops any live session and erases the stored credentials.
    ///
    /// Available only where there are credentials; a session-only connector is switched off with
    /// the enable toggle.
    pub async fn disconnect(&self, connector_id: &str) -> Result<(), ConnectError> {
        let _operation = self.try_connector_operation(connector_id)?;
        self.disconnect_locked(connector_id).await
    }

    /// Caller holds the connector operation guard (also used by removal).
    pub(super) async fn disconnect_locked(&self, connector_id: &str) -> Result<(), ConnectError> {
        let connector = self
            .get(connector_id)
            .ok_or_else(|| ConnectError::UnknownConnector(connector_id.into()))?;
        let credentialed = connector
            .as_credentialed()
            .ok_or_else(|| ConnectError::NothingToForget(connector_id.into()))?;

        if let Some(session) = connector.as_session() {
            session.drop_session().await;
        }
        credentialed.forget().await.map_err(ConnectError::Failed)?;
        self.activity
            .write()
            .expect("activity lock poisoned")
            .remove(connector_id);
        self.forget_success_history(connector_id);
        Ok(())
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
