//! Browser sign-in.
//!
//! WKWebView does not dispatch WebAuthn, so security keys and passkeys cannot be used inside
//! the webview. Sign-in therefore happens in the user's browser, and the resulting session
//! reaches the webview through Proton's session-fork protocol.
//!
//! This module reserves a fork whose child is the web app, sends the user to account with a
//! user code and an encryption key, and on callback points the webview at the fork. The webview
//! redeems it.

use base64::Engine;
use log::{error, info, warn};
use serde::Deserialize;
use tauri::{AppHandle, Manager};

/// Fork payload version requested of account: 12-byte IV, `fork` as AES-GCM context.
const PAYLOAD_VERSION: u8 = 3;

/// Child of the fork: the client that redeems it.
///
/// Must match what the web app reports as its own client ID, since the fork is stored under
/// that value — a mismatch surfaces as `Invalid selector` rather than anything client-related.
const CHILD_CLIENT_ID: &str = PLATFORM_CLIENT_ID;

/// App version sent on the reserve call. Identifies the caller only; matches nothing.
const RESERVE_CLIENT_ID: &str = PLATFORM_CLIENT_ID;

const PLATFORM_CLIENT_ID: &str = if cfg!(target_os = "macos") {
    "macos-lumo"
} else if cfg!(target_os = "windows") {
    "windows-lumo"
} else {
    "linux-lumo"
};

/// Percent-encode everything outside the URL unreserved set.
///
/// The payload rides in a fragment that account parses with `URLSearchParams`, which decodes
/// `+` to a space, so standard-alphabet base64 cannot be passed through raw.
fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// The product half of a `<platform>-<product>` client ID, used as account's `app` value.
///
/// Account's sign-in page requires the child client ID to contain this value, so both are
/// derived from one constant.
fn app_param(client_id: &str) -> Result<&str, String> {
    client_id
        .split_once('-')
        .map(|(_platform, product)| product)
        .filter(|product| !product.is_empty())
        .ok_or_else(|| format!("client ID '{client_id}' is not <platform>-<product>"))
}

/// Derive account's host from the webview's own URL.
///
/// A fork produced against one environment cannot be redeemed by a webview on another, and the
/// failure is silent, so the two are derived from a single source. Unknown hosts are rejected.
///
/// QA environments live at `lumo.<env>.proton.black`, and each one has its own account at
/// `account.<env>.proton.black`, so any depth of subdomain under `proton.black` is accepted.
pub(super) fn account_url(lumo_url: &str) -> Result<String, String> {
    let url = url::Url::parse(lumo_url).map_err(|e| format!("LUMO_URL is not a URL: {e}"))?;
    let host = url.host_str().ok_or("LUMO_URL has no host")?;
    let domain = host
        .strip_prefix("lumo.")
        .ok_or_else(|| format!("expected a 'lumo.' host in LUMO_URL, got '{host}'"))?;

    match domain {
        "proton.me" | "proton.black" | "proton.dev" => Ok(format!("https://account.{domain}")),
        env if env.ends_with(".proton.black") => Ok(format!("https://account.{env}")),
        other => Err(format!("unknown Proton environment '{other}'")),
    }
}

/// Build the browser sign-in URL, which carries the user code and the encryption key.
///
/// `account/desktop/login?app=<product>&pv=3&redirectUrl=…#payload=0:<userCode>:<key>:<child>`
///
/// The key is standard base64, as account decodes it with `Uint8Array.fromBase64` at its
/// default alphabet — unlike `sk` in [`build_handoff_url`], which is base64url.
fn build_login_url(
    account_url: &str,
    user_code: &str,
    key: &[u8],
    child_client_id: &str,
    redirect: &str,
) -> Result<String, String> {
    // Account splits the payload on ':' by position, so an extra ':' would move the key field.
    if user_code.contains(':') || child_client_id.contains(':') {
        return Err("sign-in payload fields must not contain ':'".into());
    }
    let encoded_key = base64::engine::general_purpose::STANDARD.encode(key);
    let payload = format!("0:{user_code}:{encoded_key}:{child_client_id}");

    Ok(format!(
        "{}/desktop/login?app={}&pv={PAYLOAD_VERSION}&redirectUrl={}#payload={}",
        account_url.trim_end_matches('/'),
        app_param(child_client_id)?,
        percent_encode(redirect),
        percent_encode(&payload),
    ))
}

