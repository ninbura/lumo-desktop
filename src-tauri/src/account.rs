//! The Proton account the app is acting as. Connector config and secrets are namespaced by
//! `UserID`, which is stable across logins and never reused by another account.

use log::{error, info, warn};
use tauri::{AppHandle, Manager, State, Webview};
use tokio::sync::watch;

const MAX_LEN: usize = 128;

/// Account the app acts as under `LUMO_E2E_TESTING`. Must match `e2e/support/fixtures.js`.
const E2E_ACCOUNT: &str = "e2e-test-account";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountId(String);

impl AccountId {
    /// Accepts base64url plus `=` padding, which covers Proton user ids while excluding path
    /// separators, `.`, whitespace, control bytes, and anything non-ASCII.
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() || raw.len() > MAX_LEN {
            return Err(format!(
                "account id has an unexpected length: {}",
                raw.len()
            ));
        }
        if let Some(bad) = raw
            .chars()
            .find(|c| !matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '='))
        {
            return Err(format!(
                "account id contains an unexpected character: {bad:?}"
            ));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct ActiveAccount(watch::Sender<Option<AccountId>>);

impl Default for ActiveAccount {
    fn default() -> Self {
        Self(watch::channel(None).0)
    }
}

impl ActiveAccount {
    pub fn get(&self) -> Option<AccountId> {
        self.0.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<AccountId>> {
        self.0.subscribe()
    }

    /// Returns whether the held account changed.
    pub fn set(&self, next: Option<AccountId>) -> bool {
        self.0.send_if_modified(|held| {
            if *held == next {
                return false;
            }
            *held = next;
            true
        })
    }
}

/// Acts as a fixed account under `LUMO_E2E_TESTING`.
///
/// The e2e stub is a bundled page with no Proton session, so nothing reports an account and every
/// per-account path resolves to empty. Seeding keeps the suite on the per-account code paths.
pub fn seed_e2e_account(active: &ActiveAccount) {
    match AccountId::parse(E2E_ACCOUNT) {
        Ok(account) => {
            active.set(Some(account));
            info!("e2e testing: acting as {E2E_ACCOUNT}");
        }
        Err(e) => error!("e2e account is not usable: {e}"),
    }
}

fn require_main_webview(webview: &Webview) -> Result<(), String> {
    if webview.label() == crate::main_window::LABEL {
        return Ok(());
    }
    let label = webview.label().to_string();
    warn!("refused an account report from webview {label:?}");
    Err(format!(
        "only the main webview may report the account, not {label:?}"
    ))
}

/// An unparseable report clears the account rather than leaving the previous one loaded.
async fn apply_report(app_handle: &AppHandle, user_id: &str) -> Result<(), String> {
    match AccountId::parse(user_id) {
        Ok(id) => {
            become_account(app_handle, Some(id)).await;
            Ok(())
        }
        Err(e) => {
            warn!("rejected a reported account: {e}");
            become_account(app_handle, None).await;
            Err(e)
        }
    }
}

/// Spawns rather than awaits. The navigation handler runs on the UI thread and must not block
/// while connector sessions close.
pub fn clear_on_session_loss(app_handle: &AppHandle) {
    let app_handle = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        become_account(&app_handle, None).await;
    });
}

/// The webview restates the current account on every request it makes, so most calls stop at the
/// comparison below rather than rebuilding the registry.
async fn become_account(app_handle: &AppHandle, next: Option<AccountId>) {
    // Under LUMO_E2E_TESTING the harness owns the account. The stub page carries no Proton
    // session, so an honoured report would clear the seeded account at startup and tear down the
    // connectors the suite drives.
    if crate::e2e_testing_enabled() {
        return;
    }

    let Some(active) = app_handle.try_state::<ActiveAccount>() else {
        return;
    };
    if !active.set(next.clone()) {
        return;
    }
    // A late browser callback would otherwise send the signed-in webview to the used fork.
    if next.is_some() {
        crate::browser_login::discard_pending(app_handle);
    }
    if let Some(cache) = app_handle.try_state::<super::skills::SkillCache>() {
        cache.activate(next.clone());
    }
    match &next {
        Some(account) => info!("active account is now {}", account.as_str()),
        None => info!("active account cleared"),
    }
    crate::tools::swap_account(app_handle, next).await;
}

/// Swaps connectors before returning when `user_id` differs from the account held, so a request
/// is never served under a stale account.
///
/// `None` leaves the held account unchanged and is not a sign-out. `settings.html` invokes these
/// commands without stamping an account.
pub async fn record_request_account(
    app_handle: &AppHandle,
    user_id: Option<&str>,
) -> Result<(), String> {
    match user_id {
        Some(raw) => apply_report(app_handle, raw).await,
        None => Ok(()),
    }
}

/// The settings page runs as a child webview on the bundled-asset origin and cannot read the web
/// app's session itself. Only the fact is exposed, not the id.
#[tauri::command]
pub fn is_account_signed_in(active: State<'_, ActiveAccount>) -> bool {
    active.get().is_some()
}

#[tauri::command]
pub async fn set_active_account(
    user_id: String,
    webview: Webview,
    app_handle: AppHandle,
) -> Result<(), String> {
    require_main_webview(&webview)?;
    apply_report(&app_handle, &user_id).await
}

#[tauri::command]
pub async fn clear_active_account(webview: Webview, app_handle: AppHandle) -> Result<(), String> {
    require_main_webview(&webview)?;
    become_account(&app_handle, None).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AccountId, ActiveAccount, MAX_LEN};

    fn account(raw: &str) -> AccountId {
        AccountId::parse(raw).expect("should be a valid account id")
    }

    #[test]
    fn a_proton_user_id_round_trips() {
        assert_eq!(account("Ic1kL9m-_==").as_str(), "Ic1kL9m-_==");
    }

    #[test]
    fn ids_that_are_unsafe_in_a_path_are_rejected() {
        for raw in [
            "",
            "..",
            "1234/5678",
            "1234\\5678",
            "12.34",
            "12 34",
            &"a".repeat(MAX_LEN + 1),
        ] {
            assert!(AccountId::parse(raw).is_err(), "{raw:?} should be rejected");
        }
    }

    #[test]
    fn only_a_real_change_is_reported() {
        let active = ActiveAccount::default();
        assert_eq!(active.get(), None);

        assert!(active.set(Some(account("1234"))));
        assert!(!active.set(Some(account("1234"))));
        assert!(active.set(Some(account("5678"))));
        assert_eq!(active.get(), Some(account("5678")));

        assert!(active.set(None));
        assert!(!active.set(None));
    }

    #[test]
    fn subscribers_are_notified_only_on_real_changes_and_new_subscribers_get_current_account() {
        let active = ActiveAccount::default();
        let mut changes = active.subscribe();
        assert!(!changes.has_changed().unwrap());
        active.set(Some(account("alice")));
        assert!(changes.has_changed().unwrap());
        assert_eq!(*changes.borrow_and_update(), Some(account("alice")));
        active.set(Some(account("alice")));
        assert!(!changes.has_changed().unwrap());
        assert_eq!(*active.subscribe().borrow(), Some(account("alice")));
        active.set(None);
        assert!(changes.has_changed().unwrap());
        assert_eq!(*changes.borrow_and_update(), None);
    }
}
