// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod analytics;
mod apps;
mod config;
mod crypto;
mod state;

use std::process::Command;
use std::sync::Arc;
use std::thread;

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use analytics::{
    AnalyticsPayload, DailySummaryRow, HourlyUsageRow, SessionRow, chrono_now,
    close_current_session, open_db, query_analytics, setup_db, start_background_tracker,
    write_session_close,
};
use apps::{build_app_list, fetch_app_list, icon_path, icon_path_to_b64, spawn_icon_fetch};
use config::{load_config, save_config};
use state::{AppData, AppState};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

macro_rules! no_window {
    ($cmd:expr) => {{
        #[cfg(windows)]
        {
            $cmd.creation_flags(CREATE_NO_WINDOW)
        }
        #[cfg(not(windows))]
        {
            $cmd
        }
    }};
}

// ── App commands ──────────────────────────────────────────────────────────────

#[tauri::command]
fn get_installed_apps(app_handle: AppHandle, state: State<AppState>) -> Vec<AppData> {
    {
        let lock = state.data.lock().unwrap();
        if !lock.apps.is_empty() {
            let apps = lock.apps.clone();
            let missing: Vec<_> = apps
                .iter()
                .filter(|a| a.icon_base64.is_none())
                .map(|a| (a.app_id.clone(), icon_path(&a.app_id)))
                .collect();
            if !missing.is_empty() {
                spawn_icon_fetch(missing, Arc::clone(&state.data), app_handle);
            }
            return apps;
        }
    }

    let raw = fetch_app_list();
    let mut lock = state.data.lock().unwrap();
    let existing = lock.apps.clone();
    let (apps, needs) = build_app_list(raw, &existing);
    lock.apps = apps;
    if !lock.categories.contains(&"Uncategorized".into()) {
        lock.categories.insert(0, "Uncategorized".into());
    }
    save_config(&lock);
    if !needs.is_empty() {
        spawn_icon_fetch(needs, Arc::clone(&state.data), app_handle);
    }
    lock.apps.clone()
}

#[tauri::command]
fn get_categories(state: State<AppState>) -> Vec<String> {
    state.data.lock().unwrap().categories.clone()
}

#[tauri::command]
fn add_category(name: String, state: State<AppState>) {
    let mut lock = state.data.lock().unwrap();
    let t = name.trim().to_string();
    if !t.is_empty() && !lock.categories.contains(&t) {
        lock.categories.push(t);
        save_config(&lock);
    }
}

#[tauri::command]
#[allow(non_snake_case)]
fn assign_app_to_category(
    appId: String,
    category: String,
    state: State<AppState>,
) -> Result<(), String> {
    let mut lock = state.data.lock().unwrap();
    match lock.apps.iter_mut().find(|a| a.app_id == appId) {
        Some(app) => {
            app.category = category;
            save_config(&lock);
            Ok(())
        }
        None => Err(format!("App '{}' not found", appId)),
    }
}

#[tauri::command]
#[allow(non_snake_case)]
fn open_app(appId: String) -> Result<(), String> {
    let ps = format!(
        "Start-Process 'shell:appsFolder\\{}'",
        appId.replace('\'', "''")
    );
    let s = no_window!(Command::new("powershell").args([
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-Command",
        &ps
    ]))
    .status();
    match s {
        Ok(s) if s.success() => Ok(()),
        _ => {
            no_window!(Command::new("explorer").arg(format!("shell:appsFolder\\{}", appId)))
                .spawn()
                .map_err(|e| e.to_string())?;
            Ok(())
        }
    }
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    no_window!(Command::new("explorer").arg(&url))
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ── Analytics commands ────────────────────────────────────────────────────────

#[tauri::command]
#[allow(non_snake_case)]
fn log_app_open(
    appId: String,
    appName: String,
    category: String,
    state: State<AppState>,
) -> Result<(), String> {
    close_current_session(&state)?;
    let conn = open_db().map_err(|e| e.to_string())?;
    let now = chrono_now();
    conn.execute(
        "INSERT INTO app_sessions (app_id,app_name,category,opened_at) VALUES(?1,?2,?3,?4)",
        rusqlite::params![appId, appName, category, now],
    )
    .map_err(|e| e.to_string())?;
    let sid = conn.last_insert_rowid();
    {
        let mut s = state.session.lock().unwrap();
        *s = Some(state::ActiveSession {
            session_id: sid,
            app_id: appId.clone(),
            app_name: appName.clone(),
            started_at: std::time::Instant::now(),
        });
    }
    let arc = Arc::clone(&state.session);
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_secs(30 * 60));
        let sess = { arc.lock().unwrap().clone() };
        if let Some(s) = sess {
            if s.session_id == sid {
                let _ = write_session_close(sid, &s.app_id, &s.app_name);
                *arc.lock().unwrap() = None;
            }
        }
    });
    Ok(())
}

#[tauri::command]
fn log_app_close(state: State<AppState>) -> Result<(), String> {
    close_current_session(&state)
}

