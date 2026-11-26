//! Native "Download → PDF" for documents.
//!
//! The web app prints documents through a hidden iframe and the browser's "Save as PDF", but
//! `print()` does nothing in the desktop webview. Instead the web app hands the document's HTML
//! to `export_pdf`, which asks where to save it and renders it with the OS print engine: real,
//! selectable text and page breaks driven by the document's print CSS, like a browser's PDF.

use serde::Serialize;
use tauri::{Manager, State, Webview};

use crate::downloads::{self, Downloads};

/// Generous on purpose: a long document with images can take a while to lay out and paginate.
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
const RENDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Upper bound on the HTML accepted over IPC. Assumption: artifact documents, images inlined as
/// data URLs included, stay well under this; it only stops a runaway payload.
const MAX_HTML_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PdfExportStatus {
    Saved,
    Cancelled,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfExportResult {
    status: PdfExportStatus,
}

impl PdfExportResult {
    fn of(status: PdfExportStatus) -> Self {
        Self { status }
    }
}

#[tauri::command]
pub async fn export_pdf(
    webview: Webview,
    downloads: State<'_, Downloads>,
    html: String,
    title: Option<String>,
    file_name: String,
) -> Result<PdfExportResult, String> {
    if webview.label() != crate::main_window::LABEL {
        return Err("PDF export requires the main webview".into());
    }
    if html.len() > MAX_HTML_BYTES {
        return Err("The document is too large to export".into());
    }
    let file_name = pdf_file_name(&file_name);

    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    {
        let Some(path) = pick_destination(&webview, &file_name).await? else {
            return Ok(PdfExportResult::of(PdfExportStatus::Cancelled));
        };
        let title = title.unwrap_or_else(|| file_name.trim_end_matches(".pdf").to_string());
        match render(&webview, html, title, path.clone()).await {
            Ok(()) => {
                let id = downloads.record(path.clone());
                downloads::show_feedback(
                    &webview,
                    id.as_deref().map(|id| (id, path.as_path())),
                    true,
                );
                Ok(PdfExportResult::of(PdfExportStatus::Saved))
            }
            Err(error) => {
                log::warn!("PDF export failed: {error}");
                // Don't leave a half-written file where the user asked us to save.
                let _ = std::fs::remove_file(&path);
                Ok(PdfExportResult::of(PdfExportStatus::Failed))
            }
        }
    }

    #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
    {
        let _ = (downloads, html, title, file_name);
        Err("PDF export is not supported on this platform yet".into())
    }
}

/// Blocks every network and file load, whatever the document's markup; `data:` and
/// `about:` stay allowed. Complements the CSP in case the meta tag lands somewhere ignored.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const BLOCK_NETWORK_RULES: &str = r#"[
    {"trigger": {"url-filter": "^https?:"}, "action": {"type": "block"}},
    {"trigger": {"url-filter": "^wss?:"}, "action": {"type": "block"}},
    {"trigger": {"url-filter": "^ftp:"}, "action": {"type": "block"}},
    {"trigger": {"url-filter": "^file:"}, "action": {"type": "block"}}
]"#;

/// The hosted artifact view loads nothing remote, so neither may the export of the same document.
const EXPORT_CSP: &str =
    "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:";

