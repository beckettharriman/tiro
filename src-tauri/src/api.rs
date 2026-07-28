//! The pywebview `Api` surface (PORTING_NOTES §3) against the real backend:
//! state snapshot, `set_setting` side effects, folder picker, and shortcut
//! rebinding, ported from the original's `Api` class and its config<->JS
//! value-mapping helpers.
//!
//! Not yet live here: the `powerMode` device swap (device orchestration,
//! task 3.5) — the config write is real, the side effect joins there.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::audio;
use crate::config::ConfigStore;
use crate::flow::{self, lock, AppCtx};
use crate::store;
use crate::transcribe;

// ---- config <-> JS value mapping helpers (ported 1:1) ----------------------

/// `_device_to_powermode`: cfg `device` -> UI power mode.
pub fn device_to_powermode(dev: &str) -> &'static str {
    match dev.to_lowercase().as_str() {
        "cpu" => "cpu",
        "cuda" => "gpu",
        _ => "auto",
    }
}

/// `_powermode_to_device`: UI power mode -> cfg `device`.
pub fn powermode_to_device(mode: &str) -> &'static str {
    match mode.to_lowercase().as_str() {
        "cpu" => "cpu",
        "gpu" => "cuda",
        _ => "auto",
    }
}

/// `_cleanup_to_js`: cfg `clipboard_cleanup` -> UI value.
pub fn cleanup_to_js(val: &str) -> &'static str {
    match val.to_lowercase().as_str() {
        "none" => "off",
        "fillers" => "fillers",
        _ => "light",
    }
}

/// `_cleanup_to_cfg`: UI value -> cfg `clipboard_cleanup`.
pub fn cleanup_to_cfg(val: &str) -> &'static str {
    match val.to_lowercase().as_str() {
        "off" => "none",
        "fillers" => "fillers",
        _ => "light",
    }
}

/// `_volume_to_int`: cfg `sound_volume` (0.0-1.0 string) -> 0-100 for the UI.
/// Python's `round` is banker's rounding, hence `round_ties_even`.
pub fn volume_to_int(raw: &str) -> i64 {
    match raw.trim().parse::<f64>() {
        Ok(f) if f.is_finite() => ((f * 100.0).round_ties_even() as i64).min(100),
        _ => 100,
    }
}

fn clamp_f64(f: Option<f64>, lo: i64, hi: i64, default: i64) -> i64 {
    match f {
        Some(f) if f.is_finite() => (f.round_ties_even() as i64).clamp(lo, hi),
        _ => default,
    }
}

/// `_clamp_int` over a JS value (number or numeric string).
pub fn clamp_int(raw: &Value, lo: i64, hi: i64, default: i64) -> i64 {
    let f = match raw {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    };
    clamp_f64(f, lo, hi, default)
}

/// `_clamp_int` over a raw config string.
pub fn clamp_int_str(raw: &str, lo: i64, hi: i64, default: i64) -> i64 {
    clamp_f64(raw.trim().parse::<f64>().ok(), lo, hi, default)
}

/// Python's `str.capitalize()`: first char upper, rest lower.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// `keys_to_combo`: a stored hotkey string -> the COMBO dict for `get_state`.
pub fn keys_to_combo(hotkey: &str) -> Value {
    let (mut ctrl, mut alt, mut shift, mut meta) = (false, false, false, false);
    let mut code = String::new();
    let mut keys: Vec<String> = Vec::new();
    for p in hotkey.split('+').filter(|p| !p.is_empty()) {
        match p.to_lowercase().as_str() {
            "ctrl" => {
                ctrl = true;
                keys.push("Ctrl".into());
            }
            "alt" => {
                alt = true;
                keys.push("Alt".into());
            }
            "shift" => {
                shift = true;
                keys.push("Shift".into());
            }
            "windows" | "win" | "meta" => {
                meta = true;
                keys.push("Win".into());
            }
            low => {
                let label = match low {
                    "esc" => "Esc".to_string(),
                    "space" => "Space".to_string(),
                    "enter" => "Enter".to_string(),
                    "tab" => "Tab".to_string(),
                    _ if p.chars().count() == 1 => p.to_uppercase(),
                    _ => capitalize(p),
                };
                keys.push(label);
                code = if p.chars().count() == 1 {
                    format!("Key{}", p.to_uppercase())
                } else {
                    capitalize(p)
                };
            }
        }
    }
    json!({ "ctrl": ctrl, "alt": alt, "shift": shift, "meta": meta,
            "code": code, "keys": keys })
}

