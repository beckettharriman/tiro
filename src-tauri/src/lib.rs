//! Tauri entry point: window setup and the pywebview-shaped command surface
//! (PORTING_NOTES §3). The commands are thin wrappers — the real api logic
//! lives in `api`, the recording state machine in `flow`.

pub mod api;
pub mod audio;
pub mod clipboard;
pub mod config;
pub mod cues;
pub mod flow;
pub mod gpu;
pub mod gpu_worker;
pub mod hotkeys;
pub mod placement;
pub mod power;
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

/// The original's 20 s `power_watcher` tick: keep the engine chip's power
/// label and the system theme in sync with the live machine state. (Theme
/// polling is needed because on Linux tao emits OS ThemeChanged with a
/// dummy window id that never reaches on_window_event; the device swap on
/// power flips joins in task 3.5.)
fn power_watcher(app: tauri::AppHandle) {
    use tauri::Manager;
    std::thread::spawn(move || {
        let mut last_theme: Option<String> = None;
        let mut last_power = power::on_ac_power();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(20));
            let ctx = app.state::<flow::AppCtx>();
            let ac = power::on_ac_power();
            if ac != last_power {
                last_power = ac;
                eprintln!("power flip -> {}", if ac { "plugged" } else { "battery" });
                flow::push_panel(&app, "tiroSetEngine", flow::engine_dict(&ctx));
            }
            let eff = {
                let cfg = flow::lock(&ctx.cfg);
                api::effective_theme(&app, &cfg)
            };
            if last_theme.as_deref() != Some(eff.as_str()) {
                if last_theme.is_some() {
                    flow::push_panel(&app, "tiroSetTheme", serde_json::json!(eff));
                }
                last_theme = Some(eff);
            }
        }
    });
}

/// Tray icon: a control/recovery surface for the otherwise-invisible app.
/// Left-click toggles the panel; the right-click menu covers Open,
/// Start/Stop dictation, Restart, and Quit. Menu/click actions go through
/// the hotkey dispatcher so a slow action never blocks the main thread
/// (the original's `_tray_dispatch`). A tray failure is logged, never
/// fatal — the panel hotkey still works without it.
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open = MenuItem::with_id(app, "open", "Open Tiro", true, None::<&str>)?;
    let dictate = MenuItem::with_id(app, "dictate", "Start/Stop dictation", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "Restart Tiro", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Tiro", true, None::<&str>)?;
    let menu = MenuBuilder::new(app)
        .items(&[&open, &dictate, &restart, &quit])
        .build()?;
    let mut tray = TrayIconBuilder::with_id("tiro")
        .tooltip("Tiro")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => hotkeys::dispatch(app, "panel"),
            "dictate" => hotkeys::dispatch(app, "dictate"),
            "restart" => app.restart(),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                hotkeys::dispatch(tray.app_handle(), "panel");
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // LIFECYCLE-1: a second launch summons the running instance's
            // panel instead of starting another app.
            eprintln!("second instance launch -> summoning panel");
            hotkeys::summon_panel(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(flow::AppCtx::new())
        .manage(placement::Placement::default())
        .on_window_event(|window, event| {
            // Follow the OS light/dark setting while theme == "system" (the
            // original polled the registry every 20 s; Tauri delivers events).
            if let tauri::WindowEvent::ThemeChanged(theme) = event {
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
            power_watcher(app.handle().clone());
            if let Err(e) = build_tray(app) {
                eprintln!("tray unavailable: {e}");
            }
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