/// `html` with [`EXPORT_CSP`] directly after its doctype.
///
/// The parser places a meta tag at that position in the head whatever markup follows. A tag
/// before the doctype switches to quirks mode.
fn with_csp(html: &str) -> String {
    let meta = format!(r#"<meta http-equiv="Content-Security-Policy" content="{EXPORT_CSP}">"#);
    let at = doctype_end(html).unwrap_or(0);
    format!("{}{meta}{}", &html[..at], &html[at..])
}

fn doctype_end(html: &str) -> Option<usize> {
    let trimmed = html.trim_start();
    let offset = html.len() - trimmed.len();
    if !trimmed.to_ascii_lowercase().starts_with("<!doctype") {
        return None;
    }
    trimmed.find('>').map(|end| offset + end + 1)
}

/// A bare file name ending in `.pdf`: whatever the web app sends, never a path.
fn pdf_file_name(requested: &str) -> String {
    let name: String = requested
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim().trim_start_matches('.').trim();
    let name = if name.is_empty() { "Document" } else { name };
    if name.to_ascii_lowercase().ends_with(".pdf") {
        name.to_string()
    } else {
        format!("{name}.pdf")
    }
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
async fn pick_destination(
    webview: &Webview,
    file_name: &str,
) -> Result<Option<std::path::PathBuf>, String> {
    use tauri_plugin_dialog::DialogExt;

    let window = webview.window();
    let dialog = webview
        .app_handle()
        .dialog()
        .file()
        .set_parent(&window)
        .set_file_name(file_name)
        .add_filter("PDF", &["pdf"]);
    let picked = tauri::async_runtime::spawn_blocking(move || dialog.blocking_save_file())
        .await
        .map_err(|_| "Could not open the save dialog".to_string())?;
    picked
        .map(|path| {
            path.into_path()
                .map_err(|_| "Unsupported save location".to_string())
        })
        .transpose()
}

#[cfg(target_os = "macos")]
async fn render(
    webview: &Webview,
    html: String,
    title: String,
    path: std::path::PathBuf,
) -> Result<(), String> {
    let ns_window = webview.window().ns_window().map_err(|e| e.to_string())? as usize;
    let (done, result) = tokio::sync::oneshot::channel();
    webview
        .run_on_main_thread(move || macos::print_to_pdf(html, title, path, ns_window, done))
        .map_err(|e| e.to_string())?;
    match tokio::time::timeout(RENDER_TIMEOUT, result).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => Err("PDF renderer stopped without a result".into()),
        Err(_) => Err("PDF rendering timed out".into()),
    }
}

#[cfg(windows)]
async fn render(
    webview: &Webview,
    html: String,
    _title: String,
    path: std::path::PathBuf,
) -> Result<(), String> {
    let page = webview2::write_page(&html)?;
    let url = url::Url::from_file_path(&page)
        .map_err(|_| "Could not prepare the PDF renderer".to_string())?
        .to_string();
    let (done, result) = tokio::sync::oneshot::channel();
    let rendered = webview
        .with_webview(move |platform| {
            webview2::print_to_pdf(
                platform.controller(),
                platform.environment(),
                url,
                path,
                done,
            )
        })
        .map_err(|e| e.to_string());
    let outcome = match rendered {
        Ok(()) => match tokio::time::timeout(RENDER_TIMEOUT, result).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err("PDF renderer stopped without a result".into()),
            Err(_) => Err("PDF rendering timed out".into()),
        },
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_file(&page);
    outcome
}

#[cfg(windows)]
mod webview2 {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::{
        take_pwstr, CreateCoreWebView2ControllerCompletedHandler, NavigationCompletedEventHandler,
        NavigationStartingEventHandler, PrintToPdfCompletedHandler,
        WebResourceRequestedEventHandler,
    };
    use windows_core::{Interface, BOOL, HSTRING, PWSTR};

    type Done = tokio::sync::oneshot::Sender<Result<(), String>>;

    /// Page margins in inches, matching the documents' `@page { margin: 18mm 16mm }`.
    const MARGIN_VERTICAL: f64 = 18.0 / 25.4;
    const MARGIN_HORIZONTAL: f64 = 16.0 / 25.4;

    /// `file:` is blocked separately because the document itself is a file.
    const BLOCKED_SCHEMES: [&str; 5] = ["http:", "https:", "ws:", "wss:", "ftp:"];

    struct Job {
        controller: Option<ICoreWebView2Controller>,
        path: PathBuf,
        navigated: bool,
        started: bool,
        done: Option<Done>,
    }

    type SharedJob = Rc<RefCell<Job>>;

    /// A file is used because `NavigateToString` limits HTML to 2 MB. The BOM fixes the encoding.
    pub(super) fn write_page(html: &str) -> Result<PathBuf, String> {
        let page =
            std::env::temp_dir().join(format!("lumo-pdf-export-{}.html", uuid::Uuid::new_v4()));
        std::fs::write(&page, format!("\u{FEFF}{}", super::with_csp(html)))
            .map_err(|e| format!("Could not prepare the PDF renderer: {e}"))?;
        Ok(page)
    }

    fn finish(job: &SharedJob, outcome: Result<(), String>) {
        let (controller, done) = {
            let mut job = job.borrow_mut();
            (job.controller.take(), job.done.take())
        };
        if let Some(controller) = controller {
            let _ = unsafe { controller.Close() };
        }
        if let Some(done) = done {
            let _ = done.send(outcome);
        }
    }

    /// Must run on the main thread.
    pub(super) fn print_to_pdf(
        main: ICoreWebView2Controller,
        environment: ICoreWebView2Environment,
        url: String,
        path: PathBuf,
        done: Done,
    ) {
        let job = Rc::new(RefCell::new(Job {
            controller: None,
            path,
            navigated: false,
            started: false,
            done: Some(done),
        }));
        if let Err(error) = unsafe { create(&main, environment, url, &job) } {
            finish(&job, Err(error.message()));
        }
    }

    unsafe fn create(
        main: &ICoreWebView2Controller,
        environment: ICoreWebView2Environment,
        url: String,
        job: &SharedJob,
    ) -> windows_core::Result<()> {
        let mut parent = Default::default();
        main.ParentWindow(&mut parent)?;

        let created = {
            let main = main.clone();
            let environment = environment.clone();
            let job = job.clone();
            CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
                move |result, controller| {
                    let loaded = match (result, controller) {
                        (Ok(()), Some(controller)) => unsafe {
                            load(&main, &environment, controller, &url, &job)
                                .map_err(|e| e.message())
                        },
                        (Err(error), _) => Err(error.message()),
                        (Ok(()), None) => Err("Could not create the PDF renderer".into()),
                    };
                    if let Err(error) = loaded {
                        finish(&job, Err(error));
                    }
                    Ok(())
                },
            ))
        };
        environment.CreateCoreWebView2Controller(parent, &created)
    }

    unsafe fn load(
        main: &ICoreWebView2Controller,
        environment: &ICoreWebView2Environment,
        controller: ICoreWebView2Controller,
        url: &str,
        job: &SharedJob,
    ) -> windows_core::Result<()> {
        job.borrow_mut().controller = Some(controller.clone());
        controller.SetIsVisible(false)?;
        let mut bounds = Default::default();
        main.Bounds(&mut bounds)?;
        controller.SetBounds(bounds)?;

        let webview = controller.CoreWebView2()?;
        let settings = webview.Settings()?;
        settings.SetIsScriptEnabled(false)?;
        settings.SetIsWebMessageEnabled(false)?;
        settings.SetAreHostObjectsAllowed(false)?;
        settings.SetAreDefaultScriptDialogsEnabled(false)?;

        let mut token = 0i64;
        webview.AddWebResourceRequestedFilter(
            &HSTRING::from("*"),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        )?;
        let blocker = {
            let environment = environment.clone();
            WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                unsafe {
                    let mut uri = PWSTR::null();
                    args.Request()?.Uri(&mut uri)?;
                    let uri = take_pwstr(uri).to_ascii_lowercase();
                    let mut context = COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL;
                    args.ResourceContext(&mut context)?;
                    // Frame navigations are cancelled, so the only document is the page itself.
                    let subresource = context != COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT;
                    if BLOCKED_SCHEMES.iter().any(|scheme| uri.starts_with(scheme))
                        || (subresource && uri.starts_with("file:"))
                    {
                        let response = environment.CreateWebResourceResponse(
                            None,
                            403,
                            &HSTRING::from("Blocked"),
                            &HSTRING::new(),
                        )?;
                        args.SetResponse(&response)?;
                    }
                }
                Ok(())
            }))
        };
        webview.add_WebResourceRequested(&blocker, &mut token)?;

        let gate = {
            let job = job.clone();
            NavigationStartingEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut job = job.borrow_mut();
                if job.navigated {
                    unsafe { args.SetCancel(true)? };
                } else {
                    job.navigated = true;
                }
                Ok(())
            }))
        };
        webview.add_NavigationStarting(&gate, &mut token)?;
        let no_frames = NavigationStartingEventHandler::create(Box::new(|_, args| {
            if let Some(args) = args {
                unsafe { args.SetCancel(true)? };
            }
            Ok(())
        }));
        webview.add_FrameNavigationStarting(&no_frames, &mut token)?;

        let loaded = {
            let environment = environment.clone();
            let webview = webview.clone();
            let job = job.clone();
            NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                {
                    let mut job = job.borrow_mut();
                    // Cancelled navigations also raise `NavigationCompleted`.
                    if job.started || job.done.is_none() {
                        return Ok(());
                    }
                    job.started = true;
                }
                let mut success = BOOL::default();
                if let Some(args) = args {
                    unsafe { args.IsSuccess(&mut success)? };
                }
                let printed = if success.as_bool() {
                    unsafe { print(&environment, &webview, &job) }.map_err(|e| e.message())
                } else {
                    Err("The document could not be loaded".into())
                };
                if let Err(error) = printed {
                    finish(&job, Err(error));
                }
                Ok(())
            }))
        };
        webview.add_NavigationCompleted(&loaded, &mut token)?;

        webview.Navigate(&HSTRING::from(url))
    }

    unsafe fn print(
        environment: &ICoreWebView2Environment,
        webview: &ICoreWebView2,
        job: &SharedJob,
    ) -> windows_core::Result<()> {
        let webview = webview.cast::<ICoreWebView2_7>()?;
        let settings = environment
            .cast::<ICoreWebView2Environment6>()?
            .CreatePrintSettings()?;
        settings.SetMarginTop(MARGIN_VERTICAL)?;
        settings.SetMarginBottom(MARGIN_VERTICAL)?;
        settings.SetMarginLeft(MARGIN_HORIZONTAL)?;
        settings.SetMarginRight(MARGIN_HORIZONTAL)?;

        let path = HSTRING::from(job.borrow().path.as_os_str());
        let completed = {
            let job = job.clone();
            PrintToPdfCompletedHandler::create(Box::new(move |result, printed| {
                let outcome = match result {
                    Ok(()) if printed => Ok(()),
                    Ok(()) => Err("The print operation did not complete".into()),
                    Err(error) => Err(error.message()),
                };
                finish(&job, outcome);
                Ok(())
            }))
        };
        webview.PrintToPdf(&path, &settings, &completed)
    }
}

