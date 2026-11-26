//! The tools the connector offers and their input schemas.

use super::{DEFAULT_MESSAGES, DEFAULT_RECENT_DAYS, MAX_MESSAGES, MAX_RECENT_DAYS};
use crate::tools::registry::ToolDefinition;
use serde_json::{json, Value};

pub(super) fn tool(
    name: &str,
    title: &str,
    description: &str,
    input_schema: Value,
) -> ToolDefinition {
    ToolDefinition {
        read_only: match name {
            "list_mailboxes" | "list_messages" | "search_messages" | "read_message" => Some(true),
            _ => None,
        },
        name: name.into(),
        title: Some(title.into()),
        description: description.into(),
        input_schema,
        ..Default::default()
    }
}

const MAILBOX_DESCRIPTION: &str = "Exact mailbox name as returned by list_mailboxes";

pub(super) fn mailboxes_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

pub(super) fn list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "mailbox": { "type": "string", "default": "INBOX", "description": MAILBOX_DESCRIPTION },
            "limit": { "type": "integer", "minimum": 1, "maximum": MAX_MESSAGES, "default": DEFAULT_MESSAGES },
            "days": { "type": "integer", "minimum": 1, "maximum": MAX_RECENT_DAYS, "default": DEFAULT_RECENT_DAYS, "description": "Only include messages received within this many days" }
        }
    })
}

pub(super) fn search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "mailbox": { "type": "string", "default": "INBOX", "description": MAILBOX_DESCRIPTION },
            "text": { "type": "string" },
            "from": { "type": "string" },
            "to": { "type": "string" },
            "subject": { "type": "string" },
            "since": { "type": "string", "description": "Inclusive date in YYYY-MM-DD format" },
            "before": { "type": "string", "description": "Exclusive date in YYYY-MM-DD format" },
            "unread": { "type": "boolean" },
            "limit": { "type": "integer", "minimum": 1, "maximum": MAX_MESSAGES, "default": DEFAULT_MESSAGES }
        }
    })
}

pub(super) fn read_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "mailbox": { "type": "string", "default": "INBOX", "description": MAILBOX_DESCRIPTION },
            "uid": { "type": "integer", "minimum": 1 },
            "format": { "type": "string", "enum": ["text", "html", "both"], "default": "text", "description": "Body format to return; HTML is returned as source and is not rendered" }
        },
        "required": ["uid"]
    })
}

pub(super) fn send_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "to": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
            "cc": { "type": "array", "items": { "type": "string" } },
            "bcc": { "type": "array", "items": { "type": "string" } },
            "subject": { "type": "string" },
            "body": { "type": "string", "description": "Required plain-text body and fallback for HTML-capable clients" },
            "html_body": { "type": "string", "description": "Optional HTML alternative" }
        },
        "required": ["to", "subject", "body"]
    })
}
