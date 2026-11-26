//! Provider-specific upgrades for saved MCP server definitions.

use super::config::{HttpAuth, McpServerConfig, McpTransport};
use super::credentials::on_remove;
use crate::tools::auth::SecretStore;
use log::warn;

pub const ATLASSIAN_MCP_URL: &str = "https://mcp.atlassian.com/v2/mcp";

/// Mirrors the `mcp-atlassian` preset in `webview/pages/settings/index.js`. v2 grants only the
/// scopes requested, and `offline_access` is what yields a refresh token.
pub const ATLASSIAN_MCP_SCOPES: &[&str] = &[
    "read:me",
    "read:account",
    "offline_access",
    "read:jira:agent-interface",
    "write:jira:agent-interface",
    "search:jira:agent-interface",
    "read:confluence:agent-interface",
    "write:confluence:agent-interface",
    "search:confluence:agent-interface",
    "search:rovo:agent-interface",
];

/// Atlassian removes the v1 endpoints in March 2027.
fn is_legacy_atlassian_url(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| {
        url.host_str() == Some("mcp.atlassian.com")
            && matches!(url.path().trim_end_matches('/'), "/v1/mcp" | "/v1/sse")
    })
}

/// Moves OAuth servers on a legacy Atlassian endpoint to v2, returning the ids it changed.
///
/// v2 is served by a different authorization server. Clear all of each server's credentials,
/// binding included, before changing its config; failures retain the legacy URL so the next
/// launch retries migration.
pub async fn migrate_legacy_endpoints(
    servers: &mut [McpServerConfig],
    secrets: &dyn SecretStore,
) -> Vec<String> {
    let mut migrated = Vec::new();
    for server in servers {
        if let McpTransport::Http {
            url,
            auth: HttpAuth::Oauth { scopes, .. },
            ..
        } = &mut server.transport
        {
            if is_legacy_atlassian_url(url) {
                if let Err(e) = on_remove(secrets, &server.id, &[]).await {
                    warn!(
                        "{}: failed to drop legacy OAuth credentials, will retry: {e}",
                        server.id
                    );
                    continue;
                }
                *url = ATLASSIAN_MCP_URL.to_string();
                *scopes = ATLASSIAN_MCP_SCOPES.iter().map(|s| s.to_string()).collect();
                migrated.push(server.id.clone());
            }
        }
    }
    migrated
}