#[cfg(target_os = "linux")]
async fn render(
    webview: &Webview,
    html: String,
    _title: String,
    path: std::path::PathBuf,
) -> Result<(), String> {
    let filters = webview
        .app_handle()
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("content-filters");
    std::fs::create_dir_all(&filters).map_err(|e| e.to_string())?;
    let (done, result) = tokio::sync::oneshot::channel();
    webview
        .run_on_main_thread(move || webkitgtk::print_to_pdf(html, path, filters, done))
        .map_err(|e| e.to_string())?;
    match tokio::time::timeout(RENDER_TIMEOUT, result).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => Err("PDF renderer stopped without a result".into()),
        Err(_) => Err("PDF rendering timed out".into()),
    }
}

#[cfg(target_os = "linux")]
mod webkitgtk {
    use std::cell::RefCell;
    use std::ffi::CString;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use gtk::prelude::*;
    use webkit2gtk::glib::translate::{FromGlibPtrFull, ToGlibPtr};
    use webkit2gtk::{
        ffi, gio, glib, LoadEvent, PolicyDecisionExt, PolicyDecisionType, PrintOperation,
        PrintOperationExt, SettingsExt, UserContentManager, WebView, WebViewExt,
    };

    type Done = tokio::sync::oneshot::Sender<Result<(), String>>;
    type Compiled = Box<dyn FnOnce(Result<UserContentManager, String>)>;