#[tauri::command]
fn get_analytics(days: i64) -> Result<AnalyticsPayload, String> {
    query_analytics(days)
}

#[tauri::command]
fn get_daily_totals(days: i64) -> Result<Vec<DailySummaryRow>, String> {
    Ok(query_analytics(days)?.daily_totals)
}

#[tauri::command]
fn get_app_totals(days: i64) -> Result<Vec<DailySummaryRow>, String> {
    Ok(query_analytics(days)?.app_totals)
}

#[tauri::command]
fn get_hourly_usage(days: i64) -> Result<Vec<HourlyUsageRow>, String> {
    Ok(query_analytics(days)?.hourly_usage)
}

#[tauri::command]
fn get_long_sessions(days: i64) -> Result<Vec<SessionRow>, String> {
    Ok(query_analytics(days)?.long_sessions)
}

#[tauri::command]
fn debug_analytics() -> Result<serde_json::Value, String> {
    let conn = open_db().map_err(|e| e.to_string())?;
    let session_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM app_sessions", [], |r| r.get(0))
        .unwrap_or(0);
    let summary_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM daily_summary", [], |r| r.get(0))
        .unwrap_or(0);
    let mut stmt = conn.prepare(
        "SELECT id,app_name,opened_at,closed_at,duration_secs FROM app_sessions ORDER BY id DESC LIMIT 5"
    ).map_err(|e| e.to_string())?;
    let sessions: Vec<_> = stmt
        .query_map([], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_,i64>(0)?, "app_name": r.get::<_,String>(1)?,
                "opened_at": r.get::<_,String>(2)?, "closed_at": r.get::<_,Option<String>>(3)?,
                "duration_secs": r.get::<_,Option<i64>>(4)?,
            }))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(serde_json::json!({
        "db_path": analytics::get_db_path_str(),
        "now": chrono_now(),
        "session_count": session_count,
        "summary_count": summary_count,
        "last_5_sessions": sessions,
    }))
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    setup_db();

    let app_state = AppState::new(load_config());
    let bg_arc = Arc::clone(&app_state.bg_session);
    start_background_tracker(bg_arc);

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            get_installed_apps,
            get_categories,
            add_category,
            assign_app_to_category,
            open_app,
            open_url,
            log_app_open,
            log_app_close,
            get_analytics,
            get_daily_totals,
            get_app_totals,
            get_hourly_usage,
            get_long_sessions,
            debug_analytics,
            ai::process_voice_command,
            ai::save_api_key,
            ai::load_api_key,
        ])
        .setup(|app| {
            // Pre-warm app list in background
            let data_arc = Arc::clone(&app.state::<AppState>().data);
            let app_handle = app.handle().clone();
            thread::spawn(move || {
                let raw = fetch_app_list();
                let mut lock = data_arc.lock().unwrap();
                if lock.apps.is_empty() {
                    let config = load_config();
                    let (apps, needs) = build_app_list(raw, &config.apps);
                    lock.apps = apps;
                    drop(lock);
                    let _ = app_handle.emit("apps-ready", ());
                    if !needs.is_empty() {
                        spawn_icon_fetch(needs, Arc::clone(&data_arc), app_handle);
                    }
                }
            });

            // Tray menu
            let open_item = MenuItemBuilder::new("📦 Open AppDrawer")
                .id("open")
                .build(app)?;
            let analytics_item = MenuItemBuilder::new("📊 Analytics")
                .id("analytics")
                .build(app)?;
            let voice_item = MenuItemBuilder::new("🎙 Voice Command")
                .id("voice")
                .build(app)?;
            let quit_item = MenuItemBuilder::new("✕ Quit").id("quit").build(app)?;
            let menu = MenuBuilder::new(app)
                .item(&open_item)
                .item(&analytics_item)
                .item(&voice_item)
                .separator()
                .item(&quit_item)
                .build()?;

            TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("AppDrawer")
                .icon(app.default_window_icon().unwrap().clone())
                .on_menu_event(|app, event| {
                    let show = |app: &AppHandle| {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    };
                    match event.id().as_ref() {
                        "open" => show(app),
                        "analytics" => {
                            show(app);
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.emit("open-analytics", ());
                            }
                        }
                        "voice" => {
                            show(app);
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.emit("open-voice", ());
                            }
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // Global shortcuts
            let h = app.handle().clone();
            app.global_shortcut()
                .on_shortcut("Alt+F1", move |_, _, event| {
                    if event.state() == ShortcutState::Pressed {
                        if let Some(w) = h.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    }
                })?;

            // Ctrl+Shift+V — open voice command from anywhere
            let h2 = app.handle().clone();
            app.global_shortcut()
                .on_shortcut("Alt+F2", move |_, _, event| {
                    if event.state() == ShortcutState::Pressed {
                        if let Some(w) = h2.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                            let _ = w.emit("open-voice", ());
                        }
                    }
                })?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("Error while running Tauri application");
}