/// Build the callback URL that hands the fork to the web app.
///
/// `lumo/login#selector=…&sk=…&pv=3&p=1&returnUrl=/`
///
/// `sk` is unpadded base64url, which is what the consumer validates against; `returnUrl` sets
/// the post-login landing page explicitly rather than leaving it to a fallback.
fn build_handoff_url(lumo_url: &str, selector: &str, sk: &[u8]) -> String {
    let encoded_sk = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sk);

    format!(
        "{}/login#selector={}&sk={}&pv={PAYLOAD_VERSION}&p=1&returnUrl={}",
        lumo_url.trim_end_matches('/'),
        percent_encode(selector),
        percent_encode(&encoded_sk),
        percent_encode("/"),
    )
}

/// Callback target for the browser once account has deposited the fork.
///
/// Correlation uses a caller-generated `state`, as account does not append the user code its
/// documentation shows. The value is opaque and carries nothing sensitive.
fn redirect_url(state: &str) -> String {
    format!("proton-lumo://login?state={state}")
}

pub(super) fn random_bytes<const N: usize>() -> [u8; N] {
    use rand::RngCore;

    let mut buffer = [0u8; N];
    rand::rng().fill_bytes(&mut buffer);
    buffer
}

/// What the webview needs to redeem the fork, held until the callback or a new sign-in.
#[derive(Clone)]
struct Handoff {
    selector: String,
    key: Vec<u8>,
    state: String,
}

#[derive(Default)]
pub struct PendingHandoff(std::sync::Mutex<Option<Handoff>>);

#[derive(Deserialize)]
struct ReserveResponse {
    #[serde(rename = "Selector")]
    selector: String,
    #[serde(rename = "UserCode")]
    user_code: String,
}

/// The reserve response is server-supplied and ends up in the payload account uses to choose
/// the key that encrypts the user's key password, so it must match account's own format.
fn validate_reserve(slot: &ReserveResponse) -> Result<(), String> {
    // Crockford base32, as account's `userCodeRegex`: 8 chars, uppercase, no I/L/O/U.
    let valid_user_code = slot.user_code.len() == 8
        && slot
            .user_code
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_uppercase() && !b"ILOU".contains(&b)));
    if !valid_user_code {
        return Err("reserve returned an invalid user code".into());
    }
    let valid_selector = !slot.selector.is_empty()
        && slot.selector.len() <= 256
        && slot.selector.bytes().all(|b| b.is_ascii_graphic());
    if !valid_selector {
        return Err("reserve returned an invalid selector".into());
    }
    Ok(())
}

/// Reserve an empty fork slot.
///
/// The route accepts an anonymous caller, so no session is required. It is sent to the
/// webview's own origin so the fork is created in the environment that will redeem it.
async fn reserve(lumo_url: &str) -> Result<ReserveResponse, String> {
    let url = format!(
        "{}/api/auth/v4/sessions/forks",
        lumo_url.trim_end_matches('/')
    );

    let res = reqwest::Client::new()
        .get(&url)
        .header(
            "x-pm-appversion",
            format!("{RESERVE_CLIENT_ID}@{}", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|e| format!("reserve request failed: {e}"))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(format!("reserve rejected ({status}): {body}"));
    }

    res.json()
        .await
        .map_err(|e| format!("could not parse reserve response: {e}"))
}

/// Reserve a fork and send the user to their browser to sign in.
async fn begin(app_handle: AppHandle) -> Result<(), String> {
    let account_url = account_url(crate::LUMO_URL)?;
    let slot = reserve(crate::LUMO_URL).await?;
    validate_reserve(&slot)?;

    let key = random_bytes::<32>();
    let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes::<16>());

    let login_url = build_login_url(
        &account_url,
        &slot.user_code,
        &key,
        CHILD_CLIENT_ID,
        &redirect_url(&state),
    )?;

    app_handle
        .state::<PendingHandoff>()
        .0
        .lock()
        .map_err(|_| "pending handoff lock poisoned")?
        .replace(Handoff {
            selector: slot.selector,
            key: key.to_vec(),
            state,
        });

    // Not the error's text: it can quote the URL, which carries the sign-in key.
    open::that(&login_url).map_err(|e| format!("failed to open browser: {:?}", e.kind()))?;
    info!("browser opened; waiting for the callback");

    Ok(())
}

