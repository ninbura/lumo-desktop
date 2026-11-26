mod catalog;
mod config;
mod credentials;
mod migrations;
mod tool_cache;

pub use config::{
    connector_id, load_mcp_servers, save_mcp_servers, validate_server_id, CustomHeader, HttpAuth,
    McpServerConfig, McpTransport,
};
#[cfg(test)]
use credentials::secret_keys;
pub use credentials::{on_config_change, on_remove};
use migrations::migrate_legacy_endpoints;
pub use tool_cache::{mcp_tools_path, NoToolCache, ToolCache};

use super::super::auth::{
    keychain_secrets, AuthMechanism, CredentialState, Interactive, McpTransportInfo, Readiness,
    Secrets, Stored,
};
use super::super::child_process::{tool_process, Environment};
use super::super::command_path;
use super::super::connector_state::ToolPolicy;
use super::super::error::{AuthError, ToolError};
use super::super::exposure::ToolRisk;
use super::super::hidden_text;
use super::super::icons::bundled_icon_data_uri;
use super::super::oauth::{
    bind_oauth_callback, browser_url, wait_for_oauth_callback, DeepLinkCallback, SLACK_CALLBACK_URI,
};
use super::super::registry::{
    Connector, ConnectorDescriptor, Credentialed, Session, ToolDefinition,
};
use crate::account::AccountId;
use crate::reporting::mcp_result_serialization_failure;
use async_trait::async_trait;
use credentials::{
    client_secret_key, credential_state, env_secret_key, header_secret_key, token_key,
    BoundSecrets, KeychainCredentialStore, McpCredentialInput, UserAgentOAuthHttpClient,
    CLIENT_USER_AGENT,
};
use log::{debug, info, warn};
use reqwest::header::{HeaderName, HeaderValue, AUTHORIZATION};
use rmcp::model::{CallToolRequestParams, ClientConfig, ProtocolVersion};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::auth::{
    default_oauth_http_client, AuthClient, AuthorizationManager, OAuthClientConfig,
};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::ServiceExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tauri::AppHandle;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tool_cache::AccountToolCache;

/// Servers are third-party: only a catalog server's tool that declares itself read-only runs
/// unprompted. A server added by hand can label anything read-only.
fn default_policy(read_only: Option<bool>, catalog: bool) -> ToolPolicy {
    if catalog && read_only == Some(true) {
        ToolPolicy::AutoApprove
    } else {
        ToolPolicy::Ask
    }
}

/// Applied to discovered and cached tools alike: caches written by older builds recorded
/// AutoApprove for every tool, and could hold hidden characters.
fn vet(mut tool: ToolDefinition, catalog: bool) -> ToolDefinition {
    tool.policy = default_policy(tool.read_only, catalog);
    tool.description = hidden_text::strip(&tool.description);
    tool.title = tool.title.as_deref().map(hidden_text::strip);
    hidden_text::strip_value(&mut tool.input_schema);
    tool
}

/// Drops every tool whose name appears more than once. Approvals are pinned per name, so a
/// second definition under an approved name would carry the approval to a changed tool, and
/// which copy the model acts on is up to the server.
fn without_repeated_names(server_id: &str, tools: Vec<ToolDefinition>) -> Vec<ToolDefinition> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for tool in &tools {
        *counts.entry(tool.name.clone()).or_default() += 1;
    }
    for (name, count) in &counts {
        if *count > 1 {
            warn!("mcp-{server_id}: dropped tool {name:?}, listed {count} times");
        }
    }
    tools
        .into_iter()
        .filter(|tool| counts[&tool.name] == 1)
        .collect()
}

fn tool_definition(tool: rmcp::model::Tool) -> ToolDefinition {
    let read_only = tool.annotations.as_ref().and_then(|a| a.read_only_hint);
    ToolDefinition {
        policy: ToolPolicy::Ask,
        input_schema: tool.schema_as_json_value(),
        title: tool
            .title
            .clone()
            .or_else(|| tool.annotations.as_ref().and_then(|a| a.title.clone())),
        read_only,
        open_world: tool.annotations.as_ref().and_then(|a| a.open_world_hint),
        name: tool.name.to_string(),
        description: tool.description.map(|d| d.to_string()).unwrap_or_default(),
    }
}

