//! Tauri entry point: window setup and the pywebview-shaped command surface
//! (PORTING_NOTES §3). The commands are thin wrappers — the real api logic
//! lives in `api`, the recording state machine in `flow`.

pub mod api;
#[cfg(target_os = "linux")]
pub mod app_scope;
pub mod audio;
pub mod clipboard;
pub mod config;
pub mod cues;
pub mod flow;
pub mod gpu;
pub mod gpu_worker;
pub mod hotkeys;
#[cfg(target_os = "linux")]
pub mod hotkeys_portal;
pub mod hw;
pub mod inject;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod placement;
pub mod power;
pub mod store;
pub mod transcribe;
pub mod vocab;

use serde_json::Value;
use tauri::{State, WebviewWindow};

// Async on purpose: sync commands run inline in the webview IPC handler ON
// THE MAIN THREAD (Linux/WebKitGTK), and get_state is the heavy snapshot
// (mic enumeration, transcript read — easily a second). The body itself is
// synchronous, so it goes through spawn_blocking: the shared async pool
// (portal listeners live there) must not stall on it either. The panel
// treats the result as a promise either way; a join failure surfaces as an
// invoke rejection, same as a panic did when the command was sync.
#[tauri::command]
async fn get_state(app: tauri::AppHandle) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || api::get_state(&app))
        .await
        .map_err(|e| e.to_string())
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
async fn toggle_record(app: tauri::AppHandle) -> Result<(), String> {
    // Opening a microphone can include the first macOS permission dialog.
    // Keep the event loop responsive while the recording thread opens it.
    tauri::async_runtime::spawn_blocking(move || flow::toggle_record(&app))
        .await
        .map_err(|e| e.to_string())
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
fn set_expanded(on: bool, app: tauri::AppHandle) {
    placement::set_panel_expanded(&app, on);
}

#[tauri::command]
fn close_panel(window: WebviewWindow) -> Result<(), String> {
    // Capture the panel's spot while it is still mapped (mirrors the hotkey
    // hide path) so closing via the X button also remembers the position.
    use tauri::Manager;
    let app = window.app_handle().clone();
    window
        .run_on_main_thread(move || {
            placement::remember_panel_now(&app);
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.hide();
            }
        })
        .map_err(|e| e.to_string())
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

// Disk reads — spawn_blocking for the same reason as get_state.
#[tauri::command]
async fn history_days(app: tauri::AppHandle) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || api::history_days(&app))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn history_entries(app: tauri::AppHandle, day: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || api::history_entries(&app, &day))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn list_vocab() -> Value {
    vocab::list(&flow::app_dir())
}

#[tauri::command]
fn set_vocab(hotwords: Value, corrections: Value) -> Value {
    vocab::set(&flow::app_dir(), &hotwords, &corrections)
}

// Opening the mic can take a moment and sync commands run inline on the
// main thread (Linux/WebKitGTK) — same reasoning as get_state.
#[tauri::command]
async fn start_mic_monitor(app: tauri::AppHandle) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || flow::start_mic_monitor(&app))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn stop_mic_monitor(app: tauri::AppHandle) {
    flow::stop_mic_monitor(&app);
}

#[tauri::command]
fn list_models(app: tauri::AppHandle) -> Value {
    api::list_models(&app)
}

#[tauri::command]
fn download_model(app: tauri::AppHandle, name: String) -> Value {
    api::download_model(&app, &name)
}

#[tauri::command]
fn cancel_download(name: String) -> Value {
    api::cancel_download(&name)
}

