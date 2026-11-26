//! Microphone access for dictation in the hosted web app, and nothing else.
//!
//! wry grants every media-capture request on macOS, whatever the origin or frame, and the
//! Linux hook used to allow any audio request. Model-generated script runs on other origins
//! (the artifact shell on lumo-api), so access is limited to the web app's own origin.

/// Whether a page at `url` may use the microphone: only the exact `LUMO_URL` origin.
pub fn is_allowed(url: &url::Url) -> bool {
    crate::LUMO_URL
        .parse::<url::Url>()
        .is_ok_and(|lumo| lumo.origin() == url.origin())
}

/// Whether a media-capture request is granted: the microphone alone, asked for by the main frame
/// of the web app.
pub fn grants(microphone_only: bool, main_frame: bool, origin: Option<&url::Url>) -> bool {
    microphone_only && main_frame && origin.is_some_and(is_allowed)
}

#[cfg(target_os = "macos")]
pub use macos::install;

#[cfg(target_os = "macos")]
mod macos {
    #![allow(unexpected_cfgs)]

    use crate::macos_overrides::replace_method;
    use log::warn;
    use objc::runtime::{Imp, Object, Sel};
    use objc::runtime::{BOOL, NO};
    use objc::{msg_send, sel, sel_impl};

    /// Clang block ABI header, enough to call a block taking a `WKPermissionDecision`.
    #[repr(C)]
    struct DecisionBlock {
        isa: *const std::ffi::c_void,
        flags: i32,
        reserved: i32,
        invoke: unsafe extern "C" fn(*mut DecisionBlock, isize),
    }

    const WK_MEDIA_CAPTURE_TYPE_MICROPHONE: isize = 1;
    const WK_PERMISSION_DECISION_GRANT: isize = 1;
    const WK_PERMISSION_DECISION_DENY: isize = 2;

    type RequestPermission = extern "C" fn(
        &Object,
        Sel,
        *mut Object,
        *mut Object,
        *mut Object,
        isize,
        *mut DecisionBlock,
    );

    extern "C" fn request_permission(
        _this: &Object,
        _sel: Sel,
        _webview: *mut Object,
        origin: *mut Object,
        frame: *mut Object,
        capture_type: isize,
        handler: *mut DecisionBlock,
    ) {
        let grant = super::grants(
            capture_type == WK_MEDIA_CAPTURE_TYPE_MICROPHONE,
            is_main_frame(frame),
            origin_url(origin).as_ref(),
        );
        let decision = if grant {
            WK_PERMISSION_DECISION_GRANT
        } else {
            warn!("refused a media capture request (type {capture_type})");
            WK_PERMISSION_DECISION_DENY
        };
        unsafe { ((*handler).invoke)(handler, decision) };
    }

    fn is_main_frame(frame: *mut Object) -> bool {
        if frame.is_null() {
            return false;
        }
        let is_main: BOOL = unsafe { msg_send![frame, isMainFrame] };
        is_main != NO
    }

    fn string(value: *mut Object) -> Option<String> {
        if value.is_null() {
            return None;
        }
        let bytes: *const std::ffi::c_char = unsafe { msg_send![value, UTF8String] };
        (!bytes.is_null()).then(|| {
            unsafe { std::ffi::CStr::from_ptr(bytes) }
                .to_string_lossy()
                .into_owned()
        })
    }

    /// `WKSecurityOrigin` as a URL; port 0 means the scheme's default.
    fn origin_url(origin: *mut Object) -> Option<url::Url> {
        if origin.is_null() {
            return None;
        }
        let (protocol, host, port): (*mut Object, *mut Object, isize) = unsafe {
            (
                msg_send![origin, protocol],
                msg_send![origin, host],
                msg_send![origin, port],
            )
        };
        let (protocol, host) = (string(protocol)?, string(host)?);
        let port = if port > 0 {
            format!(":{port}")
        } else {
            String::new()
        };
        format!("{protocol}://{host}{port}").parse().ok()
    }

    /// Replaces the method on wry's UI delegate class, so every webview the app creates is
    /// covered. Taken from the live delegate, since objc2 names the class with wry's version.
    pub fn install(window: &tauri::WebviewWindow) {
        if let Err(e) = window.with_webview(|webview| unsafe {
            let delegate: *mut Object = msg_send![webview.inner() as *mut Object, UIDelegate];
            if delegate.is_null() {
                warn!("webview has no UI delegate, microphone requests use WebKit's default");
                return;
            }
            let imp: Imp = std::mem::transmute(request_permission as RequestPermission);
            let selector = sel!(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:);
            if replace_method((*delegate).class(), selector, imp).is_none() {
                warn!("UI delegate has no media capture method, WebKit's default applies");
            }
        }) {
            warn!("could not hook microphone permissions: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{grants, is_allowed};

    #[test]
    fn only_the_web_app_origin_may_use_the_microphone() {
        let url = |s: &str| s.parse::<url::Url>().unwrap();
        assert!(is_allowed(&url(crate::LUMO_URL)));
        assert!(is_allowed(&url(&format!("{}/u/0/chat", crate::LUMO_URL))));

        let api = crate::LUMO_URL.replace("//lumo.", "//lumo-api.");
        assert!(!is_allowed(&url(&format!("{api}/lumo/v1/artifact-shell"))));
        assert!(!is_allowed(&url("https://evil.example/")));
        assert!(!is_allowed(&url("tauri://localhost/login.html")));
    }

    #[test]
    fn only_the_web_apps_main_frame_gets_the_microphone_and_nothing_gets_the_camera() {
        let web_app = crate::LUMO_URL.parse::<url::Url>().unwrap();
        let elsewhere = "https://evil.example/".parse::<url::Url>().unwrap();
        assert!(grants(true, true, Some(&web_app)));

        assert!(!grants(false, true, Some(&web_app)), "camera, or both");
        assert!(!grants(true, false, Some(&web_app)), "an iframe");
        assert!(!grants(true, true, Some(&elsewhere)));
        assert!(!grants(true, true, None), "an origin that can't be read");
    }
}