fn intake(
    server_id: &str,
    tools: impl IntoIterator<Item = ToolDefinition>,
    catalog: bool,
) -> Vec<ToolDefinition> {
    let vetted = tools.into_iter().map(|tool| vet(tool, catalog));
    without_repeated_names(server_id, vetted.collect())
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(60);
const MCP_OAUTH_REDIRECT_PORTS: [u16; 3] = [9876, 9877, 9878];

/// The variables set on top of the inherited ones: the resolved search path, then the config's
/// `env` laid over it.
fn child_env(env: &HashMap<String, String>, search_path: &str) -> HashMap<String, String> {
    let mut resolved = HashMap::from([("PATH".to_string(), search_path.to_string())]);
    resolved.extend(env.clone());
    resolved
}

pub struct McpConnector {
    id: String,
    config: McpServerConfig,
    secrets: Secrets,
    tool_cache: Box<dyn ToolCache>,
    tools: RwLock<Vec<ToolDefinition>>,
    client: Mutex<Option<RunningService<RoleClient, ClientConfig>>>,
    credentials: RwLock<CredentialState>,
    call_timeout: Duration,
    catalog: bool,
}

impl McpConnector {
    pub fn new(
        config: McpServerConfig,
        secrets: Secrets,
        tool_cache: Box<dyn ToolCache>,
        credentials: CredentialState,
    ) -> Self {
        let catalog = catalog::is_catalog_server(&config);
        let tools = Self::cached_tools(&config.id, tool_cache.as_ref(), catalog);
        Self {
            id: connector_id(&config.id),
            secrets: Arc::new(BoundSecrets::new(secrets, &config)),
            config,
            tools: RwLock::new(tools),
            tool_cache,
            client: Mutex::new(None),
            credentials: RwLock::new(credentials),
            call_timeout: DEFAULT_CALL_TIMEOUT,
            catalog,
        }
    }

    fn cached_tools(server_id: &str, cache: &dyn ToolCache, catalog: bool) -> Vec<ToolDefinition> {
        intake(server_id, cache.load(), catalog)
    }

    /// Probes the keychain once, which is what keeps [`Connector::readiness`] synchronous.
    pub async fn from_config(
        config: McpServerConfig,
        secrets: Secrets,
        app_handle: &AppHandle,
        account: &AccountId,
    ) -> Self {
        let credentials =
            credential_state(&config, &BoundSecrets::new(secrets.clone(), &config)).await;
        let tool_cache = Box::new(AccountToolCache {
            app_handle: app_handle.clone(),
            account: account.clone(),
            server_id: config.id.clone(),
        });
        Self::new(config, secrets, tool_cache, credentials)
    }

    pub fn with_call_timeout(
        config: McpServerConfig,
        secrets: Secrets,
        call_timeout: Duration,
    ) -> Self {
        Self {
            call_timeout,
            ..Self::new(
                config,
                secrets,
                Box::new(NoToolCache),
                CredentialState::Absent,
            )
        }
    }

    pub fn connector_id(&self) -> String {
        self.id.clone()
    }

    fn client_secret_key(&self) -> String {
        client_secret_key(&self.id)
    }

    fn token_key(&self) -> String {
        token_key(&self.id)
    }

    fn header_secret_key(&self, header_name: &str) -> String {
        header_secret_key(&self.id, header_name)
    }

    fn env_secret_key(&self, var_name: &str) -> String {
        env_secret_key(&self.id, var_name)
    }

    fn credentials(&self) -> CredentialState {
        *self
            .credentials
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set_credentials(&self, state: CredentialState) {
        *self
            .credentials
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
    }

    /// Writes whatever the form supplied, reporting whether an access credential was written.
    ///
    /// A confidential client's secret is configuration rather than an access credential, so it
    /// does not count.
    async fn store_supplied(&self, supplied: &McpCredentialInput) -> Result<Stored, AuthError> {
        if supplied.is_empty() {
            return Ok(Stored::Nothing);
        }

        let mut wrote = Stored::Nothing;
        if let Some(secret) = supplied.client_secret.as_deref().filter(|s| !s.is_empty()) {
            self.secrets
                .store(&self.client_secret_key(), secret)
                .await
                .map_err(AuthError::Failed)?;
        }

        let auth = match &self.config.transport {
            McpTransport::Http { auth, .. } => auth,
            McpTransport::Stdio { secret_env, .. } => {
                for name in secret_env {
                    let Some(value) = supplied
                        .env
                        .get(name)
                        .map(String::as_str)
                        .filter(|value| !value.is_empty())
                    else {
                        continue;
                    };
                    self.secrets
                        .store(&self.env_secret_key(name), value)
                        .await
                        .map_err(AuthError::Failed)?;
                    wrote = Stored::Something;
                }
                return Ok(wrote);
            }
        };

        match auth {
            HttpAuth::Token { .. } => {
                if let Some(token) = supplied.token.as_deref().filter(|t| !t.is_empty()) {
                    self.secrets
                        .store(&self.token_key(), token)
                        .await
                        .map_err(AuthError::Failed)?;
                    wrote = Stored::Something;
                }
            }
            HttpAuth::Headers { headers } => {
                for header in headers {
                    let Some(value) = supplied
                        .headers
                        .get(&header.name)
                        .map(String::as_str)
                        .filter(|value| !value.is_empty())
                    else {
                        continue;
                    };
                    self.secrets
                        .store(&self.header_secret_key(&header.name), value)
                        .await
                        .map_err(AuthError::Failed)?;
                    wrote = Stored::Something;
                }
            }
            HttpAuth::Oauth { .. } | HttpAuth::None => {}
        }
        Ok(wrote)
    }

    /// The config's `env` with each declared secret's keychain value laid over it.
    ///
    /// A declared secret that is not in the keychain is an error rather than an omission: the
    /// server would start without the credential it needs and fail its first call.
    fn parse_request_headers(
        request_headers: &HashMap<String, String>,
    ) -> Result<HashMap<HeaderName, HeaderValue>, ToolError> {
        let mut header_map = HashMap::new();
        for (name, value) in request_headers {
            if value.is_empty() {
                continue;
            }
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                ToolError::ExecutionFailed(format!("invalid request header name '{name}': {e}"))
            })?;
            let value = HeaderValue::from_str(value).map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "invalid request header value for '{name}': {e}"
                ))
            })?;
            header_map.insert(name, value);
        }
        Ok(header_map)
    }

    fn mcp_client() -> ClientConfig {
        // serve() uses initialize, which the 2026-07-28 protocol removed.
        let mut config = ClientConfig::default();
        config.protocol_version = ProtocolVersion::LATEST_WITH_INITIALIZE;
        config
    }

    fn http_transport_config(
        url: &str,
        request_headers: &HashMap<String, String>,
    ) -> Result<StreamableHttpClientTransportConfig, ToolError> {
        let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_string());
        let headers = Self::parse_request_headers(request_headers)?;
        if !headers.is_empty() {
            config = config.custom_headers(headers);
        }
        Ok(config)
    }

    async fn resolved_env(
        &self,
        env: &HashMap<String, String>,
        secret_env: &[String],
    ) -> Result<HashMap<String, String>, ToolError> {
        let mut resolved = env.clone();
        for name in secret_env {
            let value = self
                .secrets
                .load(&self.env_secret_key(name))
                .await
                .map_err(ToolError::ExecutionFailed)?
                .filter(|value| !value.is_empty())
                .ok_or(ToolError::NotAuthenticated)?;
            resolved.insert(name.clone(), value);
        }
        Ok(resolved)
    }

    async fn ensure_connected(&self) -> Result<(), ToolError> {
        let mut guard = self.client.lock().await;
        if guard.is_some() {
            return Ok(());
        }

        let service = match &self.config.transport {
            McpTransport::Stdio {
                command,
                args,
                env,
                secret_env,
            } => {
                let env = self.resolved_env(env, secret_env).await?;

                let resolved = command_path::resolve(command)
                    .await
                    .map_err(|e| ToolError::Unreachable(format!("{}: {e}", self.id)))?;

                let mut command = tool_process(&resolved.program, Environment::Allowlisted);
                command
                    .command_mut()
                    .args(args)
                    .envs(child_env(&env, &resolved.search_path));

                let (transport, stderr) = TokioChildProcess::builder(command)
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| {
                        ToolError::Unreachable(format!("{}: failed to start server: {e}", self.id))
                    })?;

                if let Some(stderr) = stderr {
                    let connector_id = self.connector_id();
                    tokio::spawn(async move {
                        let mut lines = BufReader::new(stderr).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            debug!("[{connector_id}] {line}");
                        }
                    });
                }

                tokio::time::timeout(CONNECT_TIMEOUT, Self::mcp_client().serve(transport))
                    .await
                    .map_err(|_| {
                        ToolError::Unreachable(format!(
                            "{}: no response within {CONNECT_TIMEOUT:?}",
                            self.id
                        ))
                    })?
                    .map_err(|e| {
                        ToolError::Unreachable(format!("{}: initialize failed: {e}", self.id))
                    })?
            }
            McpTransport::Http {
                url,
                request_headers,
                auth: HttpAuth::Oauth {
                    scopes, client_id, ..
                },
            } => {
                let mut manager = self
                    .new_oauth_manager(url)
                    .await
                    .map_err(ToolError::ExecutionFailed)?;

                let has_creds = manager.initialize_from_store().await.map_err(|e| {
                    ToolError::ExecutionFailed(format!("OAuth credential load failed: {e}"))
                })?;
                if !has_creds {
                    return Err(ToolError::NotAuthenticated);
                }

                // Confidential clients: re-supply the client_secret so rmcp's
                // implicit token refresh can authenticate. initialize_from_store
                // only restores a secret-less client from the stored client_id.
                if let Some(client_id) = client_id {
                    self.configure_confidential_client(
                        &mut manager,
                        client_id,
                        scopes,
                        &self.oauth_redirect_uri(MCP_OAUTH_REDIRECT_PORTS[0]),
                    )
                    .await
                    .map_err(ToolError::ExecutionFailed)?;
                }

                let http = reqwest::Client::builder()
                    .user_agent(CLIENT_USER_AGENT)
                    .build()
                    .map_err(|e| ToolError::ExecutionFailed(format!("http client: {e}")))?;
                let auth_client = AuthClient::new(http, manager);
                let transport = StreamableHttpClientTransport::with_client(
                    auth_client,
                    Self::http_transport_config(url, request_headers)?,
                );

                match tokio::time::timeout(CONNECT_TIMEOUT, Self::mcp_client().serve(transport))
                    .await
                    .map_err(|_| {
                        ToolError::Unreachable(format!(
                            "{}: no response within {CONNECT_TIMEOUT:?}",
                            self.id
                        ))
                    })? {
                    Ok(service) => service,
                    Err(e) if e.is_authorization_required() => {
                        self.secrets
                            .delete(&self.id)
                            .await
                            .map_err(ToolError::ExecutionFailed)?;
                        self.set_credentials(CredentialState::Absent);
                        return Err(ToolError::NotAuthenticated);
                    }
                    Err(e) => {
                        return Err(ToolError::Unreachable(format!(
                            "{}: initialize failed: {e}",
                            self.id
                        )))
                    }
                }
            }
            McpTransport::Http {
                url,
                request_headers,
                auth: HttpAuth::Token { scheme },
            } => {
                let token = self
                    .secrets
                    .load(&self.token_key())
                    .await
                    .map_err(ToolError::ExecutionFailed)?
                    .filter(|v| !v.is_empty())
                    .ok_or(ToolError::NotAuthenticated)?;

                let header = HeaderValue::from_str(&format!("{scheme} {token}")).map_err(|e| {
                    ToolError::ExecutionFailed(format!("invalid Authorization header: {e}"))
                })?;
                let mut headers = Self::parse_request_headers(request_headers)?;
                headers.insert(AUTHORIZATION, header);

                let transport = StreamableHttpClientTransport::from_config(
                    StreamableHttpClientTransportConfig::with_uri(url.clone())
                        .custom_headers(headers),
                );

                tokio::time::timeout(CONNECT_TIMEOUT, Self::mcp_client().serve(transport))
                    .await
                    .map_err(|_| {
                        ToolError::Unreachable(format!(
                            "{}: no response within {CONNECT_TIMEOUT:?}",
                            self.id
                        ))
                    })?
                    .map_err(|e| {
                        ToolError::Unreachable(format!("{}: initialize failed: {e}", self.id))
                    })?
            }
            McpTransport::Http {
                url,
                request_headers,
                auth: HttpAuth::Headers { headers },
            } => {
                let mut header_map = Self::parse_request_headers(request_headers)?;
                for spec in headers {
                    let secret = self
                        .secrets
                        .load(&self.header_secret_key(&spec.name))
                        .await
                        .map_err(ToolError::ExecutionFailed)?
                        .filter(|v| !v.is_empty())
                        .ok_or(ToolError::NotAuthenticated)?;

                    let name = HeaderName::from_bytes(spec.name.as_bytes()).map_err(|e| {
                        ToolError::ExecutionFailed(format!(
                            "invalid header name '{}': {e}",
                            spec.name
                        ))
                    })?;
                    let value = HeaderValue::from_str(&format!("{}{}", spec.prefix, secret))
                        .map_err(|e| {
                            ToolError::ExecutionFailed(format!(
                                "invalid value for header '{}': {e}",
                                spec.name
                            ))
                        })?;
                    header_map.insert(name, value);
                }

                let transport = StreamableHttpClientTransport::from_config(
                    StreamableHttpClientTransportConfig::with_uri(url.clone())
                        .custom_headers(header_map),
                );

                tokio::time::timeout(CONNECT_TIMEOUT, Self::mcp_client().serve(transport))
                    .await
                    .map_err(|_| {
                        ToolError::Unreachable(format!(
                            "{}: no response within {CONNECT_TIMEOUT:?}",
                            self.id
                        ))
                    })?
                    .map_err(|e| {
                        ToolError::Unreachable(format!("{}: initialize failed: {e}", self.id))
                    })?
            }
            McpTransport::Http {
                url,
                request_headers,
                auth: HttpAuth::None,
            } => {
                let transport = StreamableHttpClientTransport::from_config(
                    Self::http_transport_config(url, request_headers)?,
                );

                tokio::time::timeout(CONNECT_TIMEOUT, Self::mcp_client().serve(transport))
                    .await
                    .map_err(|_| {
                        ToolError::Unreachable(format!(
                            "{}: no response within {CONNECT_TIMEOUT:?}",
                            self.id
                        ))
                    })?
                    .map_err(|e| {
                        ToolError::Unreachable(format!("{}: initialize failed: {e}", self.id))
                    })?
            }
        };

        let listed = service
            .list_all_tools()
            .await
            .map_err(|e| ToolError::Unreachable(format!("{}: listing tools failed: {e}", self.id)))?
            .into_iter()
            .map(tool_definition);
        let tools = intake(&self.config.id, listed, self.catalog);

        self.tool_cache.save(&tools);
        *self
            .tools
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = tools;
        *guard = Some(service);
        Ok(())
    }

    fn oauth_redirect_uri(&self, port: u16) -> String {
        if self.uses_slack_callback() {
            SLACK_CALLBACK_URI.to_string()
        } else {
            format!("http://127.0.0.1:{port}/callback")
        }
    }

    fn uses_slack_callback(&self) -> bool {
        matches!(&self.config.transport, McpTransport::Http { url, .. }
            if url::Url::parse(url).is_ok_and(|url|
                url.scheme() == "https" && url.host_str() == Some("mcp.slack.com")
                && url.path().trim_end_matches('/') == "/mcp"))
    }

    /// Builds an `AuthorizationManager` with the keychain credential store and
    /// server metadata already discovered, so callers never depend on another
    /// method's discovery side effects.
    async fn new_oauth_manager(&self, oauth_base: &str) -> Result<AuthorizationManager, String> {
        let http_client =
            default_oauth_http_client().map_err(|e| format!("OAuth init failed: {e}"))?;
        let mut manager = AuthorizationManager::new_with_oauth_http_client(
            oauth_base.to_string(),
            std::sync::Arc::new(UserAgentOAuthHttpClient(http_client)),
        )
        .await
        .map_err(|e| format!("OAuth init failed: {e}"))?;
        manager.set_credential_store(KeychainCredentialStore::new(
            self.secrets.clone(),
            self.connector_id(),
        ));

        let resolution = manager
            .resolve_metadata()
            .await
            .map_err(|e| format!("OAuth metadata discovery failed: {e}"))?;
        manager.set_metadata(resolution.metadata);

        Ok(manager)
    }

    /// Configures an explicit-`client_id` (confidential) OAuth client, attaching
    /// the stored `client_secret` when one is present.
    ///
    /// Must be called after server metadata is set and, on the reconnect path,
    /// after `initialize_from_store`, which would otherwise install a secret-less
    /// client that clobbers this configuration.
    async fn configure_confidential_client(
        &self,
        manager: &mut AuthorizationManager,
        client_id: &str,
        scopes: &[String],
        redirect_uri: &str,
    ) -> Result<(), String> {
        let mut config = OAuthClientConfig::new(client_id.to_string(), redirect_uri.to_string())
            .with_scopes(scopes.to_vec());
        if let Some(secret) = self
            .secrets
            .load(&self.client_secret_key())
            .await?
            .filter(|v| !v.is_empty())
        {
            config = config.with_client_secret(secret);
        }
        manager
            .configure_client(config)
            .map_err(|e| format!("client configuration failed: {e}"))
    }

    fn append_authorize_params(
        auth_url: &str,
        params: &HashMap<String, String>,
    ) -> Result<String, String> {
        if params.is_empty() {
            return Ok(auth_url.to_string());
        }
        let mut parsed =
            url::Url::parse(auth_url).map_err(|e| format!("invalid authorization URL: {e}"))?;
        {
            let mut pairs = parsed.query_pairs_mut();
            for (key, value) in params {
                pairs.append_pair(key, value);
            }
        }
        Ok(parsed.to_string())
    }

    async fn authenticate_http(
        &self,
        oauth_base: &str,
        scopes: &[String],
        client_id: Option<&str>,
        authorize_params: &HashMap<String, String>,
    ) -> Result<(), AuthError> {
        let listener = if self.uses_slack_callback() {
            None
        } else {
            Some(bind_oauth_callback(&MCP_OAUTH_REDIRECT_PORTS).await?)
        };
        let redirect_uri = match &listener {
            Some(listener) => self.oauth_redirect_uri(
                listener
                    .local_addr()
                    .map_err(|e| AuthError::Failed(e.to_string()))?
                    .port(),
            ),
            None => SLACK_CALLBACK_URI.to_string(),
        };

        let mut manager = self
            .new_oauth_manager(oauth_base)
            .await
            .map_err(AuthError::Failed)?;

        let discovered_scopes;
        let scopes = if scopes.is_empty() {
            discovered_scopes = manager.select_scopes(None, &[]);
            &discovered_scopes
        } else {
            scopes
        };
        let scope_refs: Vec<&str> = scopes.iter().map(String::as_str).collect();

        match client_id {
            Some(client_id) => {
                self.configure_confidential_client(&mut manager, client_id, scopes, &redirect_uri)
                    .await
                    .map_err(AuthError::Failed)?;
            }
            None => {
                manager
                    .register_client("Lumo Desktop", &redirect_uri, &scope_refs)
                    .await
                    .map_err(|e| AuthError::Failed(format!("client registration failed: {e}")))?;
            }
        }

        let auth_url = manager
            .get_authorization_url(&scope_refs)
            .await
            .map_err(|e| AuthError::Failed(format!("authorization URL failed: {e}")))?;
        let auth_url = Self::append_authorize_params(&auth_url, authorize_params)
            .map_err(AuthError::Failed)?;
        let auth_url = browser_url(&auth_url)?.to_string();
        let (code, state, issuer) = match listener {
            Some(listener) => {
                open::that(&auth_url).map_err(|e| {
                    AuthError::Failed(format!("failed to open browser: {:?}", e.kind()))
                })?;
                wait_for_oauth_callback(listener).await?
            }
            None => {
                let callback = DeepLinkCallback::register(&auth_url)?;
                open::that(&auth_url).map_err(|e| {
                    AuthError::Failed(format!("failed to open browser: {:?}", e.kind()))
                })?;
                callback.wait().await?
            }
        };
        manager
            .exchange_code_for_token_with_issuer(&code, &state, issuer.as_deref())
            .await
            .map_err(|e| AuthError::Failed(format!("token exchange failed: {e}")))?;

        Ok(())
    }

    /// Ends the session and falls back to the cached tool list.
    async fn close_session(&self) {
        let service = self.client.lock().await.take();
        if let Some(service) = service {
            let _ = service.cancel().await;
        }
        *self
            .tools
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Self::cached_tools(&self.config.id, self.tool_cache.as_ref(), self.catalog);
    }

    /// Separates a server that answered with an error from one the session could not reach.
    fn describe_call_failure(&self, error: ServiceError) -> ToolError {
        match error {
            ServiceError::TransportSend(_)
            | ServiceError::TransportClosed
            | ServiceError::Cancelled { .. } => {
                ToolError::Unreachable(format!("{}: {error}", self.id))
            }
            other => ToolError::ExecutionFailed(format!("MCP tool call failed: {other}")),
        }
    }

    async fn call_tool(&self, tool_name: &str, input: Value) -> Result<Value, ToolError> {
        let guard = self.client.lock().await;
        let service = guard
            .as_ref()
            .ok_or_else(|| ToolError::Unreachable(format!("{}: no session", self.id)))?;

        let mut params = CallToolRequestParams::new(tool_name.to_string());
        if let Some(arguments) = input.as_object() {
            params = params.with_arguments(arguments.clone());
        }

        let result = tokio::time::timeout(self.call_timeout, service.call_tool(params))
            .await
            .map_err(|_| {
                ToolError::ExecutionFailed(format!(
                    "MCP tool '{tool_name}' timed out after {:?}",
                    self.call_timeout
                ))
            })?
            .map_err(|e| self.describe_call_failure(e))?;

        serde_json::to_value(result)
            .inspect_err(|_| mcp_result_serialization_failure())
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }
}

