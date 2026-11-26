//! The quick-ask window: a small always-on-top search bar opened by the global shortcut.

use anyhow::anyhow;
use log::warn;
use tauri::{
    AppHandle, Manager, Monitor, PhysicalPosition, Runtime, State, Webview, WebviewWindow, Window,
};

use super::PendingQuickAsk;
use crate::{main_window, updater};

const LABEL: &str = "search";

/// Hand a question from the quick-ask window to the web app.
///
/// The question is held in [`PendingQuickAsk`] and the main window is sent to
/// `#quickask=<nonce>`; the web app redeems the nonce with `take_quick_ask`. The question is
/// never put in the URL: the web app auto-sends it, and a URL can come from any link followed
/// inside the webview.
#[tauri::command]
pub fn inject_search(
    query: String,
    window: Window,
    app_handle: AppHandle,
    pending: State<'_, PendingQuickAsk>,
) -> Result<(), String> {
    if window.label() != LABEL {
        warn!("inject_search refused from window {}", window.label());
        return Err("Quick ask is only available from the quick-ask window".to_string());
    }

    let query = query.trim();
    if query.is_empty() {
        return Err("Nothing to ask".to_string());
    }

    let main_webview = app_handle
        .get_webview(main_window::LABEL)
        .ok_or_else(|| "Lumo window is not open".to_string())?;

    let mut target = crate::LUMO_URL
        .parse::<url::Url>()
        .map_err(|e| format!("LUMO_URL is not a URL: {e}"))?;
    let current = main_webview.url().map_err(|e| e.to_string())?;

    if current.origin() != target.origin() {
        main_window::show(&app_handle)?;
        return Err("Sign in to Lumo first".to_string());
    }

    let nonce = pending.store(query.to_string());
    let fragment = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(super::FRAGMENT_KEY, &nonce)
        .finish();
    target.set_fragment(Some(&fragment));

    if let Err(e) = main_webview.navigate(target) {
        pending.clear();
        return Err(e.to_string());
    }

    main_window::show(&app_handle)?;
    Ok(())
}

#[tauri::command]
pub fn hide_search(window: Window) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn show<R: Runtime>(app_handle: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    if updater::is_required(app_handle) {
        return Err(tauri::Error::Anyhow(anyhow!("Update Lumo to continue")));
    }
    let main_window_was_back = app_handle
        .get_window(main_window::LABEL)
        .is_some_and(|window| {
            !window.is_visible().unwrap_or(true) && !window.is_minimized().unwrap_or(false)
        });

    let search_window = match app_handle.get_webview_window(LABEL) {
        Some(window) => {
            window.show()?;
            window.unminimize()?;
            // The page empties its field before hiding, so there is nothing to clear here.
            window.set_focus()?;
            // On Windows, focusing the native window does not focus WebView2: the page gets no
            // `focus` event, keeps its pill stowed, and the window sits invisible holding focus.
            // Focus the webview itself and tell the page it is shown.
            if let Err(e) = AsRef::<Webview<R>>::as_ref(&window).set_focus() {
                warn!("could not focus the quick-ask webview: {e}");
            }
            if let Err(e) = window.eval("window.__lumoRevealSearch?.()") {
                warn!("could not reveal the quick-ask field: {e}");
            }

            window
        }
        None => tauri::WebviewWindowBuilder::new(
            app_handle,
            LABEL,
            tauri::WebviewUrl::App("search.html".into()),
        )
        .title("Search")
        .decorations(false)
        .skip_taskbar(true)
        .always_on_top(true)
        // Transparent only so the pill's rounded corners show; the pill itself is opaque.
        .transparent(true)
        // The native shadow traces the window's alpha mask and draws a grey outline and a heavy
        // band under the pill. The pill draws its own soft shadow in CSS instead.
        .shadow(false)
        .inner_size(600.0, 102.0)
        .build()?,
    };

    let monitor = active_monitor(&search_window)?;
    position_on(&search_window, &monitor)?;

    if main_window_was_back {
        if let Some(main_window) = app_handle.get_window(main_window::LABEL) {
            main_window.hide()?;
        }
    }

    Ok(search_window)
}

/// The monitor under the cursor, else the window's own, else one showing another Lumo window,
/// else the primary.
fn active_monitor<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<Monitor> {
    if let Ok(cursor_position) = window.cursor_position() {
        for monitor in window.available_monitors()? {
            let position = monitor.position();
            let size = monitor.size();
            let min_x = position.x as f64;
            let min_y = position.y as f64;
            let max_x = min_x + size.width as f64;
            let max_y = min_y + size.height as f64;

            if (min_x..max_x).contains(&cursor_position.x)
                && (min_y..max_y).contains(&cursor_position.y)
            {
                return Ok(monitor);
            }
        }
    }

    if let Ok(Some(monitor)) = window.current_monitor() {
        return Ok(monitor);
    }

    let app_handle = window.app_handle();
    for (label, other_window) in app_handle.webview_windows() {
        if label != window.label() && other_window.is_visible().unwrap_or(false) {
            if let Ok(Some(monitor)) = other_window.current_monitor() {
                return Ok(monitor);
            }
        }
    }

    match window.primary_monitor() {
        Ok(Some(monitor)) => Ok(monitor),
        Ok(None) => Err(anyhow!("No monitors found").into()),
        Err(e) => Err(e),
    }
}

/// Centered horizontally near the bottom of `monitor`, clear of the macOS Dock.
fn position_on<R: Runtime>(window: &WebviewWindow<R>, monitor: &Monitor) -> tauri::Result<()> {
    let monitor_size = monitor.size();
    let monitor_position = monitor.position();
    let window_size = window.outer_size()?;

    let x = monitor_position.x + ((monitor_size.width as i32 - window_size.width as i32) / 2);

    #[cfg(target_os = "macos")]
    let bottom_margin = 150;
    #[cfg(not(target_os = "macos"))]
    let bottom_margin = 20;
    let y = monitor_position.y + (monitor_size.height as i32 - window_size.height as i32)
        - bottom_margin;

    window.set_position(PhysicalPosition::new(x, y))?;
    window.set_always_on_top(true)?;

    Ok(())
}
