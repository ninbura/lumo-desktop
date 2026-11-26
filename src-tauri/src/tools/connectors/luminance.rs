//! Luminance Public API v2, spoken directly: Luminance publishes no MCP server.
//!
//! - `config`: which instance to talk to, and where that choice is kept.
//! - `api`: token exchange and turning HTTP responses into tool results.
//! - `operations`: the tool table generated from Luminance's OpenAPI spec.

mod api;
mod config;
mod operations;

use super::super::auth::{
    keychain_secrets, AuthMechanism, CredentialState, Interactive, LuminanceInfo, Readiness,
    Secrets, Stored,
};
use super::super::error::{AuthError, ToolError};
use super::super::exposure::{Exposure, ToolRisk};
use super::super::icons::bundled_icon_data_uri;
use super::super::registry::{Connector, ConnectorDescriptor, Credentialed, ToolDefinition};
use crate::account::AccountId;
use api::{
    describe_error_status, exchange_client_credentials, file_result, header_str, read_capped,
    CachedToken, USER_AGENT_VALUE,
};
use async_trait::async_trait;
use config::{instance_origin, AccountConfigFile, ConfigFile, NoConfigFile};
pub use config::{LuminanceAuth, LuminanceConfig};
use log::warn;
use operations::{
    build_request, tool_definition, ApiRequest, Operation, RequestBody, ResponseKind, OPERATIONS,
};
use reqwest::header::{ACCEPT, CONTENT_DISPOSITION, CONTENT_TYPE, USER_AGENT};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::RwLock;
use std::time::Duration;
use tauri::AppHandle;
use tokio::sync::Mutex;

pub const CONNECTOR_ID: &str = "luminance";
const CLIENT_ID_SECRET: &str = "luminance:client_id";
const CLIENT_SECRET_SECRET: &str = "luminance:client_secret";
const ACCESS_TOKEN_SECRET: &str = "luminance:access_token";
const VERIFY_PATH: &str = "/api2/users/me";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
struct SetupInput {
    instance: String,
    auth: LuminanceAuth,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    client_secret: String,
    #[serde(default)]
    access_token: String,
}

pub struct LuminanceConnector {
    secrets: Secrets,
    storage: Box<dyn ConfigFile>,
    http: reqwest::Client,
    config: RwLock<Option<LuminanceConfig>>,
    credentials: RwLock<CredentialState>,
    token: Mutex<Option<CachedToken>>,
}

impl LuminanceConnector {
    pub fn new(
        storage: Box<dyn ConfigFile>,
        secrets: Secrets,
        credentials: CredentialState,
    ) -> Self {
        let config = storage
            .load()
            .map_err(|e| warn!("failed to load Luminance config: {e}"))
            .ok()
            .flatten();
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client builds");
        Self {
            secrets,
            storage,
            http,
            config: RwLock::new(config),
            credentials: RwLock::new(credentials),
            token: Mutex::new(None),
        }
    }

    /// Probes the keychain once, which is what keeps [`Connector::readiness`] synchronous.
    pub async fn from_account(app_handle: AppHandle, account: AccountId) -> Self {
        let secrets = keychain_secrets(account.clone());
        let storage = AccountConfigFile {
            app_handle,
            account,
        };
        let auth = storage.load().ok().flatten().map(|config| config.auth);
        let credentials = match auth {
            Some(auth) => stored_credential_state(&secrets, auth).await,
            None => CredentialState::Absent,
        };
        Self::new(Box::new(storage), secrets, credentials)
    }