    /// Page margins matching the documents' `@page { margin: 18mm 16mm }`.
    const MARGIN_VERTICAL_MM: f64 = 18.0;
    const MARGIN_HORIZONTAL_MM: f64 = 16.0;

    struct Job {
        view: Option<WebView>,
        operation: Option<PrintOperation>,
        navigated: bool,
        started: bool,
        error: Option<String>,
        done: Option<Done>,
    }

    type SharedJob = Rc<RefCell<Job>>;

    fn finish(job: &SharedJob, outcome: Result<(), String>) {
        let (view, done) = {
            let mut job = job.borrow_mut();
            job.operation = None;
            (job.view.take(), job.done.take())
        };
        if let Some(done) = done {
            let _ = done.send(outcome);
        }
        if let Some(view) = view {
            // Deferred because `finish` can run inside a signal of the view being destroyed.
            glib::idle_add_local_once(move || unsafe { view.destroy() });
        }
    }

    fn start(job: &SharedJob) -> bool {
        let mut job = job.borrow_mut();
        !std::mem::replace(&mut job.started, true)
    }

    /// Must run on the main thread.
    pub(super) fn print_to_pdf(html: String, path: PathBuf, filters: PathBuf, done: Done) {
        let job = Rc::new(RefCell::new(Job {
            view: None,
            operation: None,
            navigated: false,
            started: false,
            error: None,
            done: Some(done),
        }));
        let html = super::with_csp(&html);
        compile_filter(
            &filters,
            Box::new(move |manager| match manager {
                Ok(manager) => load(&manager, &html, path, &job),
                Err(error) => finish(&job, Err(error)),
            }),
        );
    }