/// The original's 20 s `power_watcher` tick: keep the engine chip's power
/// label and the system theme in sync with the live machine state. (Theme
/// polling is needed because on Linux tao emits OS ThemeChanged with a
/// dummy window id that never reaches on_window_event; the device swap on
/// power flips joins in task 3.5.)
///
/// On DESKTOP machines (no battery, or treat_as_desktop) the AC/battery
/// half is inert: no power sampling, no flip pushes — the policy table has
/// no power-dependent cell there. The dead-worker latch and the policy
/// reconcile stay on every class (they are crash recovery, not power
/// flapping).
fn power_watcher(app: tauri::AppHandle) {
    use tauri::Manager;
    std::thread::spawn(move || {
        let mut last_theme: Option<String> = None;
        let mut last_power = power::on_ac_power();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(20));
            let ctx = app.state::<flow::AppCtx>();
            // If the GPU worker died on its own (crash / driver reset), don't
            // keep claiming GPU: latch it off so resolve_target steers to CPU
            // below; the periodic re-probe allows a respawn later.
            {
                let engine = flow::lock(&ctx.engine);
                if engine.device == "gpu" && !engine.worker.as_ref().is_some_and(|w| w.alive()) {
                    eprintln!("power: GPU worker died unexpectedly; latching GPU off");
                    flow::latch_gpu_off(&ctx);
                }
            }
            let target = flow::resolve_target(&ctx);
            // Device AND model must match the policy cell — an integrated
            // laptop swaps models on a power flip without changing device.
            if !flow::engine_in_policy(&ctx, target) {
                eprintln!("Power/device change -> switching to {target}");
                flow::ensure_device(&app, target); // kills/spawns; pushes the chip
            }
            if !flow::machine_is_desktop(&ctx) {
                let ac = power::on_ac_power();
                if ac != last_power {
                    last_power = ac;
                    eprintln!("power flip -> {}", if ac { "plugged" } else { "battery" });
                    // power may have flipped without a device swap; keep the
                    // footer chip's power label current.
                    flow::push_panel(&app, "tiroSetEngine", flow::engine_dict(&ctx));
                }
            }
            let eff = {
                // snapshot then drop the lock: effective_theme can shell
                // out to the portal (dbus-send)
                let cfg = flow::lock(&ctx.cfg).clone();
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

/// The three tray icon pixel buffers (RGBA), built once from the app's
/// default window icon: idle (untouched) plus recording/transcribing
/// variants with a status dot stamped in the bottom-right corner. Kept as
/// raw buffers in a static so `set_tray_state` can hand `tauri::image::
/// Image::new` a `'static` borrow without copying per call.
struct TrayIconSet {
    width: u32,
    height: u32,
    idle: Vec<u8>,
    recording: Vec<u8>,
    transcribing: Vec<u8>,
}

static TRAY_ICONS: std::sync::OnceLock<Option<TrayIconSet>> = std::sync::OnceLock::new();

/// Stamp a filled status dot (diameter ~35% of the icon's smaller side)
/// into the bottom-right corner of an RGBA buffer, fully opaque, with a
/// ~1 px darker rim so it reads against both light and dark trays.
fn stamp_status_dot(rgba: &mut [u8], width: u32, height: u32, color: [u8; 3]) {
    let size = width.min(height) as f32;
    let radius = size * 0.175; // dot diameter = 35% of icon size
    let margin = size * 0.02;
    let cx = width as f32 - radius - margin;
    let cy = height as f32 - radius - margin;
    let rim = color.map(|c| (f32::from(c) * 0.55) as u8);
    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            let fill = if d <= radius - 1.0 {
                Some(color)
            } else if d <= radius {
                Some(rim)
            } else {
                None
            };
            if let Some(c) = fill {
                let i = ((y * width + x) * 4) as usize;
                if i + 3 < rgba.len() {
                    rgba[i] = c[0];
                    rgba[i + 1] = c[1];
                    rgba[i + 2] = c[2];
                    rgba[i + 3] = 255;
                }
            }
        }
    }
}

/// Swap the tray icon + tooltip to reflect the dictation state ("idle",
/// "recording", "transcribing"). Non-fatal like the rest of the tray: if
/// the tray never built (or the app had no icon) this is a no-op, and a
/// failed swap only logs.
pub fn set_tray_state(app: &tauri::AppHandle, state: &str) {
    let Some(tray) = app.tray_by_id("tiro") else {
        return;
    };
    let icons = TRAY_ICONS.get().and_then(Option::as_ref);
    let (buf, tooltip) = match state {
        "recording" => (icons.map(|i| &i.recording), "Tiro — recording…"),
        "transcribing" => (icons.map(|i| &i.transcribing), "Tiro — transcribing…"),
        _ => (icons.map(|i| &i.idle), "Tiro"),
    };
    if let (Some(buf), Some(set)) = (buf, icons) {
        let image = tauri::image::Image::new(buf, set.width, set.height);
        if let Err(e) = tray.set_icon(Some(image)) {
            eprintln!("tray icon swap failed: {e}");
        }
    }
    if let Err(e) = tray.set_tooltip(Some(tooltip)) {
        eprintln!("tray tooltip update failed: {e}");
    }
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
            // Kill the GPU worker FIRST: exiting without it would leave an
            // orphan holding the dGPU awake until its EOF backstop fires.
            "restart" => {
                flow::stop_worker(app);
                // request_restart, NOT restart: on the main thread (where
                // menu events run) plain restart skips ExitRequested/Exit,
                // and the portal-session close hooks RunEvent::Exit.
                app.request_restart()
            }
            "quit" => {
                flow::stop_worker(app);
                app.exit(0)
            }
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
    // Build the state-icon cache once, from the same default icon.
    TRAY_ICONS.get_or_init(|| {
        app.default_window_icon().map(|icon| {
            let (width, height) = (icon.width(), icon.height());
            let idle = icon.rgba().to_vec();
            let mut recording = idle.clone();
            stamp_status_dot(&mut recording, width, height, [0xE5, 0x48, 0x4D]);
            let mut transcribing = idle.clone();
            stamp_status_dot(&mut transcribing, width, height, [0xF5, 0xA5, 0x24]);
            TrayIconSet {
                width,
                height,
                idle,
                recording,
                transcribing,
            }
        })
    });
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Adopt CWD-era stray config/vocab files into the canonical app dir
    // BEFORE anything loads the config (AppCtx::new below reads it).
    flow::adopt_stray_app_files();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // LIFECYCLE-1: a second launch reaches the running instance.
            // With an action flag it acts as a remote control — the CLI
            // fallback for Wayland sessions where global hotkeys can't be
            // grabbed (bind a DE-level shortcut to `tiro --toggle`, see
            // BUILDING.md). A plain second launch summons the panel.
            if argv.iter().any(|a| a == "--toggle") {
                eprintln!("second instance: --toggle");
                hotkeys::dispatch(app, "dictate");
            } else if argv.iter().any(|a| a == "--cancel") {
                eprintln!("second instance: --cancel");
                hotkeys::dispatch(app, "cancel");
            } else if argv.iter().any(|a| a == "--paste") {
                eprintln!("second instance: --paste");
                hotkeys::dispatch(app, "paste");
            } else if argv.iter().any(|a| a == "--panel") {
                eprintln!("second instance: --panel");
                hotkeys::dispatch(app, "panel");
            } else {
                eprintln!("second instance launch -> summoning panel");
                hotkeys::summon_panel(app);
            }
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
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            // On Linux the WebKitGTK widget reports a ~200 px minimum height,
            // so GTK refuses to make the pill window its configured 76 px.
            // Clear the size request on every descendant widget and re-apply
            // the intended size at the GTK level. (76 = the design's pill
            // stage: a 44 px pill centered with headroom for the 14 px
            // entrance travel.)
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
                        gtk_win.resize(300, 76);
                    }
                }
            }
            // The pill is a pure overlay: it must never take keyboard focus,
            // or showing it mid-take would steal focus from the app the user
            // is dictating into (paste-at-cursor depends on the target app
            // keeping focus). `focusable: false` in tauri.conf.json sets
            // this at creation (GTK accept_focus(false) / WM_HINTS input on
            // X11, WS_EX_NOACTIVATE on Windows); re-assert it here so a
            // config regression can't quietly bring focus stealing back.
            {
                use tauri::Manager;
                if let Some(pill) = app.webview_windows().get("pill") {
                    let _ = pill.set_focusable(false);
                }
            }
            // WM_HINTS input=False alone is NOT enough on X11: GDK also
            // advertises WM_TAKE_FOCUS, which keeps the pill in the ICCCM
            // "Globally Active" input model that KWin still activates on
            // map (live failure: showing the pill deselected Chrome's
            // search bar mid-dictation). Strip it to reach the true
            // "No Input" model. See placement::pill_no_input_fixup.
            #[cfg(target_os = "linux")]
            placement::pill_no_input_fixup(app);
            // The panel window is created at the EXPANDED footprint
            // (800x560, tauri.conf.json) and boots compact:
            // - Linux: the footprint is permanent — X11's non-atomic
            //   move+resize made the old native expand/collapse flash the
            //   glass sideways — so booting compact only means shaping
            //   input down to the glass rect (the transparent left margin
            //   must not eat clicks), re-asserted on every realize/map.
            // - Windows: the native resize path is kept (DWM applies
            //   move+resize atomically), so this shrinks the window to the
            //   compact size now, before its first show.
            #[cfg(target_os = "linux")]
            placement::panel_input_fixup(app);
            placement::pin_webview_zoom(app);
            placement::set_panel_expanded(app.handle(), false);
            // The panel is configured hidden (summoned by hotkey/tray, which
            // land in phase 2/3); dev builds show it at startup so there is
            // something to work against. The pill is driven by the recording
            // state machine.
            #[cfg(any(debug_assertions, target_os = "macos"))]
            {
                use tauri::Manager;
                if let Some(w) = app.webview_windows().get("panel") {
                    let _ = w.show();
                }
                placement::reposition_burst(app.handle(), "panel");
            }
            // The GlobalShortcuts portal identifies callers by their systemd
            // unit; claim an app-<id> scope BEFORE hotkeys::register_all
            // spawns the portal worker (first D-Bus contact resolves — and
            // caches — our app id). Setup only runs in the primary instance,
            // so forwarding launches never churn scopes.
            #[cfg(target_os = "linux")]
            app_scope::ensure_app_scope(&app.config().identifier);
            // Detect the hardware class off-thread NOW (the --gpu-enum
            // child + battery probe) so the first get_state / resolve
            // never pays the spawn latency inline.
            hw::warm_up();
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
            set_expanded,
            close_panel,
            begin_drag,
            pick_folder,
            rebind_shortcut,
            history_days,
            history_entries,
            list_vocab,
            set_vocab,
            start_mic_monitor,
            stop_mic_monitor,
            list_models,
            download_model,
            cancel_download
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                // Graceful exits funnel through here — tray Quit (app.exit)
                // and tray Restart (request_restart) both raise
                // RunEvent::Exit. SIGTERM/SIGKILL never reach it; for those
                // the backstop is process death dropping the D-Bus
                // connection, which ends the portal session. Close the
                // Wayland portal session exactly once, now.
                inject::close_portal_session();
            }
        });
}