    fn current_config(&self) -> Option<LuminanceConfig> {
        self.config
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_config(&self, config: Option<LuminanceConfig>) -> Result<(), String> {
        *self
            .config
            .write()
            .map_err(|_| "Luminance config lock poisoned".to_string())? = config;
        Ok(())
    }

    fn set_credentials(&self, state: CredentialState) {
        *self
            .credentials
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
    }

    async fn load_secret(&self, name: &str) -> Result<String, ToolError> {
        self.secrets
            .load(name)
            .await
            .map_err(ToolError::ExecutionFailed)?
            .filter(|value| !value.is_empty())
            .ok_or(ToolError::NotAuthenticated)
    }

    /// A bearer token for `config`, exchanging the client credentials when the cached one is
    /// missing, near expiry, or `refresh` is set.
    async fn bearer(&self, config: &LuminanceConfig, refresh: bool) -> Result<String, ToolError> {
        match config.auth {
            LuminanceAuth::AccessToken => self.load_secret(ACCESS_TOKEN_SECRET).await,
            LuminanceAuth::ClientCredentials => {
                let mut cached = self.token.lock().await;
                if !refresh {
                    if let Some(token) = cached.as_ref().filter(|token| token.is_fresh()) {
                        return Ok(token.value.clone());
                    }
                }
                let client_id = self.load_secret(CLIENT_ID_SECRET).await?;
                let client_secret = self.load_secret(CLIENT_SECRET_SECRET).await?;
                let token = exchange_client_credentials(
                    &self.http,
                    &config.instance_url,
                    &client_id,
                    &client_secret,
                )
                .await?;
                let value = token.value.clone();
                *cached = Some(token);
                Ok(value)
            }
        }
    }

    /// Sends one API request, retrying once with a fresh token when a client-credentials token
    /// is refused.
    async fn send(
        &self,
        config: &LuminanceConfig,
        request: &ApiRequest,
    ) -> Result<reqwest::Response, ToolError> {
        let token = self.bearer(config, false).await?;
        let response = self.send_once(config, request, &token).await?;
        if response.status() != StatusCode::UNAUTHORIZED
            || config.auth != LuminanceAuth::ClientCredentials
        {
            return Ok(response);
        }
        let token = self.bearer(config, true).await?;
        self.send_once(config, request, &token).await
    }

    async fn send_once(
        &self,
        config: &LuminanceConfig,
        request: &ApiRequest,
        token: &str,
    ) -> Result<reqwest::Response, ToolError> {
        let mut url = url::Url::parse(&format!("{}{}", config.instance_url, request.path))
            .map_err(|e| ToolError::InvalidInput(format!("invalid request path: {e}")))?;
        if !request.query.is_empty() {
            url.query_pairs_mut().extend_pairs(&request.query);
        }
        let mut builder = self
            .http
            .request(request.method.clone(), url)
            .bearer_auth(token)
            .header(USER_AGENT, USER_AGENT_VALUE)
            .header(ACCEPT, "application/json, */*");
        builder = match &request.body {
            RequestBody::None => builder,
            RequestBody::Json(value) => builder.json(value),
            RequestBody::Binary(bytes) => builder
                .header(CONTENT_TYPE, "application/octet-stream")
                .body(bytes.clone()),
        };
        builder
            .send()
            .await
            .map_err(|e| ToolError::Unreachable(format!("Luminance: {e}")))
    }

    async fn verify(&self, config: &LuminanceConfig) -> Result<(), String> {
        let request = ApiRequest {
            method: Method::GET,
            path: VERIFY_PATH.into(),
            query: Vec::new(),
            body: RequestBody::None,
        };
        let response = self.send(config, &request).await.map_err(|e| match e {
            ToolError::NotAuthenticated => "credentials are missing".to_string(),
            other => other.to_string(),
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body = response.text().await.unwrap_or_default();
        Err(match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => match config.auth {
                LuminanceAuth::AccessToken => "Luminance rejected the access token. Access tokens expire; generate a new one, or connect with a Client ID and Secret so Lumo can renew it.".into(),
                LuminanceAuth::ClientCredentials => "Luminance issued a token but refused it. Check that the service user can use the Public API.".into(),
            },
            _ => describe_error_status(status, &body),
        })
    }

    async fn call(&self, operation: &Operation, input: Value) -> Result<Value, ToolError> {
        let config = self.current_config().ok_or(ToolError::NotAuthenticated)?;
        let request = build_request(operation, &input)?;
        let response = self.send(&config, &request).await?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(ToolError::ExecutionFailed(match config.auth {
                LuminanceAuth::AccessToken => "Luminance rejected the access token, which has probably expired. Reconnect Luminance in Settings with a new token, or with a Client ID and Secret.".into(),
                LuminanceAuth::ClientCredentials => "Luminance refused a freshly issued token. Check the service user's API access.".into(),
            }));
        }
        let content_type = header_str(&response, CONTENT_TYPE);
        let disposition = header_str(&response, CONTENT_DISPOSITION);
        let bytes = read_capped(response).await?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(describe_error_status(
                status,
                &String::from_utf8_lossy(&bytes),
            )));
        }
        if bytes.is_empty() {
            return Ok(json!({ "status": status.as_u16() }));
        }
        if operation.response == ResponseKind::Binary {
            return Ok(file_result(content_type, disposition, &bytes));
        }
        Ok(serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({ "text": String::from_utf8_lossy(&bytes) })))
    }
}