    /// The Rust bindings do not wrap `WebKitUserContentFilterStore`, so it is called directly.
    fn compile_filter(storage: &Path, then: Compiled) {
        unsafe extern "C" fn saved(
            store: *mut glib::gobject_ffi::GObject,
            result: *mut gio::ffi::GAsyncResult,
            data: glib::ffi::gpointer,
        ) {
            let then = unsafe { Box::from_raw(data as *mut Compiled) };
            let mut error = std::ptr::null_mut();
            let filter = unsafe {
                ffi::webkit_user_content_filter_store_save_finish(
                    store as *mut _,
                    result,
                    &mut error,
                )
            };
            if filter.is_null() {
                let message = if error.is_null() {
                    "Could not prepare the PDF renderer".to_string()
                } else {
                    unsafe { glib::Error::from_glib_full(error) }.to_string()
                };
                then(Err(message));
                return;
            }
            let manager = UserContentManager::new();
            unsafe {
                ffi::webkit_user_content_manager_add_filter(manager.to_glib_none().0, filter);
                ffi::webkit_user_content_filter_unref(filter);
            }
            then(Ok(manager));
        }

        let (Ok(storage), Ok(identifier)) = (
            CString::new(storage.to_string_lossy().into_owned()),
            CString::new("LumoPdfExportBlockNetwork"),
        ) else {
            then(Err("Could not prepare the PDF renderer".into()));
            return;
        };
        let rules = glib::Bytes::from_static(super::BLOCK_NETWORK_RULES.as_bytes());
        unsafe {
            let store = ffi::webkit_user_content_filter_store_new(storage.as_ptr());
            ffi::webkit_user_content_filter_store_save(
                store,
                identifier.as_ptr(),
                ToGlibPtr::<*const glib::ffi::GBytes>::to_glib_none(&rules).0 as *mut _,
                std::ptr::null_mut(),
                Some(saved),
                Box::into_raw(Box::new(then)) as glib::ffi::gpointer,
            );
            glib::gobject_ffi::g_object_unref(store as *mut _);
        }
    }

    fn load(manager: &UserContentManager, html: &str, path: PathBuf, job: &SharedJob) {
        let settings = webkit2gtk::Settings::new();
        settings.set_enable_javascript(false);
        let view = WebView::builder()
            .user_content_manager(manager)
            .settings(&settings)
            .build();

        let gate = job.clone();
        view.connect_decide_policy(move |_, decision, kind| match kind {
            PolicyDecisionType::NavigationAction => {
                let mut job = gate.borrow_mut();
                if job.navigated {
                    decision.ignore();
                } else {
                    job.navigated = true;
                    decision.use_();
                }
                true
            }
            PolicyDecisionType::NewWindowAction => {
                decision.ignore();
                true
            }
            _ => false,
        });

        let loaded = job.clone();
        view.connect_load_changed(move |view, event| {
            if event == LoadEvent::Finished && start(&loaded) {
                print(view, &path, &loaded);
            }
        });
        let failed = job.clone();
        view.connect_load_failed(move |_, _, _, error| {
            if start(&failed) {
                finish(&failed, Err(error.to_string()));
            }
            false
        });

        // Never placed in a window: drawing it makes WebKit request a GL context, which aborts.
        job.borrow_mut().view = Some(view.clone());
        view.load_html(html, None);
    }

    fn print(view: &WebView, path: &Path, job: &SharedJob) {
        let Ok(uri) = url::Url::from_file_path(path) else {
            finish(job, Err("Unsupported save location".into()));
            return;
        };
        let settings = gtk::PrintSettings::new();
        // GTK names its file printer with the translated string.
        settings.set_printer(&glib::dgettext(Some("gtk30"), "Print to File"));
        settings.set(&gtk::PRINT_SETTINGS_OUTPUT_FILE_FORMAT, Some("pdf"));
        settings.set(&gtk::PRINT_SETTINGS_OUTPUT_URI, Some(uri.as_str()));
        let page = gtk::PageSetup::new();
        page.set_top_margin(MARGIN_VERTICAL_MM, gtk::Unit::Mm);
        page.set_bottom_margin(MARGIN_VERTICAL_MM, gtk::Unit::Mm);
        page.set_left_margin(MARGIN_HORIZONTAL_MM, gtk::Unit::Mm);
        page.set_right_margin(MARGIN_HORIZONTAL_MM, gtk::Unit::Mm);

        let operation = PrintOperation::new(view);
        operation.set_print_settings(&settings);
        operation.set_page_setup(&page);
        let failed = job.clone();
        operation.connect_failed(move |_, error| {
            failed.borrow_mut().error = Some(error.to_string());
        });
        let finished = job.clone();
        operation.connect_finished(move |_| {
            let error = finished.borrow_mut().error.take();
            finish(&finished, error.map_or(Ok(()), Err));
        });
        job.borrow_mut().operation = Some(operation.clone());
        operation.print();
    }
}

