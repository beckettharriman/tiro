//! Stub backend for the UI import phase (PORT_PLAN 0.2): the commands mirror
//! the pywebview api surface (see PORTING_NOTES §3) but serve an in-memory
//! dummy state so the panel renders and settings interactions round-trip.
//! Real config/audio/transcription backends replace these in later tasks.

pub mod audio;
pub mod clipboard;
pub mod config;
pub mod cues;
pub mod store;
pub mod transcribe;

use std::sync::{Mutex, MutexGuard};

use serde_json::{json, Value};
use tauri::{State, WebviewWindow};

struct AppState(Mutex<Value>);

fn default_state() -> Value {
    json!({
        "entries": [],
        "settings": {
            "powerMode": "auto",
            "modelBattery": "base.en",
            "modelPlugged": "small.en",
            "soundCues": true,
            "volume": 67,
            "recordingPill": true,
            "clipboardCleanup": "light",
            "smartVocab": true,
            "micName": "",
            "launchAtLogin": false,
            "savePath": "~/Documents/Tiro",
            "transparency": 45,
            "storageFallback": false,
            "storagePath": ""
        },
        "engine": { "model": "base.en", "device": "CPU", "power": "battery" },
        "mics": [],
        "shortcuts": {
            "dictate": { "ctrl": true, "alt": true, "shift": false, "meta": false,
                         "code": "Space", "keys": ["Ctrl", "Alt", "Space"] },
            "panel":   { "ctrl": true, "alt": true, "shift": false, "meta": false,
                         "code": "KeyV", "keys": ["Ctrl", "Alt", "V"] },
            "cancel":  { "ctrl": true, "alt": true, "shift": false, "meta": false,
                         "code": "KeyX", "keys": ["Ctrl", "Alt", "X"] }
        },
        "theme": "system",
        "effectiveTheme": "dark"
    })
}

fn lock_state(state: &AppState) -> MutexGuard<'_, Value> {
    // A poisoned lock only means another command panicked; the state itself
    // is plain JSON and still usable.
    state.0.lock().unwrap_or_else(|e| e.into_inner())
}

/// Mirror of the original's engine derivation: device follows the power mode
/// (auto = GPU when plugged, CPU on battery), model follows the power source.
fn derive_engine(root: &mut Value) {
    let settings = &root["settings"];
    let power = root["engine"]["power"]
        .as_str()
        .unwrap_or("battery")
        .to_owned();
    let power_mode = settings["powerMode"].as_str().unwrap_or("auto");
    let device = match power_mode {
        "auto" => {
            if power == "plugged" {
                "GPU"
            } else {
                "CPU"
            }
        }
        "gpu" => "GPU",
        _ => "CPU",
    };
    let model = if power == "plugged" {
        settings["modelPlugged"].clone()
    } else {
        settings["modelBattery"].clone()
    };
    root["engine"] = json!({ "model": model, "device": device, "power": power });
}

fn effective_theme(theme: &str) -> &str {
    match theme {
        "light" => "light",
        // "system" resolves via Tauri's theme API in a later task; dark for now.
        _ => "dark",
    }
}

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> Value {
    lock_state(&state).clone()
}

#[tauri::command]
fn copy_text(text: String) {
    // Best-effort like the original bridge method; the copy cue joins in 2.2.
    if let Err(err) = clipboard::copy(&text) {
        eprintln!("clipboard copy failed: {err}");
    }
}

#[tauri::command]
fn set_setting(key: String, value: Value, state: State<'_, AppState>) -> Value {
    let mut root = lock_state(&state);
    if key == "theme" {
        root["theme"] = value;
    } else {
        root["settings"][&key] = value;
    }
    derive_engine(&mut root);
    let theme = root["theme"].as_str().unwrap_or("system").to_owned();
    let effective = effective_theme(&theme).to_owned();
    root["effectiveTheme"] = json!(effective);
    json!({
        "ok": true,
        "engine": root["engine"],
        "theme": theme,
        "effectiveTheme": effective,
        "launchAtLogin": root["settings"]["launchAtLogin"]
    })
}

#[tauri::command]
fn list_mics(state: State<'_, AppState>) -> Value {
    lock_state(&state)["mics"].clone()
}

#[tauri::command]
fn toggle_record() {
    // Recording state machine lands in task 2.1.
}

#[tauri::command]
fn cancel_record() {}

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
fn pick_folder() -> Option<Value> {
    // Real folder picker (tauri dialog plugin) lands in task 2.2.
    None
}

#[tauri::command]
fn rebind_shortcut(which: String, combo: Value, state: State<'_, AppState>) -> Value {
    let keys = combo["keys"].clone();
    let mut root = lock_state(&state);
    if matches!(which.as_str(), "dictate" | "panel" | "cancel") {
        root["shortcuts"][&which] = combo;
        json!({ "ok": true, "keys": keys })
    } else {
        json!({ "ok": false, "keys": keys })
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState(Mutex::new(default_state())))
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
            // Both windows are configured hidden (the panel is summoned by
            // hotkey/tray, the pill only during takes). Until those exist,
            // dev builds show the windows at startup so there is something
            // to work against.
            #[cfg(debug_assertions)]
            {
                use tauri::Manager;
                for label in ["panel", "pill"] {
                    if let Some(w) = app.webview_windows().get(label) {
                        let _ = w.show();
                    }
                }
            }
            #[cfg(all(not(debug_assertions), not(target_os = "linux")))]
            let _ = app;
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