#[cfg(test)]
mod tray_icon_tests {
    use super::stamp_status_dot;

    fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn dot_stamps_bottom_right_and_leaves_corners_alone() {
        const W: u32 = 32;
        const H: u32 = 32;
        let mut buf = vec![0u8; (W * H * 4) as usize];
        stamp_status_dot(&mut buf, W, H, [0xE5, 0x48, 0x4D]);
        // radius = 32*0.175 = 5.6, margin 0.64 -> center ~ (25.76, 25.76)
        assert_eq!(
            px(&buf, W, 25, 25),
            [0xE5, 0x48, 0x4D, 255],
            "dot center is the fill color, opaque"
        );
        for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1)] {
            assert_eq!(
                px(&buf, W, x, y),
                [0, 0, 0, 0],
                "corner ({x},{y}) untouched"
            );
        }
        assert_eq!(
            px(&buf, W, W - 1, H - 1),
            [0, 0, 0, 0],
            "the very bottom-right corner sits outside the circle"
        );
        // a rim pixel exists: darker than fill, still opaque
        let rim = [
            (0xE5 as f32 * 0.55) as u8,
            (0x48 as f32 * 0.55) as u8,
            (0x4D as f32 * 0.55) as u8,
            255,
        ];
        let has_rim = (0..H).any(|y| (0..W).any(|x| px(&buf, W, x, y) == rim));
        assert!(has_rim, "rim ring pixels present");
        // dot covers roughly pi*r^2 pixels (r=5.6 -> ~98), sanity band
        let colored = (0..H)
            .flat_map(|y| (0..W).map(move |x| (x, y)))
            .filter(|&(x, y)| px(&buf, W, x, y)[3] == 255)
            .count();
        assert!(
            (70..=130).contains(&colored),
            "dot area ~ pi*r^2, got {colored}"
        );
    }

    #[test]
    fn dot_survives_tiny_icons_without_panicking() {
        let mut buf = vec![0u8; 4 * 4 * 4];
        stamp_status_dot(&mut buf, 4, 4, [0xF5, 0xA5, 0x24]);
        // just must not panic; something near bottom-right may be colored
    }
}