/// A tool counts as not sending data only when a vetted server marks it read-only and
/// closed-world. The MCP specification treats an absent `openWorldHint` as open.
pub(crate) fn risk(tool: Option<&ToolDefinition>, vetted: bool) -> ToolRisk {
    let read_only = tool.and_then(|tool| tool.read_only) == Some(true);
    let open_world = tool.and_then(|tool| tool.open_world) != Some(false);
    ToolRisk {
        sends_data: !vetted || !read_only || open_world,
        ..Default::default()
    }
}

#[async_trait]
impl Connector for McpConnector {
    fn id(&self) -> &str {
        &self.id
    }

    fn vetted(&self) -> bool {
        self.catalog
    }

    fn risk(&self, tool_name: &str) -> ToolRisk {
        let tools = self.tools();
        risk(
            tools.iter().find(|tool| tool.name == tool_name),
            self.catalog,
        )
    }

    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor {
            id: self.id.clone(),
            display_name: self.config.display_name.clone(),
            description: self.config.description.clone(),
            version: "1.0.0".into(),
            icon: bundled_icon_data_uri(&self.id),
            auth: AuthMechanism::McpServer {
                transport: match &self.config.transport {
                    McpTransport::Stdio {
                        command,
                        args,
                        env,
                        secret_env,
                    } => McpTransportInfo::Stdio {
                        command: command.clone(),
                        args: args.clone(),
                        env: env.clone(),
                        secret_env: secret_env.clone(),
                    },
                    McpTransport::Http { url, .. } => McpTransportInfo::Http { url: url.clone() },
                },
            },
        }
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        self.tools
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn readiness(&self) -> Readiness {
        match &self.config.transport {
            McpTransport::Stdio { secret_env, .. } if secret_env.is_empty() => Readiness::Ready,
            McpTransport::Http {
                auth: HttpAuth::None,
                ..
            } => Readiness::Ready,
            _ => match self.credentials().blocked_reason() {
                Some(reason) => Readiness::blocked(reason),
                None => Readiness::Ready,
            },
        }
    }