/// `combo_to_hotkey`: a JS COMBO dict -> hotkey string, e.g.
/// `{keys:["Ctrl","Alt","Space"]}` -> `"ctrl+alt+space"`.
pub fn combo_to_hotkey(combo: &Value) -> String {
    let keys = combo
        .get("keys")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut parts: Vec<String> = Vec::new();
    for k in keys {
        let kk = match k {
            Value::String(s) => s,
            other => other.to_string(),
        };
        let kk = kk.trim();
        let low = kk.to_lowercase();
        let part = match low.as_str() {
            "ctrl" | "control" => "ctrl".to_string(),
            "alt" => "alt".to_string(),
            "shift" => "shift".to_string(),
            "win" | "windows" | "meta" | "cmd" => "windows".to_string(),
            "esc" | "escape" => "esc".to_string(),
            "space" | "spacebar" => "space".to_string(),
            "enter" | "return" => "enter".to_string(),
            "tab" => "tab".to_string(),
            _ if kk.chars().count() == 1 => low,
            _ => low,
        };
        if !part.is_empty() {
            parts.push(part);
        }
    }
    parts.join("+")
}

/// `_parse_hotkey` as a validity check: modifiers plus at least one mappable
/// key (named key, f1-f24, or any single character); an unknown multi-char
/// part rejects the whole combo. The Win32 VK mapping itself is not needed —
/// registration goes through tauri-plugin-global-shortcut in task 2.3.
pub fn parse_hotkey(hotkey: &str) -> bool {
    let mut has_key = false;
    for p in hotkey.to_lowercase().split('+') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        if matches!(
            p,
            "ctrl" | "control" | "alt" | "shift" | "win" | "windows" | "meta" | "cmd"
        ) {
            continue;
        }
        let named = matches!(
            p,
            "space"
                | "enter"
                | "return"
                | "tab"
                | "esc"
                | "escape"
                | "backspace"
                | "delete"
                | "insert"
                | "home"
                | "end"
                | "pageup"
                | "pagedown"
                | "up"
                | "down"
                | "left"
                | "right"
        );
        let fkey = p.strip_prefix('f').is_some_and(|n| {
            !n.starts_with('0') && n.parse::<u8>().is_ok_and(|v| (1..=24).contains(&v))
        });
        if named || fkey || p.chars().count() == 1 {
            has_key = true;
        } else {
            return false;
        }
    }
    has_key
}

// ---- state helpers ---------------------------------------------------------

/// `resolve_mic_name`: the stored substring resolved to the first matching
/// full device name (fallback: first mic / stored value).
pub fn resolve_mic_name(cfg: &ConfigStore) -> String {
    let cur = cfg.get("mic_name");
    let names = audio::list_mic_names();
    if names.is_empty() {
        return cur;
    }
    let cur_low = cur.to_lowercase();
    names
        .iter()
        .find(|n| n.to_lowercase().contains(&cur_low))
        .cloned()
        .unwrap_or_else(|| names[0].clone())
}

