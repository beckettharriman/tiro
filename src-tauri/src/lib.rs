//! Tauri entry point: window setup and the pywebview-shaped command surface
//! (PORTING_NOTES §3). The commands are thin wrappers — the real api logic
//! lives in `api`, the recording state machine in `flow`.

pub mod api;
pub mod audio;
pub mod clipboard;
pub mod config;
pub mod cues;
pub mod flow;
pub mod hotkeys;
pub mod placement;
pub mod store;
pub mod transcribe;

use serde_json::Value;
use tauri::{State, WebviewWindow};

#[tauri::command]
fn get_state(app: tauri::AppHandle) -> Value {
    api::get_state(&app)
}

#[tauri::command]
fn copy_text(text: String, ctx: State<'_, flow::AppCtx>) {
    // Best-effort like the original bridge method, then the copy tick.
    if let Err(err) = clipboard::copy(&text) {
        eprintln!("clipboard copy failed: {err}");
    }
    cues::play_cue(&flow::lock(&ctx.cfg), "copy");
}

#[tauri::command]
fn set_setting(app: tauri::AppHandle, key: String, value: Value) -> Value {
    api::set_setting(&app, &key, &value)
}

#[tauri::command]
fn list_mics() -> Vec<String> {
    audio::list_mic_names()
}

#[tauri::command]
fn toggle_record(app: tauri::AppHandle) {
    flow::toggle_record(&app);
}

#[tauri::command]
fn cancel_record(app: tauri::AppHandle) {
    flow::cancel_record(&app);
}

#[tauri::command]
fn set_pin(on: bool, app: tauri::AppHandle) {
    placement::set_pin(&app, on);
}

#[tauri::command]
fn close_panel(window: WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}

#[tauri::command]
fn begin_drag(window: WebviewWindow) -> Result<(), String> {
    window.start_dragging().map_err(|e| e.to_string())
}

#[tauri::command]
async fn pick_folder(app: tauri::AppHandle) -> Option<Value> {
    api::pick_folder(&app)
}

#[tauri::command]
fn rebind_shortcut(app: tauri::AppHandle, which: String, combo: Value) -> Value {
    api::rebind_shortcut(&app, &which, &combo)
}

/// Follow the OS light/dark setting while theme == "system", like the
/// original's 20 s watcher tick. The ThemeChanged window event covers this
/// natively on Windows, but on Linux tao emits the OS change with a dummy
/// window id that never reaches on_window_event — so the poll is the
/// portable signal (and matches the original's behavior exactly).
fn theme_watcher(app: tauri::AppHandle) {
    use tauri::Manager;
    std::thread::spawn(move || {
        let mut last: Option<String> = None;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(20));
            let ctx = app.state::<flow::AppCtx>();
            let eff = {
                let cfg = flow::lock(&ctx.cfg);
                api::effective_theme(&app, &cfg)
            };
            if last.as_deref() != Some(eff.as_str()) {
                if last.is_some() {
                    flow::push_panel(&app, "tiroSetTheme", serde_json::json!(eff));
                }
                last = Some(eff);
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(flow::AppCtx::new())
        .manage(placement::Placement::default())
        .on_window_event(|window, event| {
            // Follow the OS light/dark setting while theme == "system" (the
            // original polled the registry every 20 s; Tauri delivers events).
            if let tauri::WindowEvent::ThemeChanged(theme) = event {
                eprintln!("ThemeChanged({theme:?}) on window '{}'", window.label());
                if window.label() != "panel" {
                    return;
                }
                use tauri::Manager;
                let app = window.app_handle();
                let ctx = app.state::<flow::AppCtx>();
                let cfg_theme = flow::lock(&ctx.cfg).get("theme").to_lowercase();
                if matches!(cfg_theme.as_str(), "light" | "dark") {
                    return; // a fixed theme ignores the OS
                }
                let eff = if *theme == tauri::Theme::Light {
                    "light"
                } else {
                    "dark"
                };
                flow::push_panel(app, "tiroSetTheme", serde_json::json!(eff));
            }
        })
        .setup(|app| {
            // On Linux the WebKitGTK widget reports a ~200 px minimum height,
            // so GTK refuses to make the pill window its configured 72 px.
            // Clear the size request on every descendant widget and re-apply
            // the intended size at the GTK level.
            #[cfg(target_os = "linux")]
            {
                use gtk::prelude::*;
                use tauri::Manager;
                fn clear_size_request(widget: &gtk::Widget) {
                    widget.set_size_request(-1, -1);
                    if let Some(container) = widget.dynamic_cast_ref::<gtk::Container>() {
                        for child in container.children() {
                            clear_size_request(&child);
                        }
                    }
                }
                if let Some(pill) = app.webview_windows().get("pill") {
                    if let Ok(gtk_win) = pill.gtk_window() {
                        clear_size_request(gtk_win.upcast_ref::<gtk::Widget>());
                        gtk_win.resize(300, 72);
                    }
                }
            }
            // The panel is configured hidden (summoned by hotkey/tray, which
            // land in phase 2/3); dev builds show it at startup so there is
            // something to work against. The pill is driven by the recording
            // state machine.
            #[cfg(debug_assertions)]
            {
                use tauri::Manager;
                if let Some(w) = app.webview_windows().get("panel") {
                    let _ = w.show();
                }
                placement::reposition_burst(app.handle(), "panel");
            }
            flow::boot_engine(app.handle().clone());
            hotkeys::register_all(app.handle());
            theme_watcher(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            copy_text,
            set_setting,
            list_mics,
            toggle_record,
            cancel_record,
            set_pin,
            close_panel,
            begin_drag,
            pick_folder,
            rebind_shortcut
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