    async fn invoke(&self, tool_name: &str, input: Value) -> Result<Value, ToolError> {
        self.ensure_connected().await?;
        self.call_tool(tool_name, input).await
    }

    async fn invoke_read_only(&self, tool_name: &str, input: Value) -> Result<Value, ToolError> {
        // Reconnection replaces cached metadata. Check the live catalog before calling.
        self.ensure_connected().await?;
        self.check_read_only(tool_name)?;
        self.call_tool(tool_name, input).await
    }

    /// A stdio server holds credentials only where its config declares secret environment
    /// variables; the values are supplied through the form and can be withdrawn.
    fn as_credentialed(&self) -> Option<&dyn Credentialed> {
        match &self.config.transport {
            McpTransport::Stdio { secret_env, .. } if secret_env.is_empty() => None,
            McpTransport::Http {
                auth: HttpAuth::None,
                ..
            } => None,
            _ => Some(self),
        }
    }

    fn as_session(&self) -> Option<&dyn Session> {
        Some(self)
    }
}

#[async_trait]
impl Credentialed for McpConnector {
    async fn authenticate(
        &self,
        input: Option<Value>,
        mode: Interactive,
    ) -> Result<Stored, AuthError> {
        let supplied = McpCredentialInput::parse(input)?;

        let wrote = self.store_supplied(&supplied).await?;
        let state = credential_state(&self.config, self.secrets.as_ref()).await;
        self.set_credentials(state);
        match state {
            CredentialState::Present => return Ok(wrote),
            CredentialState::Unreadable => {
                return Err(AuthError::Failed(
                    "secure storage is unavailable, so stored credentials could not be read".into(),
                ))
            }
            CredentialState::Absent => {}
        }

        // Only an HTTP transport has a flow to start: a secret environment variable is supplied
        // through the form or not at all.
        let McpTransport::Http { url, auth, .. } = &self.config.transport else {
            return Err(AuthError::RequiresSetup);
        };

        match auth {
            HttpAuth::Oauth {
                scopes,
                client_id,
                resource: _,
                authorize_params,
            } => {
                if mode == Interactive::No {
                    return Err(AuthError::RequiresSetup);
                }
                self.authenticate_http(url, scopes, client_id.as_deref(), authorize_params)
                    .await?;
                self.set_credentials(CredentialState::Present);
                Ok(Stored::Something)
            }
            HttpAuth::Token { .. } | HttpAuth::Headers { .. } => Err(AuthError::RequiresSetup),
            HttpAuth::None => Ok(wrote),
        }
    }