async fn stored_credential_state(secrets: &Secrets, auth: LuminanceAuth) -> CredentialState {
    match auth {
        LuminanceAuth::AccessToken => CredentialState::of(secrets.load(ACCESS_TOKEN_SECRET).await),
        LuminanceAuth::ClientCredentials => {
            CredentialState::of(secrets.load(CLIENT_ID_SECRET).await).and(CredentialState::of(
                secrets.load(CLIENT_SECRET_SECRET).await,
            ))
        }
    }
}

/// Write operations can share documents with third parties, so they count as sending data.
pub(crate) fn risk(read_only: bool) -> ToolRisk {
    ToolRisk {
        exposes: Exposure::PRIVATE,
        sends_data: !read_only,
        changes_files: false,
    }
}

#[async_trait]
impl Connector for LuminanceConnector {
    fn id(&self) -> &str {
        CONNECTOR_ID
    }

    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor {
            id: CONNECTOR_ID.into(),
            display_name: "Luminance".into(),
            version: "1.0.0".into(),
            description: None,
            icon: bundled_icon_data_uri(CONNECTOR_ID),
            auth: AuthMechanism::Luminance {
                config: self.current_config().map(|config| LuminanceInfo {
                    instance_url: config.instance_url,
                    auth: match config.auth {
                        LuminanceAuth::ClientCredentials => "client_credentials".into(),
                        LuminanceAuth::AccessToken => "access_token".into(),
                    },
                }),
            },
        }
    }

    fn readiness(&self) -> Readiness {
        if self.current_config().is_none() {
            return Readiness::blocked("Not connected");
        }
        match self
            .credentials
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .blocked_reason()
        {
            Some(reason) => Readiness::blocked(reason),
            None => Readiness::Ready,
        }
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        OPERATIONS.iter().map(tool_definition).collect()
    }

    fn risk(&self, tool_name: &str) -> ToolRisk {
        let read_only = OPERATIONS
            .iter()
            .find(|operation| operation.name == tool_name)
            .is_some_and(|operation| tool_definition(operation).read_only == Some(true));
        risk(read_only)
    }

    async fn invoke(&self, tool_name: &str, input: Value) -> Result<Value, ToolError> {
        let operation = OPERATIONS
            .iter()
            .find(|operation| operation.name == tool_name)
            .ok_or_else(|| ToolError::UnknownTool(tool_name.into()))?;
        self.call(operation, input).await
    }

    fn as_credentialed(&self) -> Option<&dyn Credentialed> {
        Some(self)
    }
}

