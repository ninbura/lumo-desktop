use crate::tools::auth::{AuthMechanism, Readiness};
use crate::tools::error::ToolError;
use crate::tools::exposure::{Exposure, RequestUrl, ToolRisk};
use crate::tools::icons::bundled_icon_data_uri;
use crate::tools::registry::{Connector, ConnectorDescriptor, ToolDefinition};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use reqwest::{Client, Method};
use scraper::{ElementRef, Html, Node, Selector};
use serde_json::{json, Value};
use std::sync::LazyLock;
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
/// Cap extracted page text returned to the model (avoids blowing context on huge pages).
const MAX_TEXT_CHARS: usize = 100_000;
const USER_AGENT_VALUE: &str = "Lumo-Desktop/1.0";

pub struct BrowserConnector {
    http: Client,
}

impl BrowserConnector {
    pub fn new() -> Self {
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()
                .expect("failed to build HTTP client"),
        }
    }

    fn validate_url(url: &str) -> Result<(), ToolError> {
        let parsed = url::Url::parse(url)
            .map_err(|e| ToolError::InvalidInput(format!("invalid URL: {e}")))?;
        match parsed.scheme() {
            "http" | "https" => Ok(()),
            other => Err(ToolError::InvalidInput(format!(
                "unsupported URL scheme: {other} (only http/https allowed)"
            ))),
        }
    }

    fn parse_headers(input: Option<&Value>) -> Result<HeaderMap, ToolError> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));

        let Some(Value::Object(map)) = input else {
            return Ok(headers);
        };

        for (key, value) in map {
            let value_str = value
                .as_str()
                .ok_or_else(|| ToolError::InvalidInput(format!("header {key} must be a string")))?;
            let name = HeaderName::from_bytes(key.as_bytes())
                .map_err(|e| ToolError::InvalidInput(format!("invalid header name {key}: {e}")))?;
            let val = HeaderValue::from_str(value_str).map_err(|e| {
                ToolError::InvalidInput(format!("invalid header value for {key}: {e}"))
            })?;
            headers.insert(name, val);
        }

        Ok(headers)
    }

    async fn http_fetch(
        &self,
        url: &str,
        method: Method,
        headers: HeaderMap,
        body: Option<Value>,
    ) -> Result<(u16, String, String), ToolError> {
        Self::validate_url(url)?;

        let mut request = self.http.request(method, url).headers(headers);
        if let Some(body) = body {
            request = request.json(&body);
        }

        let response = request
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let bytes = response
            .bytes()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(ToolError::ExecutionFailed(format!(
                "response exceeds {} byte limit",
                MAX_RESPONSE_BYTES
            )));
        }

        let body_text = String::from_utf8_lossy(&bytes).into_owned();
        Ok((status, content_type, body_text))
    }
}

pub(crate) fn risk() -> ToolRisk {
    ToolRisk {
        exposes: Exposure::WEB,
        ..Default::default()
    }
}

pub(crate) fn request_url(tool_name: &str, input: &Value) -> Option<RequestUrl> {
    let is_empty = |key: &str| match input.get(key) {
        None | Some(Value::Null) => true,
        Some(Value::Object(map)) => map.is_empty(),
        Some(_) => false,
    };
    let plain_get = match tool_name {
        "navigate" => true,
        "fetch" => {
            input
                .get("method")
                .and_then(Value::as_str)
                .is_none_or(|method| method.eq_ignore_ascii_case("GET"))
                && is_empty("headers")
                && is_empty("body")
        }
        _ => false,
    };
    let url = input.get("url").and_then(Value::as_str)?.to_owned();
    Some(RequestUrl { url, plain_get })
}

#[async_trait]
impl Connector for BrowserConnector {
    fn id(&self) -> &str {
        "browser"
    }

    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor {
            id: "browser".into(),
            display_name: "Browser".into(),
            description: None,
            version: "1.0.0".into(),
            icon: bundled_icon_data_uri("browser"),
            auth: AuthMechanism::None,
        }
    }

    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "fetch".into(),
                read_only: Some(false),
                title: Some("Fetch URL".into()),
                description: "Fetch a URL and return the response. HTML is converted to plain text by default.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "method": { "type": "string", "enum": ["GET", "POST"], "default": "GET" },
                        "headers": { "type": "object", "additionalProperties": { "type": "string" } },
                        "body": { "type": "object", "description": "JSON body for POST requests" },
                        "format": { "type": "string", "enum": ["text", "html", "json"], "default": "text" }
                    },
                    "required": ["url"]
                }),
                ..Default::default()
            },
            ToolDefinition {
                name: "navigate".into(),
                // Arbitrary GET URLs can trigger side effects or transmit sensitive data.
                read_only: Some(false),
                title: Some("Navigate Page".into()),
                description: "Fetch a web page and return its title, plain-text content, and outbound links.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "max_links": { "type": "integer", "default": 30 }
                    },
                    "required": ["url"]
                }),
                ..Default::default()
            },
        ]
    }

    fn readiness(&self) -> Readiness {
        Readiness::Ready
    }

    fn risk(&self, _tool_name: &str) -> ToolRisk {
        risk()
    }

    fn request_url(&self, tool_name: &str, input: &Value) -> Option<RequestUrl> {
        request_url(tool_name, input)
    }

    async fn invoke(&self, tool_name: &str, input: Value) -> Result<Value, ToolError> {
        match tool_name {
            "fetch" => self.tool_fetch(input).await,
            "navigate" => self.tool_navigate(input).await,
            _ => Err(ToolError::UnknownTool(tool_name.into())),
        }
    }
}

impl BrowserConnector {
    async fn tool_fetch(&self, input: Value) -> Result<Value, ToolError> {
        let url = input
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidInput("url is required".into()))?;

