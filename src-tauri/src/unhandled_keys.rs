//! Drop keys the webview declines instead of letting tao turn them into text.
//!
//! The `unstable` feature attaches webviews directly to tao's `TaoView`. A key WebKit does not
//! use, such as an arrow at the edge of a text field, then reaches `TaoView`'s `keyDown:`, and
//! the arrow's private-use character ends up in the field. Without `unstable`, wry's parent view
//! offers such keys to the main menu and drops them; this gives `TaoView` the same `keyDown:`.
//! https://github.com/tauri-apps/tauri/issues/10194

use crate::macos_overrides::replace_method;
use log::warn;
use objc::runtime::{Class, Imp, Object, Sel, BOOL};
use objc::{class, msg_send, sel, sel_impl};

extern "C" fn key_down(_this: &Object, _sel: Sel, event: *mut Object) {
    unsafe {
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let menu: *mut Object = msg_send![app, mainMenu];
        if !menu.is_null() {
            let _: BOOL = msg_send![menu, performKeyEquivalent: event];
        }
    }
}

/// Must run after the first window exists, since tao registers `TaoView` lazily.
pub fn install() {
    let Some(class) = Class::get("TaoView") else {
        warn!("TaoView not registered, unhandled keys will reach the text input system");
        return;
    };

    unsafe {
        let imp: Imp = std::mem::transmute(key_down as extern "C" fn(&Object, Sel, *mut Object));
        if replace_method(class, sel!(keyDown:), imp).is_none() {
            warn!("TaoView has no keyDown:, unhandled keys will reach the text input system");
        }
    }
}
