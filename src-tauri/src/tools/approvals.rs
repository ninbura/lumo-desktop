use super::exposure::ApprovalReason;
use super::registry::{ApprovalDecision, ApprovalGate};
use crate::reporting::approval_failure;
use async_trait::async_trait;
use log::{info, warn};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
const APPROVAL_TIMEOUT_ENV: &str = "LUMO_APPROVAL_TIMEOUT_MS";

static PENDING: OnceLock<Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>> =
    OnceLock::new();
static REQUEST_SEQ: AtomicU64 = AtomicU64::new(0);

fn pending() -> &'static Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

fn parse_timeout_ms(raw: &str) -> Option<Duration> {
    raw.trim()
        .parse::<u64>()
        .ok()
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
}

fn approval_timeout() -> Duration {
    static TIMEOUT: OnceLock<Duration> = OnceLock::new();
    *TIMEOUT.get_or_init(|| {
        if !crate::e2e_testing_enabled() {
            return DEFAULT_APPROVAL_TIMEOUT;
        }
        let Ok(raw) = std::env::var(APPROVAL_TIMEOUT_ENV) else {
            return DEFAULT_APPROVAL_TIMEOUT;
        };
        match parse_timeout_ms(&raw) {
            Some(timeout) => {
                info!("approval timeout set to {timeout:?}");
                timeout
            }
            None => {
                warn!(
                    "ignoring invalid {APPROVAL_TIMEOUT_ENV}={raw:?}, using {DEFAULT_APPROVAL_TIMEOUT:?}"
                );
                DEFAULT_APPROVAL_TIMEOUT
            }
        }
    })
}

pub fn register() -> (String, oneshot::Receiver<ApprovalDecision>) {
    let id = format!(
        "approval-{}",
        REQUEST_SEQ.fetch_add(1, Ordering::SeqCst) + 1
    );
    let (tx, rx) = oneshot::channel();
    pending()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id.clone(), tx);
    (id, rx)
}

pub fn respond(request_id: &str, decision: ApprovalDecision) {
    if let Some(tx) = pending()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(request_id)
    {
        let _ = tx.send(decision);
    }
}

pub fn cancel(request_id: &str) {
    pending()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(request_id);
}

/// Called when the app stops acting as an account. An approval granted while one account was
/// active must not resolve a tool call running under another. Dropping a waiter's sender is how a
/// timeout already denies, so no explicit denial is sent.
pub fn cancel_all() {
    cancel_all_in(pending());
}

fn cancel_all_in(pending: &Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>) {
    let mut pending = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if pending.is_empty() {
        return;
    }
    info!("denying {} pending approval(s)", pending.len());
    pending.clear();
}

pub struct InteractiveApprovalGate {
    app_handle: AppHandle,
}

impl InteractiveApprovalGate {
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }
}

#[async_trait]
impl ApprovalGate for InteractiveApprovalGate {
    async fn request(
        &self,
        connector_id: &str,
        tool_name: &str,
        input: &Value,
        reason: Option<&ApprovalReason>,
        conversation: Option<&str>,
    ) -> ApprovalDecision {
        let (request_id, rx) = register();
        if self
            .app_handle
            .emit(
                "tool:approval_request",
                serde_json::json!({
                    "request_id": request_id,
                    "connector_id": connector_id,
                    "tool_name": tool_name,
                    "input": super::hidden_text::reveal(input),
                    "reason": reason,
                    "conversation_id": conversation,
                }),
            )
            .is_err()
        {
            approval_failure("Approval request emission failed");
        }

        match tokio::time::timeout(approval_timeout(), rx).await {
            Ok(Ok(decision)) => decision,
            _ => {
                cancel(&request_id);
                if self
                    .app_handle
                    .emit(
                        "tool:approval_resolved",
                        serde_json::json!({
                            "request_id": request_id,
                            "approved": false,
                            "conversation_id": conversation,
                        }),
                    )
                    .is_err()
                {
                    approval_failure("Approval resolution emission failed");
                }
                ApprovalDecision::Deny
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respond_wakes_the_registered_waiter() {
        let (request_id, rx) = register();
        respond(&request_id, ApprovalDecision::Always);
        assert_eq!(rx.blocking_recv(), Ok(ApprovalDecision::Always));
    }

    /// Uses its own map rather than the process-global one. Cargo runs tests on parallel threads,
    /// so draining the shared map would deny another test's waiter.
    #[test]
    fn cancelling_all_denies_every_waiter() {
        let pending = Mutex::new(HashMap::new());
        let (first_tx, first_rx) = oneshot::channel();
        let (second_tx, second_rx) = oneshot::channel();
        {
            let mut map = pending.lock().unwrap();
            map.insert("approval-1".to_string(), first_tx);
            map.insert("approval-2".to_string(), second_tx);
        }

        cancel_all_in(&pending);

        assert!(first_rx.blocking_recv().is_err());
        assert!(second_rx.blocking_recv().is_err());
        assert!(pending.lock().unwrap().is_empty());
    }

    #[test]
    fn cancel_drops_the_registered_waiter() {
        let (request_id, rx) = register();
        cancel(&request_id);
        assert!(rx.blocking_recv().is_err());
        respond(&request_id, ApprovalDecision::Once);
    }
}