/// `launch_at_login_enabled`: whether the app is registered to start at
/// login (the original checked for its Startup-folder .lnk).
pub fn launch_at_login_enabled(app: &AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// `set_launch_at_login`: register/unregister the autostart entry;
/// best-effort like the original (a failure just leaves the toggle off).
pub fn set_launch_at_login(app: &AppHandle, on: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let result = if on {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    };
    if let Err(e) = result {
        eprintln!("launch-at-login update failed: {e}");
    }
}

/// On Linux, ask the XDG Settings portal for the live color-scheme. The
/// windowing layer can't be trusted here: tao latches the boot-time portal
/// value into the window's preferred theme, and its OS ThemeChanged events
/// carry a dummy window id that never reaches handlers — so a direct portal
/// read is the only current signal. `dbus-send` ships with D-Bus itself.
/// Returns None when the portal is unavailable (e.g. stock WSLg).
#[cfg(target_os = "linux")]
fn portal_color_scheme() -> Option<bool> {
    let out = std::process::Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply=literal",
            "--dest=org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings.Read",
            "string:org.freedesktop.appearance",
            "string:color-scheme",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let reply = String::from_utf8_lossy(&out.stdout);
    let value = reply.split("uint32").nth(1)?.trim().parse::<u32>().ok()?;
    Some(value == 1) // 1 = prefer dark; 0 (no preference) and 2 = light
}

/// The OS light/dark answer: portal on Linux (see above), the windowing
/// layer's live report elsewhere (the original read the Windows registry).
fn os_prefers_dark(app: &AppHandle) -> bool {
    #[cfg(target_os = "linux")]
    if let Some(dark) = portal_color_scheme() {
        return dark;
    }
    app.get_webview_window("panel")
        .and_then(|w| w.theme().ok())
        .is_none_or(|theme| !matches!(theme, tauri::Theme::Light))
}

/// `effective_theme`: resolve "system" to the OS setting.
pub fn effective_theme(app: &AppHandle, cfg: &ConfigStore) -> String {
    let t = cfg.get("theme").to_lowercase();
    match t.as_str() {
        "light" | "dark" => t,
        // "system" (the empty/unknown fallback chain ends in dark, matching
        // the original read_system_theme's default)
        _ if t == "system" || t.is_empty() => {
            if os_prefers_dark(app) {
                "dark".into()
            } else {
                "light".into()
            }
        }
        _ => "dark".into(),
    }
}

fn theme_or_system(cfg: &ConfigStore) -> String {
    let t = cfg.get("theme");
    if t.is_empty() {
        "system".into()
    } else {
        t
    }
}

/// Python truthiness for the JS values the settings toggles send.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Null => false,
    }
}

/// `str(value)` for config writes.
fn as_cfg_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---- the api methods -------------------------------------------------------

/// `get_state`: the full snapshot the panel renders from.
pub fn get_state(app: &AppHandle) -> Value {
    let ctx = app.state::<AppCtx>();
    let (entries, settings, shortcuts, theme, effective) = {
        let cfg = lock(&ctx.cfg);
        let entries: Vec<Value> = store::read_today_entries(&cfg, 200)
            .iter()
            .map(|r| store::entry_from_rec(r, &cfg))
            .collect();
        // STORAGE CONTRACT: report where writes actually land so the panel
        // can render the fallback banner when the vault dir is unwritable.
        let (store_path, is_vault) = store::log_dir(&cfg)
            .map(|(p, v)| (p.to_string_lossy().into_owned(), v))
            .unwrap_or((String::new(), false));
        let settings = json!({
            "powerMode": device_to_powermode(&cfg.get("device")),
            "modelBattery": cfg.get("model_battery"),
            "modelPlugged": cfg.get("model_ac"),
            "soundCues": cfg.get_bool("beeps"),
            "volume": volume_to_int(&cfg.get("sound_volume")),
            "recordingPill": cfg.get_bool("pill"),
            "clipboardCleanup": cleanup_to_js(&cfg.get("clipboard_cleanup")),
            "smartVocab": cfg.get_bool("use_vocab_bias"),
            "micName": resolve_mic_name(&cfg),
            "launchAtLogin": launch_at_login_enabled(app),
            "savePath": cfg.get("vault_dir"),
            "transparency": clamp_int_str(&cfg.get("panel_transparency"), 0, 100, 45),
            "storageFallback": !is_vault,
            "storagePath": store_path,
        });
        let shortcuts = json!({
            "dictate": keys_to_combo(&cfg.get("dictation_hotkey")),
            "panel": keys_to_combo(&cfg.get("panel_hotkey")),
            "cancel": keys_to_combo(&cfg.get("cancel_hotkey")),
        });
        (
            entries,
            settings,
            shortcuts,
            theme_or_system(&cfg),
            effective_theme(app, &cfg),
        )
    };
    json!({
        "entries": entries,
        "settings": settings,
        "engine": flow::engine_dict(&ctx),
        "mics": audio::list_mic_names(),
        "shortcuts": shortcuts,
        "theme": theme,
        "effectiveTheme": effective,
    })
}

/// The `_ack` payload every `set_setting` returns.
fn ack(app: &AppHandle, ctx: &AppCtx) -> Value {
    let (theme, effective) = {
        let cfg = lock(&ctx.cfg);
        (theme_or_system(&cfg), effective_theme(app, &cfg))
    };
    json!({
        "ok": true,
        "engine": flow::engine_dict(ctx),
        "theme": theme,
        "effectiveTheme": effective,
        "launchAtLogin": launch_at_login_enabled(app),
    })
}