#[async_trait]
impl Credentialed for LuminanceConnector {
    async fn authenticate(
        &self,
        input: Option<Value>,
        _mode: Interactive,
    ) -> Result<Stored, AuthError> {
        let Some(input) = input else {
            let config = self.current_config().ok_or(AuthError::RequiresSetup)?;
            let state = stored_credential_state(&self.secrets, config.auth).await;
            self.set_credentials(state);
            match state {
                CredentialState::Present => {}
                CredentialState::Absent => return Err(AuthError::RequiresSetup),
                CredentialState::Unreadable => {
                    return Err(AuthError::Failed(
                        "secure storage is unavailable, so the stored Luminance credentials could not be read".into(),
                    ))
                }
            }
            self.verify(&config).await.map_err(AuthError::Failed)?;
            return Ok(Stored::Nothing);
        };

        let setup: SetupInput = serde_json::from_value(input)
            .map_err(|e| AuthError::Failed(format!("invalid Luminance settings: {e}")))?;
        let config = LuminanceConfig {
            instance_url: instance_origin(&setup.instance).map_err(AuthError::Failed)?,
            auth: setup.auth,
        };
        let previous = self.current_config();
        let keep_stored = previous.as_ref().is_some_and(|p| p == &config);
        let supplied: Vec<(&str, String)> = match config.auth {
            LuminanceAuth::AccessToken => {
                vec![(ACCESS_TOKEN_SECRET, setup.access_token.trim().to_string())]
            }
            LuminanceAuth::ClientCredentials => vec![
                (CLIENT_ID_SECRET, setup.client_id.trim().to_string()),
                (CLIENT_SECRET_SECRET, setup.client_secret.trim().to_string()),
            ],
        };
        // Blank fields keep what is stored, but only for the same instance and sign-in method:
        // credentials for one instance are never sent to another.
        let mut resolved = Vec::new();
        for (name, value) in &supplied {
            let value = if value.is_empty() && keep_stored {
                self.secrets
                    .load(name)
                    .await
                    .map_err(AuthError::Failed)?
                    .unwrap_or_default()
            } else {
                value.clone()
            };
            if value.is_empty() {
                return Err(AuthError::Failed(match config.auth {
                    LuminanceAuth::AccessToken => "enter an access token".into(),
                    LuminanceAuth::ClientCredentials => {
                        "enter both the Client ID and the Client Secret".into()
                    }
                }));
            }
            resolved.push((*name, value));
        }

        let probe = LuminanceConnector::new(
            Box::new(NoConfigFile),
            std::sync::Arc::new(ProbeSecrets(resolved.clone())),
            CredentialState::Present,
        );
        probe
            .set_config(Some(config.clone()))
            .map_err(AuthError::Failed)?;
        probe.verify(&config).await.map_err(AuthError::Failed)?;

        self.storage.save(&config).map_err(AuthError::Failed)?;
        let mut wrote = Stored::Nothing;
        for ((name, value), (_, typed)) in resolved.iter().zip(&supplied) {
            if !typed.is_empty() || !keep_stored {
                self.secrets
                    .store(name, value)
                    .await
                    .map_err(AuthError::Failed)?;
                wrote = Stored::Something;
            }
        }
        let unused: &[&str] = match config.auth {
            LuminanceAuth::AccessToken => &[CLIENT_ID_SECRET, CLIENT_SECRET_SECRET],
            LuminanceAuth::ClientCredentials => &[ACCESS_TOKEN_SECRET],
        };
        for name in unused {
            self.secrets.delete(name).await.map_err(AuthError::Failed)?;
        }
        *self.token.lock().await = probe.token.lock().await.take();
        self.set_config(Some(config)).map_err(AuthError::Failed)?;
        self.set_credentials(CredentialState::Present);
        Ok(wrote)
    }

    async fn forget(&self) -> Result<(), String> {
        for name in [CLIENT_ID_SECRET, CLIENT_SECRET_SECRET, ACCESS_TOKEN_SECRET] {
            self.secrets.delete(name).await?;
        }
        self.storage.delete()?;
        *self.token.lock().await = None;
        self.set_config(None)?;
        self.set_credentials(CredentialState::Absent);
        Ok(())
    }
}