#[cfg(target_os = "macos")]
mod macos {
    #![allow(unexpected_cfgs)]

    use std::ffi::c_void;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use cocoa::base::{id, nil, BOOL, NO, YES};
    use cocoa::foundation::{NSAutoreleasePool, NSPoint, NSRect, NSSize, NSString};
    use objc::declare::ClassDecl;
    use objc::runtime::{Class, Object, Sel};
    use objc::{class, msg_send, sel, sel_impl};

    type Done = tokio::sync::oneshot::Sender<Result<(), String>>;

    // Linked rather than spelled out: the constants' names are not their values
    // (`NSPrintJobSavingURL` is "NSJobSavingURL"), and a wrong key makes AppKit ask for a
    // destination with a second Save panel.
    #[link(name = "AppKit", kind = "framework")]
    extern "C" {
        static NSPrintJobSavingURL: id;
        static NSPrintSaveJob: id;
    }

    /// Page margins in points, matching the documents' own `@page { margin: 18mm 16mm }`.
    /// Assumption to verify: if WebKit also applies `@page` margins when printing, these add to
    /// them and the margins come out wider; zero would instead put text against the paper edge
    /// whenever WebKit ignores `@page`, which is the worse failure.
    const MARGIN_VERTICAL: f64 = 18.0 / 25.4 * 72.0;
    const MARGIN_HORIZONTAL: f64 = 16.0 / 25.4 * 72.0;

    struct Job {
        path: PathBuf,
        title: String,
        window: usize,
        webview: id,
        navigated: bool,
        started: bool,
        done: Option<Done>,
    }

    const NAVIGATION_CANCEL: isize = 0;
    const NAVIGATION_ALLOW: isize = 1;

    fn ns_string(value: &str) -> id {
        unsafe { NSString::alloc(nil).init_str(value).autorelease() }
    }

    unsafe fn job<'a>(delegate: &Object) -> Option<&'a mut Job> {
        let ptr = *delegate.get_ivar::<usize>("job") as *mut Job;
        ptr.as_mut()
    }

    /// Reports the outcome once and tears down the hidden webview and its delegate.
    unsafe fn finish(delegate: &mut Object, outcome: Result<(), String>) {
        let ptr = *delegate.get_ivar::<usize>("job") as *mut Job;
        if ptr.is_null() {
            return;
        }
        delegate.set_ivar::<usize>("job", 0);
        let mut job = Box::from_raw(ptr);
        if let Some(done) = job.done.take() {
            let _ = done.send(outcome);
        }
        let _: () = msg_send![job.webview, setNavigationDelegate: nil];
        let _: () = msg_send![job.webview, autorelease];
        let _: () = msg_send![delegate as *mut Object, autorelease];
    }

    /// Allows only the initial load of the HTML string; refresh, links and frames are cancelled.
    extern "C" fn decide_policy(
        this: &mut Object,
        _: Sel,
        _webview: id,
        _action: id,
        decision: id,
    ) {
        unsafe {
            let decision = &*(decision as *const block::Block<(isize,), ()>);
            let allow = match job(this) {
                Some(job) if !job.navigated => {
                    job.navigated = true;
                    true
                }
                _ => false,
            };
            decision.call((if allow {
                NAVIGATION_ALLOW
            } else {
                NAVIGATION_CANCEL
            },));
        }
    }

    extern "C" fn did_finish_navigation(this: &mut Object, _: Sel, _webview: id, _navigation: id) {
        unsafe {
            let Some(job) = job(this) else { return };
            // A `<meta http-equiv="refresh">` could navigate again; print the first load only.
            if job.started {
                return;
            }
            job.started = true;
            if let Err(error) = start_print(this, job) {
                finish(this, Err(error));
            }
        }
    }

    extern "C" fn did_fail_navigation(
        this: &mut Object,
        _: Sel,
        _webview: id,
        _navigation: id,
        error: id,
    ) {
        unsafe {
            let description: id = msg_send![error, localizedDescription];
            let message = if description.is_null() {
                "The document could not be loaded".to_string()
            } else {
                let bytes = NSString::UTF8String(description);
                std::ffi::CStr::from_ptr(bytes)
                    .to_string_lossy()
                    .into_owned()
            };
            finish(this, Err(message));
        }
    }

