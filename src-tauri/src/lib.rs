//! Tauri entry point: window setup and the pywebview-shaped command surface
//! (PORTING_NOTES §3). The commands are thin wrappers — the real api logic
//! lives in `api`, the recording state machine in `flow`.

pub mod api;
pub mod audio;
pub mod clipboard;
pub mod config;
pub mod cues;
pub mod flow;
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
fn set_pin(on: bool, window: WebviewWindow) -> Result<(), String> {
    window.set_always_on_top(on).map_err(|e| e.to_string())
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(flow::AppCtx::new())
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
            }
            flow::boot_engine(app.handle().clone());
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