/// Model names with a download currently in flight — prevents two threads
/// pulling the same file at once (different models concurrently are fine).
static DOWNLOADS: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// `list_models`: the model manager's catalog snapshot — every catalog entry
/// with its plain-language hint, resolved file name/size for the current
/// `compute_type`, and live install/download state.
pub fn list_models(app: &AppHandle) -> Value {
    let ctx = app.state::<AppCtx>();
    let compute_type = lock(&ctx.cfg).get("compute_type");
    let models_dir = flow::app_dir().join("models");
    let in_flight = lock(&DOWNLOADS).clone();
    let list: Vec<Value> = transcribe::CATALOG
        .iter()
        .map(|m| {
            let status = transcribe::model_status(&models_dir, m.name, &compute_type);
            json!({
                "name": m.name,
                "hint": m.hint,
                "curated": m.curated,
                "file": m.file_name(&compute_type),
                "sizeBytes": m.size_bytes(&compute_type),
                "installed": status.installed,
                "installedBytes": status.bytes,
                "downloading": in_flight.contains(m.name),
            })
        })
        .collect();
    json!(list)
}

/// `download_model`: start a background download of one catalog model,
/// streaming progress to the panel via `tiroModelProgress` pushes (throttled
/// to ~1% steps). Failures are pushed as an error payload, never fatal.
pub fn download_model(app: &AppHandle, name: &str) -> Value {
    let Some(info) = transcribe::catalog_find(name) else {
        return json!({ "ok": false, "error": format!("unknown model '{name}'") });
    };
    let ctx = app.state::<AppCtx>();
    let compute_type = lock(&ctx.cfg).get("compute_type");
    let models_dir = flow::app_dir().join("models");
    if transcribe::model_status(&models_dir, info.name, &compute_type).installed {
        return json!({ "ok": true, "installed": true });
    }
    if !lock(&DOWNLOADS).insert(info.name.to_string()) {
        return json!({ "ok": true, "downloading": true }); // already in flight
    }
    let app = app.clone();
    let model = info.name.to_string();
    std::thread::spawn(move || {
        let mut last_pct: i64 = -1;
        let result = transcribe::download_model_with(
            &models_dir,
            &model,
            &compute_type,
            &mut |done, total| {
                let Some(total) = total.filter(|t| *t > 0) else {
                    return;
                };
                let pct = (done * 100 / total) as i64;
                if pct > last_pct {
                    last_pct = pct;
                    flow::push_panel(
                        &app,
                        "tiroModelProgress",
                        json!({ "model": model, "pct": pct, "done": false, "error": Value::Null }),
                    );
                }
            },
        );
        lock(&DOWNLOADS).remove(&model);
        let payload = match result {
            Ok(_) => json!({ "model": model, "pct": 100, "done": true, "error": Value::Null }),
            Err(e) => {
                eprintln!("model download failed: {e}");
                json!({ "model": model, "pct": last_pct.max(0), "done": false, "error": e })
            }
        };
        flow::push_panel(&app, "tiroModelProgress", payload);
    });
    json!({ "ok": true, "started": true })
}

/// Force an engine reload after a battery/plugged model change — otherwise
/// the new choice would only take effect on the next device swap. Tear down
/// whatever serves right now (worker stopped, CPU model dropped) under the
/// engine lock — which serializes with any in-flight take — then bring the
/// resolved target back up, which loads the newly-configured model.
fn hot_apply_model_change(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let ctx = app.state::<AppCtx>();
        {
            let mut engine = lock(&ctx.engine);
            if let Some(mut w) = engine.worker.take() {
                w.stop();
            }
            engine.transcriber = None;
        }
        let target = flow::resolve_target(&ctx);
        flow::ensure_device(&app, target);
    });
}