        let method = input.get("method").and_then(Value::as_str).unwrap_or("GET");

        let method = match method.to_uppercase().as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            other => {
                return Err(ToolError::InvalidInput(format!(
                    "unsupported method: {other}"
                )))
            }
        };

        let format = input
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("text");

        let headers = Self::parse_headers(input.get("headers"))?;
        let body = input.get("body").cloned();

        let (status, content_type, body_text) = self.http_fetch(url, method, headers, body).await?;

        let content = match format {
            "html" => json!(body_text),
            "json" => serde_json::from_str(&body_text).unwrap_or(json!({ "raw": body_text })),
            _ => {
                let text = if looks_like_html(&content_type, &body_text) {
                    html_to_text(&body_text)
                } else {
                    body_text
                };
                json!(truncate_chars(text, MAX_TEXT_CHARS))
            }
        };

        Ok(json!({
            "url": url,
            "status": status,
            "content_type": content_type,
            "format": format,
            "content": content,
        }))
    }

    async fn tool_navigate(&self, input: Value) -> Result<Value, ToolError> {
        let url = input
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::InvalidInput("url is required".into()))?;

        let max_links = input.get("max_links").and_then(Value::as_u64).unwrap_or(30) as usize;

        let headers = Self::parse_headers(None)?;
        let (status, content_type, body_text) =
            self.http_fetch(url, Method::GET, headers, None).await?;

        let title = extract_title(&body_text);
        let text = truncate_chars(html_to_text(&body_text), MAX_TEXT_CHARS);
        let links = extract_links(&body_text, url, max_links);

        Ok(json!({
            "url": url,
            "status": status,
            "content_type": content_type,
            "title": title,
            "content": text,
            "links": links,
        }))
    }
}

fn extract_title(html: &str) -> String {
    let document = Html::parse_document(html);
    static TITLE: LazyLock<Selector> = LazyLock::new(|| Selector::parse("title").expect("title"));
    document
        .select(&TITLE)
        .next()
        .map(|el| collapse_whitespace(&el.text().collect::<String>()))
        .unwrap_or_default()
}

fn extract_links(html: &str, base_url: &str, max_links: usize) -> Vec<Value> {
    let base = url::Url::parse(base_url).ok();
    let document = Html::parse_document(html);
    static ANCHORS: LazyLock<Selector> =
        LazyLock::new(|| Selector::parse("a[href]").expect("a[href]"));

    let mut links = Vec::new();
    for el in document.select(&ANCHORS) {
        if links.len() >= max_links {
            break;
        }
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        let href = href.trim();
        if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
            continue;
        }

        let resolved = base
            .as_ref()
            .and_then(|b| b.join(href).ok())
            .map(|u| u.to_string())
            .unwrap_or_else(|| href.to_string());

        if (resolved.starts_with("http://") || resolved.starts_with("https://"))
            && !links
                .iter()
                .any(|v: &Value| v.as_str() == Some(resolved.as_str()))
        {
            links.push(json!(resolved));
        }
    }

    links
}

fn looks_like_html(content_type: &str, body: &str) -> bool {
    let ct = content_type.to_lowercase();
    if ct.contains("text/html") || ct.contains("application/xhtml") {
        return true;
    }
    let trimmed = body.trim_start();
    trimmed.starts_with("<!DOCTYPE")
        || trimmed.starts_with("<!doctype")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<HTML")
}

fn truncate_chars(s: String, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s;
    }
    let kept: String = s.chars().take(max_chars).collect();
    format!("{kept}\n\n[truncated: {n} characters in page, showing first {max_chars}]")
}

fn html_to_text(html: &str) -> String {
    let document = Html::parse_document(html);
    static BODY: LazyLock<Selector> = LazyLock::new(|| Selector::parse("body").expect("body"));
    let root = document
        .select(&BODY)
        .next()
        .unwrap_or_else(|| document.root_element());
    collapse_whitespace(&collect_visible_text(root))
}

const SKIP_TEXT_TAGS: &[&str] = &["script", "style", "noscript", "svg", "head"];

fn collect_visible_text(element: ElementRef<'_>) -> String {
    if SKIP_TEXT_TAGS.contains(&element.value().name()) {
        return String::new();
    }

    let mut out = String::new();
    for child in element.children() {
        match child.value() {
            Node::Text(text) => out.push_str(&text.text),
            Node::Element(_) => {
                if let Some(el) = ElementRef::wrap(child) {
                    out.push_str(&collect_visible_text(el));
                }
            }
            _ => {}
        }
    }
    out
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(4096));
    let mut last_was_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text_strips_scripts_and_tags() {
        let html = r#"<!DOCTYPE html><html><head><title>T</title></head>
        <body><p>Hello</p><script>alert('x'); huge = "data";</script><style>.x{}</style></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(!text.contains("alert"));
        assert!(!text.contains("huge"));
        assert!(!text.contains(".x{}"));
    }

    #[test]
    fn extract_title_and_links_use_scraper() {
        let html = r#"<html><head><title>My &amp; Page</title></head>
        <body><a href="/path">x</a><a href="https://example.com/a">y</a></body></html>"#;
        assert_eq!(extract_title(html), "My & Page");
        let links = extract_links(html, "https://site.test/page", 10);
        assert_eq!(links.len(), 2);
        assert!(links[0].as_str() == Some("https://site.test/path"));
        assert!(links[1].as_str() == Some("https://example.com/a"));
    }

    #[test]
    fn looks_like_html_sniffs_doctype() {
        assert!(looks_like_html(
            "text/plain",
            "<!DOCTYPE html><html></html>"
        ));
        assert!(!looks_like_html("text/plain", "plain notes"));
    }
}
