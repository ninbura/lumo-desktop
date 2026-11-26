//! Saved MCP server definitions and the per-account file that holds them.

use crate::account::AccountId;
use crate::reporting::config_failure;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tauri::AppHandle;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    pub display_name: String,
    /// Optional public capability summary, available before tool discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub transport: McpTransport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        /// Names of environment variables whose values are held in the keychain under
        /// `mcp-<id>:env:<NAME>` instead of in `env`. The keychain value takes precedence over an
        /// entry of the same name in `env`.
        #[serde(default)]
        secret_env: Vec<String>,
    },
    Http {
        url: String,
        auth: HttpAuth,
        /// Static headers sent on every MCP request (e.g. Grafana `X-Grafana-URL`).
        #[serde(default)]
        request_headers: HashMap<String, String>,
    },
}

/// Auth strategy for an HTTP MCP transport
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HttpAuth {
    /// MCP-spec OAuth. The client always sends PKCE, as required by OAuth 2.1;
    /// non-compliant servers without PKCE support are not specifically handled.
    ///
    /// When `client_id` is absent, the client is registered dynamically. When
    /// present, it identifies a pre-registered client, which is public unless a
    /// `client_secret` is in the keychain, in which case the client is confidential.
    Oauth {
        #[serde(default)]
        scopes: Vec<String>,
        #[serde(default)]
        client_id: Option<String>,
        #[serde(default)]
        resource: Option<String>,
        /// Provider-specific query params appended to the authorization URL to
        /// request offline access (a refresh token) e.g. Dropbox
        /// (`token_access_type=offline`)
        #[serde(default)]
        authorize_params: HashMap<String, String>,
    },
    /// Static `Authorization: <scheme> <secret>` header. `scheme` is e.g.
    /// `Bearer` or `Basic`; the secret itself lives in the keychain.
    Token { scheme: String },
    /// Several static headers at once (e.g. `Authorization: Bearer <token>` plus
    /// `x-api-key: <key>`); each secret value lives in the keychain under
    /// `mcp-<id>:header:<name>`.
    Headers { headers: Vec<CustomHeader> },
    /// No auth (e.g. Figma desktop MCP on loopback).
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomHeader {
    pub name: String,
    /// Value prepended to the keychain secret, e.g. `Bearer ` for a bearer
    /// token. Empty for headers whose value is the raw secret (e.g. `x-api-key`).
    #[serde(default)]
    pub prefix: String,
}

/// Rejects ids that would escape `mcp_tools/` or collide with another server's keychain keys.
pub fn validate_server_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
    {
        return Err(format!("invalid MCP server id: {id:?}"));
    }
    Ok(())
}

/// Registry id of a saved server, and the prefix of its keychain keys.
pub fn connector_id(server_id: &str) -> String {
    format!("mcp-{server_id}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct McpServersFile {
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

fn mcp_servers_path(
    app_handle: &AppHandle,
    account: &AccountId,
) -> Result<std::path::PathBuf, String> {
    crate::settings::account_config_dir(app_handle, account).map(|dir| dir.join("mcp_servers.json"))
}

pub fn load_mcp_servers(
    app_handle: &AppHandle,
    account: &AccountId,
) -> Result<Vec<McpServerConfig>, String> {
    let path = mcp_servers_path(app_handle, account)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file: McpServersFile = serde_json::from_str(&content)
        .inspect_err(|_| config_failure("MCP configuration load failed"))
        .map_err(|e| e.to_string())?;
    Ok(file.servers)
}

pub fn save_mcp_servers(
    app_handle: &AppHandle,
    account: &AccountId,
    servers: &[McpServerConfig],
) -> Result<(), String> {
    let path = mcp_servers_path(app_handle, account)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = McpServersFile {
        servers: servers.to_vec(),
    };
    let content = serde_json::to_string_pretty(&file)
        .inspect_err(|_| config_failure("MCP configuration save failed"))
        .map_err(|e| e.to_string())?;
    std::fs::write(path, content).map_err(|e| e.to_string())
}