/// `set_setting`: change one setting, apply live.
pub fn set_setting(app: &AppHandle, key: &str, value: &Value) -> Value {
    let ctx = app.state::<AppCtx>();
    let mut theme_push: Option<String> = None;
    {
        let mut cfg = lock(&ctx.cfg);
        match key {
            "powerMode" => {
                cfg.set(
                    "device",
                    powermode_to_device(value.as_str().unwrap_or("auto")),
                );
                // apply live off the command thread, like the original's
                // background ensure_device(resolve_target())
                let app = app.clone();
                std::thread::spawn(move || {
                    let target = flow::resolve_target(&app.state::<AppCtx>());
                    flow::ensure_device(&app, target);
                });
            }
            "modelBattery" => {
                cfg.set("model_battery", &as_cfg_str(value));
                hot_apply_model_change(app);
            }
            "modelPlugged" => {
                cfg.set("model_ac", &as_cfg_str(value));
                hot_apply_model_change(app);
            }
            "soundCues" => cfg.set("beeps", if truthy(value) { "true" } else { "false" }),
            "volume" => {
                // int(value) truncates; the stored form is Python's
                // str(n / 100.0), i.e. always with a decimal point.
                let n = match value {
                    Value::Number(v) => v.as_f64().map(|f| f.trunc() as i64),
                    Value::String(s) => s
                        .trim()
                        .parse::<i64>()
                        .ok()
                        .or_else(|| s.trim().parse::<f64>().ok().map(|f| f.trunc() as i64)),
                    _ => None,
                };
                if let Some(n) = n {
                    let mut s = format!("{}", n.clamp(0, 100) as f64 / 100.0);
                    if !s.contains('.') {
                        s.push_str(".0");
                    }
                    cfg.set("sound_volume", &s);
                }
            }
            "recordingPill" => cfg.set("pill", if truthy(value) { "true" } else { "false" }),
            "clipboardCleanup" => cfg.set(
                "clipboard_cleanup",
                cleanup_to_cfg(value.as_str().unwrap_or("")),
            ),
            "smartVocab" => cfg.set(
                "use_vocab_bias",
                if truthy(value) { "true" } else { "false" },
            ),
            "micName" => cfg.set("mic_name", &as_cfg_str(value)),
            "transparency" => {
                cfg.set(
                    "panel_transparency",
                    &clamp_int(value, 0, 100, 45).to_string(),
                );
            }
            "launchAtLogin" => set_launch_at_login(app, truthy(value)),
            "theme" => {
                let mut t = as_cfg_str(value).to_lowercase();
                if !matches!(t.as_str(), "system" | "light" | "dark") {
                    t = "system".into();
                }
                cfg.set("theme", &t);
                theme_push = Some(effective_theme(app, &cfg));
            }
            _ => {}
        }
    }
    if let Some(effective) = theme_push {
        flow::push_panel(app, "tiroSetTheme", json!(effective));
    }
    ack(app, &ctx)
}

/// `pick_folder`: native folder dialog; a selection is persisted as the new
/// vault dir. Blocking is fine — commands never run on the main thread.
pub fn pick_folder(app: &AppHandle) -> Option<Value> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app.dialog().file().blocking_pick_folder()?;
    let path = match picked.into_path() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => return None,
    };
    let ctx = app.state::<AppCtx>();
    lock(&ctx.cfg).set("vault_dir", &path);
    Some(json!({ "path": path }))
}

