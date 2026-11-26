use log::{error, info, warn};
use tauri::Manager;
#[cfg(not(feature = "e2e-webdriver"))]
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::settings::{self, load_device_settings};
use crate::tools::{self, registry::ToolRegistry};
use crate::{
    account, browser_login, downloads, main_window, notifications, pdf_export, quick_ask,
    reporting, reset, schedules, skills, theme, tray, updater, webview,
};

pub fn run() {
    tools::child_process::prepare_tool_processes();
    // The guard must outlive the event loop to prevent premature client shutdown.
    let _sentry = reporting::init();
    let builder = tauri::Builder::default();
    // Must be registered before any other plugin: it intercepts a second launch and
    // forwards its arguments to the running instance, which is how Linux and Windows
    // deliver deep links. A test binary must never forward its launch to an ordinary
    // running Lumo, so the e2e-webdriver build leaves it out.
    #[cfg(not(feature = "e2e-webdriver"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
        if let Err(e) = main_window::show(app) {
            warn!("failed to show main window on second launch: {e}");
        }
    }));
    let builder = builder.plugin(
        tauri_plugin_log::Builder::new()
            .max_file_size(5_000_000)
            .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(3))
            .level(if cfg!(debug_assertions) {
                log::LevelFilter::Debug
            } else {
                log::LevelFilter::Info
            })
            .build(),
    );
    #[cfg(feature = "e2e-webdriver")]
    let builder = builder
        .plugin(tauri_plugin_wdio_webdriver::init())
        .plugin(tauri_plugin_wdio::init());
    builder
        .invoke_handler(|invoke| {
            let app = invoke.message.webview().app_handle().clone();
            if updater::is_required(&app) {
                invoke.resolver.reject("Update Lumo to continue");
                return true;
            }
            let commands: fn(tauri::ipc::Invoke<tauri::Wry>) -> bool = tauri::generate_handler![
                quick_ask::window::inject_search,
                quick_ask::window::hide_search,
                quick_ask::take_quick_ask,
                settings::commands::get_app_settings,
                settings::commands::save_app_settings,
                settings::window::close_settings_window,
                settings::window::open_settings_window,
                settings::window::take_pending_settings_tab,
                reset::reset_local_configuration,
                theme::report_web_theme,
                theme::get_web_theme,
                browser_login::start_browser_login,
                crate::feature_flags::get_desktop_feature_flags,
                crate::feature_flags::set_desktop_feature_flags,
                account::is_account_signed_in,
                account::set_active_account,
                account::clear_active_account,
                skills::skill_sync_bundle,
                skills::skill_cache_retain,
                skills::skill_execute_script,
                notifications::desktop_notification_permission,
                notifications::desktop_notification_permission_state,
                notifications::desktop_notification_show,
                notifications::desktop_notification_open_settings,
                downloads::reveal_download,
                pdf_export::export_pdf,
                schedules::bridge::schedule_connect,
                schedules::bridge::schedule_replace,
                schedules::bridge::schedule_run_now,
                schedules::bridge::schedule_pending,
                schedules::bridge::schedule_unsettled,
                schedules::bridge::schedule_report_status,
                tools::commands::get_tool_registry,
                tools::commands::get_builtin_tools,
                tools::commands::is_secure_storage_available,
                tools::commands::set_connector_enabled,
                tools::commands::set_tool_policy,
                tools::commands::respond_tool_approval,
                tools::commands::always_allow_tool,
                tools::commands::invoke_tool,
                tools::commands::register_schedule_execution_context,
                tools::commands::connect_connector,
                tools::commands::cancel_connector_connection,
                tools::commands::disconnect_connector,
                tools::commands::save_mcp_server,
                tools::commands::list_mcp_servers,
                tools::commands::remove_mcp_server,
                tools::commands::pick_directory,
            ];
            commands(invoke)
        })
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            #[cfg(feature = "e2e-webdriver")]
            {
                // Fail closed before reading or mutating any real application state.
                if !crate::e2e_testing_enabled() {
                    return Err("e2e-webdriver builds require LUMO_E2E_TESTING".into());
                }
                let config = std::env::var_os("LUMO_E2E_CONFIG_DIR")
                    .ok_or("e2e-webdriver builds require LUMO_E2E_CONFIG_DIR")?;
                if !std::path::Path::new(&config).is_absolute() {
                    return Err("LUMO_E2E_CONFIG_DIR must be absolute".into());
                }
                app.add_capability(include_str!("../../e2e/automation-capability.json"))?;
            }
            app.manage(updater::UpdateState::default());
            app.manage(downloads::Downloads::default());
            // First line of every launch: the log appends across sessions, and a bug report is
            // only useful with the version and platform it came from.
            info!(
                "Lumo {} starting on {} {} ({})",
                app.package_info().version,
                std::env::consts::OS,
                std::env::consts::ARCH,
                if cfg!(debug_assertions) { "debug" } else { "release" }
            );

            if let Ok(data_dir) = app.path().app_data_dir() {
                tools::child_process::install_tool_helper(&data_dir);
            }

            if let Err(message) = reset::apply_pending_reset(app.handle()) {
                error!("local configuration reset failed: {message}");
                let handle = app.handle().clone();
                app.dialog()
                    .message(format!("Could not reset local configuration: {message}\n\nRetry later closes Lumo and retries the reset on the next launch. Cancel reset reopens Lumo without retrying. Changes already made will remain."))
                    .title("Reset failed")
                    .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                    .buttons(tauri_plugin_dialog::MessageDialogButtons::OkCancelCustom(
                        "Retry later".into(),
                        "Cancel reset".into(),
                    ))
                    .show(move |retry_later| {
                        if retry_later {
                            handle.exit(1);
                            return;
                        }
                        match reset::cancel_pending_reset(&handle) {
                            Ok(()) => handle.restart(),
                            Err(message) => {
                                error!("could not cancel local configuration reset: {message}");
                                let exit_handle = handle.clone();
                                handle.dialog()
                                    .message(message)
                                    .title("Could not cancel reset")
                                    .kind(tauri_plugin_dialog::MessageDialogKind::Error)
                                    .show(move |_| exit_handle.exit(1));
                            }
                        }
                    });
                return Ok(());
            }

            app.manage(browser_login::PendingHandoff::default());
            app.manage(webview::LoginPage::default());
            app.manage(settings::window::PendingSettingsTab::default());
            app.manage(theme::WebTheme::default());
            app.manage(quick_ask::PendingQuickAsk::default());
            app.manage(account::ActiveAccount::default());
            app.manage(crate::feature_flags::DesktopFeatureFlags::default());
            if crate::e2e_testing_enabled() {
                account::seed_e2e_account(&app.state::<account::ActiveAccount>());
            }
            app.manage(skills::SkillCache::new(app.path().app_cache_dir()?.join("skill-runtime"),
                app.state::<account::ActiveAccount>().get()));
            app.manage(schedules::coordinator::ScheduleCoordinator::start(app.handle()));

            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        if !tools::oauth::handle_deep_link(&url) {
                            browser_login::handle_deep_link(&handle, std::slice::from_ref(&url));
                        }
                    }
                });
            }

            let registry = ToolRegistry::new();
            registry.set_approval_gate(std::sync::Arc::new(
                tools::approvals::InteractiveApprovalGate::new(app.handle().clone()),
            ));
            registry.set_host(std::sync::Arc::new(tools::AccountStateFile::new(
                app.handle().clone(),
            )));
            app.manage(registry);

            match settings::get_config_dir(app.handle()) {
                Ok(config_dir) => tools::auth::set_pending_deletes_dir(config_dir),
                Err(e) => error!("connector secrets will not load: {e}"),
            }
            app.manage(tools::auth::SecureStorageStatus {
                available: tauri::async_runtime::block_on(tools::auth::secure_storage_available()),
            });

            // Blocks so the registry is populated before any command can read it.
            {
                let account = app.state::<account::ActiveAccount>().get();
                let handle = app.handle().clone();
                tauri::async_runtime::block_on(tools::swap_account(&handle, account));
            }

            let settings = load_device_settings(app.handle()).unwrap_or_else(|e| {
                warn!("could not load settings, using defaults: {e}");
                settings::DeviceSettings::default()
            });

            // A test binary must never register itself as a login item.
            #[cfg(not(feature = "e2e-webdriver"))]
            {
                app.handle()
                    .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))?;
                let autostart_manager = app.autolaunch();
                if settings.launch_at_startup {
                    let _ = autostart_manager.enable();
                } else {
                    let _ = autostart_manager.disable();
                }
            }

            app.add_capability(main_window::web_app_capability(crate::LUMO_URL)?)?;
            main_window::build(app)?;

            let app_handle = app.handle().clone();
            // The plugin's setup fails when the platform has no global hotkey backend (e.g. Linux
            // without an X display). Run without the quick-ask shortcut rather than not at all.
            let global_shortcuts = app
                .handle()
                .plugin(
                    tauri_plugin_global_shortcut::Builder::new()
                        .with_handler(move |_app, _shortcut, event| {
                            if event.state() == ShortcutState::Pressed {
                                if let Err(e) = quick_ask::window::show(&app_handle) {
                                    error!("could not show search window: {e}");
                                }
                            }
                        })
                        .build(),
                )
                .inspect_err(|e| error!("global shortcuts unavailable: {e}"))
                .is_ok();

            #[cfg(target_os = "macos")]
            {
                crate::unhandled_keys::install();
                app.set_activation_policy(tauri::ActivationPolicy::Regular);
            }

            if global_shortcuts && settings.search_shortcut_enabled {
                let shortcut = settings
                    .search_shortcut
                    .parse::<Shortcut>()
                    .unwrap_or_else(|_| Shortcut::new(Some(Modifiers::CONTROL), Code::KeyL));
                // Another app may already own the shortcut, or the platform may not support global
                // shortcuts (e.g. Wayland). Lumo is still usable without it, so never abort setup.
                if let Err(e) = app.handle().global_shortcut().register(shortcut) {
                    error!("could not register the quick-ask shortcut {shortcut}: {e}");
                }
            }
            tray::create(app.handle())?;
            updater::startup(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if matches!(event, tauri::RunEvent::Reopen { .. }) {
                if let Err(e) = main_window::show(app) {
                    warn!("failed to show main window on reopen: {e}");
                }
            }

            if matches!(event, tauri::RunEvent::Exit) {
                if let Some(cache) = app.try_state::<skills::SkillCache>() {
                    cache.activate(None);
                }
                if let Some(coordinator) =
                    app.try_state::<schedules::coordinator::ScheduleCoordinator>()
                {
                    coordinator.shutdown();
                }
            }
        });
}