    extern "C" fn print_did_run(
        this: &mut Object,
        _: Sel,
        _operation: id,
        success: BOOL,
        _context: *mut c_void,
    ) {
        unsafe {
            let outcome = if success == YES {
                Ok(())
            } else {
                Err("The print operation did not complete".to_string())
            };
            finish(this, outcome);
        }
    }

    fn delegate_class() -> &'static Class {
        static CLASS: OnceLock<&'static Class> = OnceLock::new();
        CLASS.get_or_init(|| {
            let mut class = ClassDecl::new("LumoPdfExportDelegate", class!(NSObject))
                .expect("PDF export delegate class must be registered once");
            class.add_ivar::<usize>("job");
            unsafe {
                class.add_method(
                    sel!(webView:decidePolicyForNavigationAction:decisionHandler:),
                    decide_policy as extern "C" fn(&mut Object, Sel, id, id, id),
                );
                class.add_method(
                    sel!(webView:didFinishNavigation:),
                    did_finish_navigation as extern "C" fn(&mut Object, Sel, id, id),
                );
                class.add_method(
                    sel!(webView:didFailNavigation:withError:),
                    did_fail_navigation as extern "C" fn(&mut Object, Sel, id, id, id),
                );
                class.add_method(
                    sel!(webView:didFailProvisionalNavigation:withError:),
                    did_fail_navigation as extern "C" fn(&mut Object, Sel, id, id, id),
                );
                class.add_method(
                    sel!(printOperationDidRun:success:contextInfo:),
                    print_did_run as extern "C" fn(&mut Object, Sel, id, BOOL, *mut c_void),
                );
            }
            class.register()
        })
    }

    unsafe fn start_print(delegate: &Object, job: &mut Job) -> Result<(), String> {
        let shared: id = msg_send![class!(NSPrintInfo), sharedPrintInfo];
        let info: id = msg_send![shared, copy];
        let info: id = msg_send![info, autorelease];

        let url: id =
            msg_send![class!(NSURL), fileURLWithPath: ns_string(&job.path.to_string_lossy())];
        let settings: id = msg_send![info, dictionary];
        let _: () = msg_send![settings, setObject: url forKey: NSPrintJobSavingURL];
        let _: () = msg_send![info, setJobDisposition: NSPrintSaveJob];
        let _: () = msg_send![info, setTopMargin: MARGIN_VERTICAL];
        let _: () = msg_send![info, setBottomMargin: MARGIN_VERTICAL];
        let _: () = msg_send![info, setLeftMargin: MARGIN_HORIZONTAL];
        let _: () = msg_send![info, setRightMargin: MARGIN_HORIZONTAL];
        let _: () = msg_send![info, setHorizontallyCentered: NO];
        let _: () = msg_send![info, setVerticallyCentered: NO];

        let operation: id = msg_send![job.webview, printOperationWithPrintInfo: info];
        if operation.is_null() {
            return Err("WebKit returned no print operation".into());
        }
        let _: () = msg_send![operation, setShowsPrintPanel: NO];
        let _: () = msg_send![operation, setShowsProgressPanel: NO];
        let _: () = msg_send![operation, setJobTitle: ns_string(&job.title)];

        // WKWebView's print view renders blank pages unless it is given a frame.
        let paper: NSSize = msg_send![info, paperSize];
        let view: id = msg_send![operation, view];
        let _: () = msg_send![view, setFrame: NSRect::new(NSPoint::new(0.0, 0.0), paper)];

        // Plain `runOperation` is reported to print WKWebView content blank; the window-modal
        // variant lays it out properly. With both panels hidden nothing is shown on the window.
        let _: () = msg_send![operation,
            runOperationModalForWindow: job.window as id
            delegate: delegate as *const Object as id
            didRunSelector: sel!(printOperationDidRun:success:contextInfo:)
            contextInfo: std::ptr::null_mut::<c_void>()];
        Ok(())
    }

    /// Loads `html` into a hidden WKWebView with JavaScript and network access off and no app
    /// bridge, then prints it to `path`. Must run on the main thread; `done` receives the outcome.
    pub(super) fn print_to_pdf(
        html: String,
        title: String,
        path: PathBuf,
        window: usize,
        done: Done,
    ) {
        let html = super::with_csp(&html);
        let pending = std::cell::RefCell::new(Some((html, title, path, done)));
        let compiled = block::ConcreteBlock::new(move |rules: id, error: id| {
            let Some((html, title, path, done)) = pending.borrow_mut().take() else {
                return;
            };
            unsafe {
                let on_main: BOOL = msg_send![class!(NSThread), isMainThread];
                if rules.is_null() || !error.is_null() || on_main != YES {
                    let _ = done.send(Err("Could not prepare the PDF renderer".into()));
                    return;
                }
                render_with_rules(rules, html, title, path, window, done);
            }
        })
        .copy();
        unsafe {
            let store: id = msg_send![class!(WKContentRuleListStore), defaultStore];
            let _: () = msg_send![store,
                compileContentRuleListForIdentifier: ns_string("LumoPdfExportBlockNetwork")
                encodedContentRuleList: ns_string(super::BLOCK_NETWORK_RULES)
                completionHandler: &*compiled];
        }
    }

    unsafe fn render_with_rules(
        rules: id,
        html: String,
        title: String,
        path: PathBuf,
        window: usize,
        done: Done,
    ) {
        unsafe {
            let config: id = msg_send![class!(WKWebViewConfiguration), new];
            let preferences: id = msg_send![config, preferences];
            // The HTML is model-generated: sanitized upstream, but never run its scripts here.
            let _: () = msg_send![preferences, setJavaScriptEnabled: NO];
            let content: id = msg_send![config, userContentController];
            let _: () = msg_send![content, addContentRuleList: rules];

            let shared: id = msg_send![class!(NSPrintInfo), sharedPrintInfo];
            let paper: NSSize = msg_send![shared, paperSize];
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), paper);
            let webview: id = msg_send![class!(WKWebView), alloc];
            let webview: id = msg_send![webview, initWithFrame: frame configuration: config];
            let _: () = msg_send![config, release];
            if webview.is_null() {
                let _ = done.send(Err("Could not create the PDF renderer".into()));
                return;
            }

            let delegate: id = msg_send![delegate_class(), new];
            let job = Box::new(Job {
                path,
                title,
                window,
                webview,
                navigated: false,
                started: false,
                done: Some(done),
            });
            (*delegate).set_ivar::<usize>("job", Box::into_raw(job) as usize);
            // `navigationDelegate` is weak; `delegate` stays retained until `finish`.
            let _: () = msg_send![webview, setNavigationDelegate: delegate];
            let _: id = msg_send![webview, loadHTMLString: ns_string(&html) baseURL: nil];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_gets_pdf_extension() {
        assert_eq!(pdf_file_name("Quarterly report"), "Quarterly report.pdf");
        assert_eq!(pdf_file_name("Report.PDF"), "Report.PDF");
    }

    #[test]
    fn file_name_cannot_be_a_path() {
        assert_eq!(pdf_file_name("../../etc/passwd"), "-..-etc-passwd.pdf");
        assert_eq!(pdf_file_name("a\\b:c"), "a-b-c.pdf");
        assert_eq!(pdf_file_name("bad\nname"), "bad-name.pdf");
    }

    #[test]
    fn empty_file_name_falls_back() {
        assert_eq!(pdf_file_name("   "), "Document.pdf");
        assert_eq!(pdf_file_name("..."), "Document.pdf");
    }

    fn csp_at(html: &str) -> usize {
        with_csp(html)
            .find(r#"<meta http-equiv="Content-Security-Policy""#)
            .unwrap()
    }

    #[test]
    fn the_csp_follows_the_doctype() {
        let html = "<!DOCTYPE html><html><head><title>t</title></head><body></body></html>";
        assert_eq!(csp_at(html), "<!DOCTYPE html>".len());
        assert!(with_csp(html).contains(EXPORT_CSP));
    }

    #[test]
    fn markup_cannot_move_the_csp() {
        let after_doctype = "<!doctype html>".len();
        assert_eq!(
            csp_at("<!doctype html><!-- <head> --><body></body>"),
            after_doctype
        );
        assert_eq!(
            csp_at("<!doctype html><title><head></title>"),
            after_doctype
        );
        assert_eq!(csp_at("<!-- <head> --><html><body></body></html>"), 0);
    }

    #[test]
    fn status_serializes_for_the_bridge() {
        let json = serde_json::to_string(&PdfExportResult::of(PdfExportStatus::Cancelled)).unwrap();
        assert_eq!(json, r#"{"status":"cancelled"}"#);
    }
}