/// `rebind_shortcut`: validate + persist a new hotkey, then re-register all
/// hotkeys live (the original's `_request_rebind`).
pub fn rebind_shortcut(app: &AppHandle, which: &str, combo: &Value) -> Value {
    let keys = combo.get("keys").cloned().unwrap_or_else(|| json!([]));
    let cfg_key = match which {
        "dictate" => "dictation_hotkey",
        "panel" => "panel_hotkey",
        "cancel" => "cancel_hotkey",
        _ => return json!({ "ok": false, "keys": keys }),
    };
    let hotkey = combo_to_hotkey(combo);
    if hotkey.is_empty() || !parse_hotkey(&hotkey) {
        return json!({ "ok": false, "keys": keys });
    }
    let ctx = app.state::<AppCtx>();
    lock(&ctx.cfg).set(cfg_key, &hotkey);
    crate::hotkeys::register_all(app);
    json!({ "ok": true, "keys": keys })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powermode_mappings_round_trip() {
        for (dev, mode) in [("cpu", "cpu"), ("cuda", "gpu"), ("auto", "auto")] {
            assert_eq!(device_to_powermode(dev), mode);
            assert_eq!(powermode_to_device(mode), dev);
        }
        assert_eq!(device_to_powermode("garbage"), "auto");
        assert_eq!(powermode_to_device("garbage"), "auto");
        assert_eq!(device_to_powermode("CUDA"), "gpu", "case-insensitive");
    }

    #[test]
    fn cleanup_mappings_round_trip() {
        for (cfg, js) in [("none", "off"), ("fillers", "fillers"), ("light", "light")] {
            assert_eq!(cleanup_to_js(cfg), js);
            assert_eq!(cleanup_to_cfg(js), cfg);
        }
        assert_eq!(cleanup_to_js("garbage"), "light");
        assert_eq!(cleanup_to_cfg("garbage"), "light");
    }

    #[test]
    fn volume_matches_python() {
        assert_eq!(volume_to_int("1.0"), 100);
        assert_eq!(volume_to_int("0.67"), 67);
        assert_eq!(volume_to_int("0.0"), 0);
        assert_eq!(volume_to_int("1.5"), 100, "clamped to 100");
        assert_eq!(volume_to_int("garbage"), 100, "unparsable -> default");
        assert_eq!(volume_to_int(""), 100);
        assert_eq!(volume_to_int("0.005"), 0, "round-half-even like Python");
        assert_eq!(volume_to_int("0.015"), 2, "round-half-even like Python");
    }

    #[test]
    fn clamp_int_matches_python() {
        assert_eq!(clamp_int_str("45", 0, 100, 45), 45);
        assert_eq!(clamp_int_str("150", 0, 100, 45), 100);
        assert_eq!(clamp_int_str("-3", 0, 100, 45), 0);
        assert_eq!(clamp_int_str("garbage", 0, 100, 45), 45);
        assert_eq!(clamp_int(&json!(72.6), 0, 100, 45), 73, "rounds");
        assert_eq!(clamp_int(&json!("30"), 0, 100, 45), 30);
        assert_eq!(clamp_int(&json!(null), 0, 100, 45), 45);
    }

    #[test]
    fn keys_to_combo_matches_python() {
        let c = keys_to_combo("ctrl+alt+space");
        assert_eq!(c["ctrl"], true);
        assert_eq!(c["alt"], true);
        assert_eq!(c["shift"], false);
        assert_eq!(c["meta"], false);
        assert_eq!(c["code"], "Space");
        assert_eq!(c["keys"], json!(["Ctrl", "Alt", "Space"]));

        let c = keys_to_combo("ctrl+alt+v");
        assert_eq!(c["code"], "KeyV");
        assert_eq!(c["keys"], json!(["Ctrl", "Alt", "V"]));

        let c = keys_to_combo("shift+windows+f5");
        assert_eq!(c["shift"], true);
        assert_eq!(c["meta"], true);
        assert_eq!(c["code"], "F5", "capitalize()");
        assert_eq!(c["keys"], json!(["Shift", "Win", "F5"]));

        let c = keys_to_combo("");
        assert_eq!(c["code"], "");
        assert_eq!(c["keys"], json!([]));
    }

    #[test]
    fn combo_to_hotkey_matches_python() {
        assert_eq!(
            combo_to_hotkey(&json!({"keys": ["Ctrl", "Alt", "Space"]})),
            "ctrl+alt+space"
        );
        assert_eq!(
            combo_to_hotkey(&json!({"keys": ["Cmd", "Return", "Escape"]})),
            "windows+enter+esc",
            "aliases normalize"
        );
        assert_eq!(
            combo_to_hotkey(&json!({"keys": ["Ctrl", "V"]})),
            "ctrl+v",
            "single chars lowercase"
        );
        assert_eq!(combo_to_hotkey(&json!({})), "");
    }

    #[test]
    fn rebind_round_trip_via_both_converters() {
        for hk in ["ctrl+alt+space", "ctrl+alt+v", "ctrl+alt+x", "shift+f12"] {
            let combo = keys_to_combo(hk);
            assert_eq!(combo_to_hotkey(&combo), hk);
        }
    }

    #[test]
    fn parse_hotkey_validation() {
        for ok in [
            "ctrl+alt+space",
            "ctrl+alt+v",
            "f12",
            "ctrl+shift+pageup",
            "win+`",
        ] {
            assert!(parse_hotkey(ok), "{ok} should be valid");
        }
        for bad in ["", "ctrl+alt", "ctrl+bogus", "ctrl+f25", "ctrl+f01"] {
            assert!(!parse_hotkey(bad), "{bad} should be invalid");
        }
    }
}