/// Point the webview at the fork for redemption.
fn finish(app_handle: &AppHandle, handoff: &Handoff) -> Result<(), String> {
    let url = build_handoff_url(crate::LUMO_URL, &handoff.selector, &handoff.key);
    let webview = app_handle
        .get_webview(crate::main_window::LABEL)
        .ok_or("main webview is not available")?;

    webview
        .navigate(
            url.parse()
                .map_err(|e| format!("invalid handoff URL: {e}"))?,
        )
        .map_err(|e| format!("failed to hand off to the web app: {e}"))?;

    info!("handed the fork to the web app");
    Ok(())
}

/// Start browser sign-in, or with `redeem`, complete the one in progress.
///
/// Completion normally arrives via [`handle_deep_link`]. `redeem` covers browsers that refuse a
/// scheme redirect no user action triggered. Without it a new sign-in replaces any pending one,
/// so a handoff left over from an earlier sign-in is never redeemed.
#[tauri::command]
pub fn start_browser_login(app_handle: AppHandle, redeem: bool) {
    if redeem && finish_pending(&app_handle) {
        return;
    }

    begin_browser_login(&app_handle);
}

/// Hand the pending sign-in to the webview without consuming it.
///
/// The request can come before account has deposited the fork, which fails redemption with
/// `Invalid selector`. Keeping the handoff lets the callback or another request complete it.
fn finish_pending(app_handle: &AppHandle) -> bool {
    let handoff = match app_handle.state::<PendingHandoff>().0.lock() {
        Ok(slot) => slot.clone(),
        Err(_) => {
            error!("pending handoff lock poisoned");
            return false;
        }
    };

    let Some(handoff) = handoff else {
        return false;
    };

    if let Err(e) = finish(app_handle, &handoff) {
        error!("{e}");
    }

    true
}

/// Drop the pending sign-in, so neither the callback nor a request can redeem it.
pub fn discard_pending(app_handle: &AppHandle) {
    match app_handle.state::<PendingHandoff>().0.lock() {
        Ok(mut slot) => *slot = None,
        Err(_) => error!("pending handoff lock poisoned"),
    }
}

/// Discards the pending handoff first, so one left over from an earlier sign-in is not redeemed
/// if this one fails to start. Spawns because the navigation handler runs on the UI thread.
pub fn begin_browser_login(app_handle: &AppHandle) {
    discard_pending(app_handle);
    let app_handle = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = begin(app_handle).await {
            error!("{e}");
        }
    });
}

/// Hand a pending sign-in to the webview, requiring `expected_state` to match.
///
/// Returns whether a pending handoff was consumed.
fn take_and_finish(app_handle: &AppHandle, expected_state: &str) -> bool {
    let pending = app_handle.state::<PendingHandoff>();
    let Ok(mut slot) = pending.0.lock() else {
        error!("pending handoff lock poisoned");
        return false;
    };

    let Some(handoff) = slot.take() else {
        return false;
    };

    if expected_state != handoff.state {
        warn!("ignoring callback with an unrecognised state");
        *slot = Some(handoff);
        return false;
    }

    drop(slot);

    if let Err(e) = finish(app_handle, &handoff) {
        error!("{e}");
    }

    true
}