    async fn forget(&self) -> Result<(), String> {
        match &self.config.transport {
            McpTransport::Stdio { secret_env, .. } => {
                for name in secret_env {
                    self.secrets.delete(&self.env_secret_key(name)).await?;
                }
            }
            McpTransport::Http { auth, .. } => {
                self.secrets.delete(&self.id).await?;
                self.secrets.delete(&self.token_key()).await?;
                if let HttpAuth::Headers { headers } = auth {
                    for header in headers {
                        self.secrets
                            .delete(&self.header_secret_key(&header.name))
                            .await?;
                    }
                }
            }
        }
        self.set_credentials(CredentialState::Absent);
        Ok(())
    }
}

#[async_trait]
impl Session for McpConnector {
    async fn ensure(&self) -> Result<(), ToolError> {
        self.ensure_connected().await
    }

    async fn drop_session(&self) {
        self.close_session().await;
    }
}

pub async fn build_mcp_connectors(
    app_handle: &AppHandle,
    account: &AccountId,
) -> Result<Vec<McpConnector>, String> {
    let secrets = keychain_secrets(account.clone());
    let mut configs = load_mcp_servers(app_handle, account)?;
    let migrated = migrate_legacy_endpoints(&mut configs, secrets.as_ref()).await;
    if !migrated.is_empty() {
        match save_mcp_servers(app_handle, account, &configs) {
            Ok(()) => {
                for id in &migrated {
                    info!("{id}: moved to the current MCP endpoint; reconnect required");
                }
            }
            Err(e) => warn!("failed to save migrated MCP servers, will retry: {e}"),
        }
    }
    let mut connectors = Vec::with_capacity(configs.len());
    for config in crate::feature_flags::filter_mcp_presets(app_handle, configs) {
        connectors
            .push(McpConnector::from_config(config, secrets.clone(), app_handle, account).await);
    }
    Ok(connectors)
}

#[cfg(test)]
mod tests;
