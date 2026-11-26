//! Methods the app replaces on Objective-C classes that wry and tao own.
//!
//! - wry's UI delegate, `webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:`:
//!   [`crate::microphone`]
//! - wry's navigation delegate, `webView:decidePolicyForNavigationAction:decisionHandler:`:
//!   [`crate::subframe_navigation`]
//! - tao's `TaoView`, `keyDown:`: [`crate::unhandled_keys`]

use objc::runtime::{class_getInstanceMethod, method_setImplementation, Class, Imp, Sel};

/// Replaces `selector` on `class` for every instance, returning the implementation it had, or
/// `None` if the class has no such method.
///
/// # Safety
/// `imp` must have the signature of the method it replaces.
pub unsafe fn replace_method(class: &Class, selector: Sel, imp: Imp) -> Option<Imp> {
    let method = class_getInstanceMethod(class, selector);
    if method.is_null() {
        return None;
    }
    Some(method_setImplementation(method as *mut _, imp))
}