/// Holds credentials typed into the form while they are tested, so nothing reaches the keychain
/// before Luminance has accepted them.
struct ProbeSecrets(Vec<(&'static str, String)>);

#[async_trait]
impl super::super::auth::SecretStore for ProbeSecrets {
    async fn store(&self, _name: &str, _secret: &str) -> Result<(), String> {
        Ok(())
    }

    async fn load(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self
            .0
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone()))
    }

    async fn delete(&self, _name: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::auth::InMemorySecrets;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    struct StoredConfig(Option<LuminanceConfig>);

    impl ConfigFile for StoredConfig {
        fn load(&self) -> Result<Option<LuminanceConfig>, String> {
            Ok(self.0.clone())
        }

        fn save(&self, _config: &LuminanceConfig) -> Result<(), String> {
            Ok(())
        }

        fn delete(&self) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn readiness_needs_both_the_instance_and_its_credentials() {
        let config = LuminanceConfig {
            instance_url: "https://acme.app.luminance.com".into(),
            auth: LuminanceAuth::AccessToken,
        };
        let ready = LuminanceConnector::new(
            Box::new(StoredConfig(Some(config.clone()))),
            InMemorySecrets::with(&[(ACCESS_TOKEN_SECRET, "t")]),
            CredentialState::Present,
        );
        assert_eq!(ready.readiness(), Readiness::Ready);

        let no_secret = LuminanceConnector::new(
            Box::new(StoredConfig(Some(config))),
            InMemorySecrets::empty(),
            CredentialState::Absent,
        );
        assert_eq!(no_secret.readiness(), Readiness::blocked("Not connected"));

        let no_config = LuminanceConnector::new(
            Box::new(StoredConfig(None)),
            InMemorySecrets::with(&[(ACCESS_TOKEN_SECRET, "t")]),
            CredentialState::Present,
        );
        assert_eq!(no_config.readiness(), Readiness::blocked("Not connected"));
    }

    /// Answers each request with the next canned response, recording what was asked.
    async fn serve(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 16 * 1024];
                let n = socket.read(&mut buf).await.unwrap();
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).to_string());
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{addr}"), seen)
    }

    #[tokio::test]
    async fn a_refused_client_credentials_token_is_renewed_once() {
        let (base, seen) = serve(vec![
            ("200 OK", r#"{"access_token":"first","expires_in":3600}"#),
            ("401 Unauthorized", ""),
            ("200 OK", r#"{"access_token":"second","expires_in":3600}"#),
            ("200 OK", r#"{"id":1,"name":"Ada"}"#),
        ])
        .await;
        let config = LuminanceConfig {
            instance_url: base,
            auth: LuminanceAuth::ClientCredentials,
        };
        let connector = LuminanceConnector::new(
            Box::new(StoredConfig(Some(config))),
            InMemorySecrets::with(&[(CLIENT_ID_SECRET, "id"), (CLIENT_SECRET_SECRET, "secret")]),
            CredentialState::Present,
        );

        let result = connector
            .invoke("get_user_by_id", json!({ "id": 1 }))
            .await
            .expect("retried call succeeds");
        assert_eq!(result["name"], "Ada");

        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[0].starts_with("POST /auth/oauth2/token"));
        assert!(requests[0].contains("grant_type=client_credentials"));
        assert!(
            requests[1].contains("authorization: Bearer first"),
            "{}",
            requests[1]
        );
        assert!(
            requests[3].contains("authorization: Bearer second"),
            "{}",
            requests[3]
        );
    }

    #[tokio::test]
    async fn a_fresh_token_is_reused_across_calls() {
        let (base, seen) = serve(vec![
            ("200 OK", r#"{"access_token":"only","expires_in":3600}"#),
            ("200 OK", r#"{"result":[]}"#),
            ("200 OK", r#"{"result":[]}"#),
        ])
        .await;
        let config = LuminanceConfig {
            instance_url: base,
            auth: LuminanceAuth::ClientCredentials,
        };
        let connector = LuminanceConnector::new(
            Box::new(StoredConfig(Some(config))),
            InMemorySecrets::with(&[(CLIENT_ID_SECRET, "id"), (CLIENT_SECRET_SECRET, "secret")]),
            CredentialState::Present,
        );
        connector.invoke("list_users", json!({})).await.unwrap();
        connector.invoke("list_users", json!({})).await.unwrap();
        let token_requests = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.contains("/auth/oauth2/token"))
            .count();
        assert_eq!(token_requests, 1);
    }

    #[tokio::test]
    async fn an_expired_pasted_token_says_how_to_fix_it() {
        let (base, _) = serve(vec![("401 Unauthorized", "")]).await;
        let config = LuminanceConfig {
            instance_url: base,
            auth: LuminanceAuth::AccessToken,
        };
        let connector = LuminanceConnector::new(
            Box::new(StoredConfig(Some(config))),
            InMemorySecrets::with(&[(ACCESS_TOKEN_SECRET, "stale")]),
            CredentialState::Present,
        );
        let err = connector.invoke("list_users", json!({})).await.unwrap_err();
        assert!(err.to_string().contains("expired"), "{err}");
    }

    #[tokio::test]
    async fn forgetting_clears_the_instance_and_every_secret() {
        let config = LuminanceConfig {
            instance_url: "https://acme.app.luminance.com".into(),
            auth: LuminanceAuth::ClientCredentials,
        };
        let secrets =
            InMemorySecrets::with(&[(CLIENT_ID_SECRET, "id"), (CLIENT_SECRET_SECRET, "s")]);
        let connector = LuminanceConnector::new(
            Box::new(StoredConfig(Some(config))),
            secrets.clone(),
            CredentialState::Present,
        );
        connector.forget().await.unwrap();
        assert_eq!(connector.readiness(), Readiness::blocked("Not connected"));
        assert_eq!(secrets.load(CLIENT_ID_SECRET).await.unwrap(), None);
        assert_eq!(secrets.load(CLIENT_SECRET_SECRET).await.unwrap(), None);
    }

    /// Run with `LUMINANCE_TEST_FILE=~/.luminance-test cargo test live_luminance -- --ignored`.
    /// Line 1 is the moniker, line 2 an access token; lines 3–4 optionally a Client ID and Secret.
    #[tokio::test]
    #[ignore]
    async fn live_luminance_smoke_test() {
        let path = std::env::var("LUMINANCE_TEST_FILE").expect("LUMINANCE_TEST_FILE");
        let lines: Vec<String> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| l.trim().to_string())
            .collect();
        let instance = instance_origin(&lines[0]).unwrap();
        let (auth, secrets) = match (lines.get(2), lines.get(3)) {
            (Some(id), Some(secret)) if !id.is_empty() && !secret.is_empty() => (
                LuminanceAuth::ClientCredentials,
                InMemorySecrets::with(&[(CLIENT_ID_SECRET, id), (CLIENT_SECRET_SECRET, secret)]),
            ),
            _ => (
                LuminanceAuth::AccessToken,
                InMemorySecrets::with(&[(ACCESS_TOKEN_SECRET, &lines[1])]),
            ),
        };
        let config = LuminanceConfig {
            instance_url: instance,
            auth,
        };
        let connector = LuminanceConnector::new(
            Box::new(StoredConfig(Some(config.clone()))),
            secrets,
            CredentialState::Present,
        );
        connector
            .verify(&config)
            .await
            .expect("credentials accepted");
        let projects = connector
            .invoke("list_projects_authenticated_user_can_access", json!({}))
            .await
            .expect("projects listed");
        println!(
            "projects response keys: {:?}",
            projects.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
    }
}
