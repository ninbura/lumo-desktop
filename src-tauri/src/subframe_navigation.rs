//! Let iframes navigate without asking the app's navigation handler.
//!
//! wry calls the handler for every frame but passes only the URL, so `webview::classify` sends
//! third-party iframes (Chargebee's card form, 3-D Secure challenges) to the browser. This
//! replaces wry's `webView:decidePolicyForNavigationAction:decisionHandler:` so that only main
//! frame navigations and new-window requests reach it.

use std::sync::OnceLock;

use crate::macos_overrides::replace_method;
use log::warn;
use objc::runtime::{Class, Imp, Object, Sel, BOOL, NO};
use objc::{msg_send, sel, sel_impl};

/// Clang block ABI header, enough to call a block taking a `WKNavigationActionPolicy`.
#[repr(C)]
struct PolicyBlock {
    isa: *const std::ffi::c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut PolicyBlock, isize),
}

const WK_NAVIGATION_ACTION_POLICY_CANCEL: isize = 0;
const WK_NAVIGATION_ACTION_POLICY_ALLOW: isize = 1;

type DecidePolicy = extern "C" fn(&Object, Sel, *mut Object, *mut Object, *mut PolicyBlock);

static WRY_DECIDE_POLICY: OnceLock<DecidePolicy> = OnceLock::new();

extern "C" fn decide_policy(
    this: &Object,
    sel: Sel,
    webview: *mut Object,
    action: *mut Object,
    handler: *mut PolicyBlock,
) {
    if is_subframe_navigation(action) {
        unsafe { ((*handler).invoke)(handler, WK_NAVIGATION_ACTION_POLICY_ALLOW) };
    } else if let Some(wry) = WRY_DECIDE_POLICY.get() {
        wry(this, sel, webview, action, handler);
    } else {
        unsafe { ((*handler).invoke)(handler, WK_NAVIGATION_ACTION_POLICY_CANCEL) };
    }
}

/// Whether a navigation is allowed without asking wry: only one inside an iframe. A new-window
/// request has no target frame, and downloads stay with wry, which routes them to the app's
/// download handler.
fn skips_handler(target_is_main_frame: Option<bool>, download: bool) -> bool {
    target_is_main_frame == Some(false) && !download
}

fn is_subframe_navigation(action: *mut Object) -> bool {
    unsafe {
        let frame: *mut Object = msg_send![action, targetFrame];
        let target_is_main_frame = if frame.is_null() {
            None
        } else {
            let is_main: BOOL = msg_send![frame, isMainFrame];
            Some(is_main != NO)
        };
        let can_download: BOOL = msg_send![action, respondsToSelector: sel!(shouldPerformDownload)];
        let download = can_download != NO && {
            let download: BOOL = msg_send![action, shouldPerformDownload];
            download != NO
        };
        skips_handler(target_is_main_frame, download)
    }
}

/// Takes the class from the window's live delegate, since objc2 names it with wry's module path
/// and version.
pub fn install(window: &tauri::WebviewWindow) {
    if let Err(e) = window.with_webview(|webview| unsafe {
        let delegate: *mut Object = msg_send![webview.inner() as *mut Object, navigationDelegate];
        if delegate.is_null() {
            warn!("webview has no navigation delegate, iframes will go through the navigation handler");
            return;
        }
        replace_decide_policy((*delegate).class());
    }) {
        warn!("could not reach the webview, iframes will go through the navigation handler: {e}");
    }
}

fn replace_decide_policy(class: &Class) {
    unsafe {
        let imp: Imp = std::mem::transmute(decide_policy as DecidePolicy);
        let selector = sel!(webView:decidePolicyForNavigationAction:decisionHandler:);
        let Some(wry) = replace_method(class, selector, imp) else {
            warn!("{} has no navigation policy method, iframes will go through the navigation handler", class.name());
            return;
        };
        let _ = WRY_DECIDE_POLICY.set(std::mem::transmute::<Imp, DecidePolicy>(wry));
    }
}

#[cfg(test)]
mod tests {
    use super::skips_handler;

    #[test]
    fn only_iframe_navigations_skip_the_navigation_handler() {
        assert!(skips_handler(Some(false), false));

        assert!(!skips_handler(Some(true), false), "the main frame");
        assert!(!skips_handler(None, false), "a new window");
        assert!(!skips_handler(Some(false), true), "a download in an iframe");
    }
}
