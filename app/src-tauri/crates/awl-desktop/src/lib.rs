//! `awl-desktop` — Agent Work Ledger macOS desktop binary.
//!
//! Thin shell around `awl-db` + `awl-ipc`: opens the database, builds the
//! Tauri 2 application, installs the macOS menu-bar tray, and wires the
//! window-state persistence on close/move/resize. Every `#[tauri::command]`
//! lives in `awl-ipc`; we just register them in `generate_handler!` here.

use awl_db::{open_db, resolve_data_dir};
use tauri::Manager;

mod window_state;

/// Menu item id for "Show window".
const MENU_ID_SHOW: &str = "show";
/// Menu item id for "Scan now" — runs an immediate `auto_rescan_all`.
const MENU_ID_SCAN_NOW: &str = "scan_now";
/// Menu item id for "Quit" — fully tears the app down.
const MENU_ID_QUIT: &str = "quit";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Stage 1-H-b2: headless `--scan-once` mode for the launchd plist
    // watcher. Detect before tracing init so a non-zero exit code lands
    // in the plist's StandardErrorPath. argv[0] is always the binary
    // itself, so scan argv[1..] for the flag.
    //
    // v0.4.6: route the flag detection through `window_state::is_cli_mode`
    // (whitelist in `window_state::CLI_FLAGS`) so adding a new CLI mode
    // touches exactly one list. Sub-mode selection still uses per-flag
    // checks below because each handler is bespoke.
    let argv: Vec<String> = std::env::args().collect();
    let scan_once_requested = argv.iter().any(|a| a == "--scan-once");
    let brief_once_requested = argv.iter().any(|a| a == "--brief-once");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    if scan_once_requested {
        awl_ipc::run_scan_once_mode();
        return;
    }

    if brief_once_requested {
        awl_ipc::run_brief_once_mode();
        return;
    }

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            let dir = resolve_data_dir().map_err(|e| {
                tracing::error!(error = %e, "failed to resolve data dir");
                e
            })?;
            let db_path = dir.join("ledger.db");
            tracing::info!(db_path = %db_path.display(), "opening ledger database");

            let conn = open_db(&db_path).map_err(|e| {
                tracing::error!(error = %e, "failed to open ledger database");
                e
            })?;

            let db = std::sync::Arc::new(std::sync::Mutex::new(conn));

            // Stage 1-H: dev startup auto-rescan. Runs on a background thread
            // so the window is not blocked while we re-ingest. Failures are
            // isolated per-connector; one bad source cannot abort the rest.
            let bg_db = db.clone();
            if let Err(e) = std::thread::Builder::new()
                .name("startup-auto-rescan".into())
                .spawn(move || awl_ipc::auto_rescan_all(bg_db))
            {
                tracing::error!(error = %e, "failed to spawn auto-rescan thread; continuing without background scan");
            }

            // Stage 1-H-b: in-app background loop. Re-runs every supported
            // connector every BACKGROUND_SCAN_INTERVAL_SECS so newly created
            // work becomes visible without user interaction. Owns its own
            // Arc clone of the DB so it survives window hide / show cycles.
            let loop_db = db.clone();
            if let Err(e) = std::thread::Builder::new()
                .name("background-scanner".into())
                .spawn(move || awl_ipc::background_scan_loop(loop_db))
            {
                tracing::error!(error = %e, "failed to spawn background scanner loop; continuing with one-shot scan only");
            }

            // Stage 1-H-b: macOS tray icon + right-click menu. The default
            // window icon is reused so we don't need to ship a separate
            // tray PNG; `icon_as_template(true)` matches the menu bar
            // appearance on macOS. Clicking the icon (left or right) opens
            // the menu; "Show Window" surfaces the app, "Scan Now" kicks
            // an immediate rescan, "Quit" really exits the process.
            use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
            let menu = Menu::with_items(
                app,
                &[
                    &MenuItem::with_id(app, MENU_ID_SHOW, "Show Window", true, None::<&str>)?,
                    &MenuItem::with_id(app, MENU_ID_SCAN_NOW, "Scan Now", true, None::<&str>)?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, MENU_ID_QUIT, "Quit Agent Work Ledger", true, None::<&str>)?,
                ],
            )?;
            let tray_db = db.clone();
            let _tray = TrayIconBuilder::with_id("agent-work-ledger")
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .ok_or_else(|| awl_core::error::AppError::Connector("tray".into(), "default window icon missing".into()))?,
                )
                .icon_as_template(true)
                .tooltip("Agent Work Ledger")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app_handle, event| {
                    let id = event.id.as_ref();
                    if id == MENU_ID_SHOW {
                        if let Some(w) = app_handle.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
                        }
                    } else if id == MENU_ID_SCAN_NOW {
                        tracing::info!("tray menu: manual scan requested");
                        let db_for_scan = tray_db.clone();
                        if let Err(e) = std::thread::Builder::new()
                            .name("tray-manual-scan".into())
                            .spawn(move || awl_ipc::auto_rescan_all(db_for_scan))
                        {
                            tracing::error!(error = %e, "tray: failed to spawn manual scan");
                        }
                    } else if id == MENU_ID_QUIT {
                        tracing::info!("tray menu: app exit requested");
                        app_handle.exit(0);
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    // Left-click on the tray icon surfaces the window so
                    // the user can reach the timeline without opening the
                    // right-click first. We deliberately ignore right-click
                    // here because the menu is already shown automatically.
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app_handle = tray.app_handle();
                        if let Some(w) = app_handle.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.unminimize();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;

            app.manage(awl_ipc::AppState { db, db_path });

            // Stage 1-H-b: closing the window hides it instead of quitting
            // the process — the app keeps running in the menu bar so the
            // background scanner loop continues. Quit through the tray menu
            // (or Cmd-Q on the focused window) to actually exit.
            //
            // v0.3.6 / Stage 5.6a: window-state persistence. We snapshot
            // on `CloseRequested` (the user closing the window) and on
            // `Resized`/`Moved` (so a manual resize sticks even if the
            // user never closes the window before rebooting). On startup
            // we apply the previously-persisted geometry so the next
            // launch opens at the same size/position.
            //
            // v0.4.6: CLI-mode guard. `run()` already early-returns for
            // `--scan-once` / `--brief-once` before this setup runs, but
            // a future contributor adding a new entry point could silently
            // regress this. The guard here makes the rule explicit at the
            // persistence boundary: window state is GUI-only, period.
            let argv: Vec<String> = std::env::args().collect();
            let skip_window_state = window_state::is_cli_mode(&argv);
            if !skip_window_state {
                if let Some(window) = app.get_webview_window("main") {
                    let startup_dir = dir.clone();
                    // Apply previously-persisted state before the user
                    // sees the window. The startup_dir is the resolved
                    // data dir, owned for the duration of this closure.
                    if let Some(state) = window_state::load(&startup_dir) {
                        window_state::apply_to_window(&window, &state);
                    }
                    let save_dir = dir.clone();
                    let save_window = window.clone();
                    let save_window_for_event = window.clone();
                    let save_dir_for_event = dir.clone();
                    window.on_window_event(move |event| {
                        match event {
                            tauri::WindowEvent::CloseRequested { api, .. } => {
                                // Snapshot before hiding so we always have
                                // a fresh geometry record.
                                if let Ok(s) =
                                    window_state::snapshot_from_window(&save_window)
                                {
                                    window_state::save(&save_dir, &s);
                                }
                                let _ = save_window.hide();
                                api.prevent_close();
                            }
                            tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Moved(_) => {
                                if let Ok(s) = window_state::snapshot_from_window(
                                    &save_window_for_event,
                                ) {
                                    window_state::save(&save_dir_for_event, &s);
                                }
                            }
                            _ => {}
                        }
                    });
                }
            } else {
                tracing::info!(
                    cli_args = ?argv,
                    "window-state: CLI mode detected, skipping load/save"
                );
            }

            Ok(())
        });

    // Stage IPC commands. `awl_ipc::register` takes a builder and
    // returns one with all `#[tauri::command]` functions wired in via
    // a single `generate_handler!` invocation that lives in the
    // `awl-ipc` crate (where the command fns are private).
    awl_ipc::register(builder)
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