/// Handle a `proton-lumo://` callback from the browser.
///
/// The callback is a notification only. The selector and key come from local state rather than
/// the URL, so an invocation by any other program cannot introduce a fork of its choosing.
pub fn handle_deep_link(app_handle: &AppHandle, urls: &[url::Url]) {
    for url in urls {
        if url.scheme() != "proton-lumo" {
            continue;
        }

        let state = url
            .query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned());

        match state {
            Some(state) => {
                if !take_and_finish(app_handle, &state) {
                    warn!("callback did not match a sign-in in progress");
                }
            }
            None => warn!("callback carried no state"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_url_percent_encodes_the_payload_fragment() {
        // 0xFF 0xFE is "//4=" in standard base64, which URLSearchParams would otherwise mangle.
        let url = build_login_url(
            "https://account.proton.me",
            "USERCODE",
            &[0xFF, 0xFE],
            "web-lumo",
            "proton-lumo://login?state=abc",
        )
        .unwrap();
        let payload = url.split("#payload=").nth(1).unwrap();

        assert!(payload.starts_with("0%3AUSERCODE%3A"));
        assert!(payload.contains("%2F%2F4%3D"));
        assert!(payload.ends_with("%3Aweb-lumo"));
    }

    fn slot(user_code: &str, selector: &str) -> ReserveResponse {
        ReserveResponse {
            selector: selector.into(),
            user_code: user_code.into(),
        }
    }

    #[test]
    fn a_reserve_response_must_match_accounts_user_code_format() {
        assert!(validate_reserve(&slot("ABCD2345", "sel-ector_1")).is_ok());
        // A ':' would shift a server-chosen key into the field account encrypts the key
        // password with.
        assert!(validate_reserve(&slot("ABCDEFGH:QUFB:macos-lumo", "sel")).is_err());
        for user_code in [
            "",
            "ABCD234",
            "ABCD23456",
            "abcd2345",
            "ABCDEFGI",
            "ABCDEFGU",
        ] {
            assert!(
                validate_reserve(&slot(user_code, "sel")).is_err(),
                "{user_code}"
            );
        }
        for selector in ["", "has space", "line\nbreak", &"a".repeat(257)] {
            assert!(
                validate_reserve(&slot("ABCD2345", selector)).is_err(),
                "{selector:?}"
            );
        }
    }

    #[test]
    fn login_url_refuses_fields_that_would_shift_the_payload() {
        let build = |user_code: &str, child: &str| {
            build_login_url(
                "https://account.proton.me",
                user_code,
                &[0u8; 32],
                child,
                "proton-lumo://login?state=abc",
            )
        };
        assert!(build("ABCDEFGH:QUFB:macos-lumo", "macos-lumo").is_err());
        assert!(build("ABCD2345", "macos-lumo:x").is_err());
        assert!(build("ABCD2345", "macos-lumo").is_ok());
    }

    #[test]
    fn redirect_url_goes_in_the_query_not_the_fragment() {
        let url = build_login_url(
            "https://account.proton.dev",
            "CODE",
            &[0u8; 32],
            "web-lumo",
            "proton-lumo://login?state=abc",
        )
        .unwrap();

        let (query, fragment) = url.split_once('#').unwrap();
        assert!(query.contains("&redirectUrl=proton-lumo%3A%2F%2Flogin%3Fstate%3Dabc"));
        assert!(!fragment.contains("redirectUrl"));
    }

    #[test]
    fn handoff_url_uses_base64url_for_the_key() {
        // 0xFF 0xFE is "//4=" in standard base64 but "__4" in unpadded base64url.
        let url = build_handoff_url("https://lumo.proton.me", "sel-ector", &[0xFF, 0xFE]);

        assert_eq!(
            url,
            "https://lumo.proton.me/login#selector=sel-ector&sk=__4&pv=3&p=1&returnUrl=%2F"
        );
    }

    #[test]
    fn app_param_is_the_product_half_of_the_client_id() {
        assert_eq!(app_param(CHILD_CLIENT_ID).unwrap(), "lumo");
        assert_eq!(app_param(RESERVE_CLIENT_ID).unwrap(), "lumo");
        assert_eq!(app_param(PLATFORM_CLIENT_ID).unwrap(), "lumo");
        assert!(app_param("lumo").is_err());
    }

    #[test]
    fn account_host_follows_the_webview_host() {
        assert_eq!(
            account_url("https://lumo.proton.me").unwrap(),
            "https://account.proton.me"
        );
        assert_eq!(
            account_url("https://lumo.proton.dev").unwrap(),
            "https://account.proton.dev"
        );
        assert!(account_url(crate::LUMO_URL).is_ok());
        assert!(account_url("https://lumo.example.com").is_err());
    }

    #[test]
    fn account_host_follows_a_qa_environment_subdomain() {
        assert_eq!(
            account_url("https://lumo.brandt.proton.black").unwrap(),
            "https://account.brandt.proton.black"
        );
        assert!(account_url("https://lumo.proton.black.example.com").is_err());
    }
}
