//! Global hotkeys, ported from the original's RegisterHotKey listener:
//! the three configured combos dispatch to toggle/panel/cancel, a repeat
//! press of an action still running is dropped (no pile-up / double-toggle),
//! and `register_all` re-registers live after a rebind (`_request_rebind`).
//!
//! Registration goes through tauri-plugin-global-shortcut (Win32 on Windows,
//! X11 on Linux — the Wayland portal story is task 4.3).

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::api;
use crate::flow::{self, lock, AppCtx};

/// which -> config key, in the original's registration order.
const ACTIONS: [(&str, &str); 3] = [
    ("dictate", "dictation_hotkey"),
    ("panel", "panel_hotkey"),
    ("cancel", "cancel_hotkey"),
];

/// Map one lowercase config hotkey part to the W3C `Code` name the shortcut
/// parser expects ("space" -> "Space", "v" -> "KeyV", "5" -> "Digit5").
fn key_code(part: &str) -> Option<String> {
    let named = match part {
        "space" => "Space",
        "enter" | "return" => "Enter",
        "tab" => "Tab",
        "esc" | "escape" => "Escape",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "`" => "Backquote",
        "-" => "Minus",
        "=" => "Equal",
        "[" => "BracketLeft",
        "]" => "BracketRight",
        "\\" => "Backslash",
        ";" => "Semicolon",
        "'" => "Quote",
        "," => "Comma",
        "." => "Period",
        "/" => "Slash",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_string());
    }
    if let Some(n) = part.strip_prefix('f') {
        if !n.starts_with('0') && n.parse::<u8>().is_ok_and(|v| (1..=24).contains(&v)) {
            return Some(format!("F{n}"));
        }
    }
    let mut chars = part.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => {
            Some(format!("Key{}", c.to_ascii_uppercase()))
        }
        (Some(c), None) if c.is_ascii_digit() => Some(format!("Digit{c}")),
        _ => None,
    }
}

/// A config hotkey string ("ctrl+alt+space") -> the plugin's accelerator
/// form ("Ctrl+Alt+Space"). Like the original's `_parse_hotkey`, the last
/// mappable non-modifier part wins; an unmappable one rejects the combo.
pub fn to_accelerator(hotkey: &str) -> Option<String> {
    let mut mods: Vec<&str> = Vec::new();
    let mut key: Option<String> = None;
    for p in hotkey.to_lowercase().split('+') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        match p {
            "ctrl" | "control" => {
                if !mods.contains(&"Ctrl") {
                    mods.push("Ctrl");
                }
            }
            "alt" => {
                if !mods.contains(&"Alt") {
                    mods.push("Alt");
                }
            }
            "shift" => {
                if !mods.contains(&"Shift") {
                    mods.push("Shift");
                }
            }
            "win" | "windows" | "meta" | "cmd" => {
                if !mods.contains(&"Super") {
                    mods.push("Super");
                }
            }
            _ => key = Some(key_code(p)?),
        }
    }
    let key = key?;
    let mut parts = mods;
    parts.push(&key);
    Some(parts.join("+"))
}

fn inflight() -> &'static Mutex<HashSet<&'static str>> {
    static INFLIGHT: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    INFLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// `_dispatch_hotkey`: run the action OFF the listener thread; drop a repeat
/// press of the same action while the prior one is still running.
fn dispatch(app: &AppHandle, which: &'static str) {
    {
        let mut set = inflight().lock().unwrap_or_else(|e| e.into_inner());
        if !set.insert(which) {
            eprintln!("hotkey '{which}' ignored (previous still running)");
            return;
        }
    }
    let app = app.clone();
    std::thread::spawn(move || {
        match which {
            "dictate" => flow::toggle_record(&app),
            "panel" => toggle_panel(&app),
            "cancel" => flow::cancel_record(&app),
            _ => {}
        }
        inflight()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(which);
    });
}

/// `toggle_panel`: hide the panel if visible, else re-render it from a fresh
/// snapshot and summon it. (Tauri reports visibility reliably, so no manual
/// tracking like pywebview needed; centering-on-summon joins in task 2.4.)
///
/// GTK window operations are only safe on the main thread, and hotkey
/// dispatch runs on a worker — so the state snapshot (slow: mic enumeration)
/// is computed here and the window ops are handed to the main thread.
pub fn toggle_panel(app: &AppHandle) {
    let Some(w) = app.get_webview_window("panel") else {
        return;
    };
    if w.is_visible().unwrap_or(false) {
        let app = app.clone();
        let _ = app.clone().run_on_main_thread(move || {
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.hide();
            }
        });
    } else {
        let state = api::get_state(app);
        let app = app.clone();
        let _ = app.clone().run_on_main_thread(move || {
            flow::push_panel(&app, "tiroApplyState", state);
            if let Some(w) = app.get_webview_window("panel") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        });
    }
}

/// ERRORS-10: the panel hotkey is the only way into an otherwise-invisible
/// app — a registration failure gets the one sanctioned visible message box.
fn panel_hotkey_warning(app: &AppHandle, hk: &str) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
    app.dialog()
        .message(format!(
            "Tiro could not register the panel hotkey:\n\n    {hk}\n\n\
             Another app is likely using this combination, so the Tiro panel \
             can't be opened. Edit panel_hotkey in config.ini (next to the \
             app) to a free combination, then restart Tiro."
        ))
        .title("Tiro — panel hotkey unavailable")
        .kind(MessageDialogKind::Warning)
        .show(|_| {});
}

/// `_register_all_hotkeys`: (re)register every configured hotkey; an
/// unmappable or conflicting one is skipped with a log, never a crash.
pub fn register_all(app: &AppHandle) {
    let gs = app.global_shortcut();
    if let Err(e) = gs.unregister_all() {
        eprintln!("hotkey unregister_all failed: {e}");
    }
    let ctx = app.state::<AppCtx>();
    let bindings: Vec<(&'static str, String)> = {
        let cfg = lock(&ctx.cfg);
        ACTIONS
            .iter()
            .map(|(which, key)| (*which, cfg.get(key)))
            .collect()
    };
    for (which, hk) in bindings {
        if !api::parse_hotkey(&hk) {
            eprintln!("hotkey '{which}' = '{hk}' is unmappable; skipped");
            continue;
        }
        let Some(accel) = to_accelerator(&hk) else {
            eprintln!("hotkey '{which}' = '{hk}' is unmappable; skipped");
            continue;
        };
        let result = gs.on_shortcut(accel.as_str(), move |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                eprintln!("hotkey fired: {which}");
                dispatch(app, which);
            }
        });
        match result {
            Ok(()) => eprintln!("hotkey registered: {which} = {hk}"),
            Err(e) => {
                eprintln!(
                    "hotkey registration FAILED for {which} = {hk} ({e}; in use by another app?)"
                );
                if which == "panel" {
                    panel_hotkey_warning(app, &hk);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerators_for_the_defaults() {
        assert_eq!(
            to_accelerator("ctrl+alt+space").as_deref(),
            Some("Ctrl+Alt+Space")
        );
        assert_eq!(
            to_accelerator("ctrl+alt+v").as_deref(),
            Some("Ctrl+Alt+KeyV")
        );
        assert_eq!(
            to_accelerator("ctrl+alt+x").as_deref(),
            Some("Ctrl+Alt+KeyX")
        );
    }

    #[test]
    fn accelerator_key_forms() {
        assert_eq!(to_accelerator("shift+f12").as_deref(), Some("Shift+F12"));
        assert_eq!(to_accelerator("win+5").as_deref(), Some("Super+Digit5"));
        assert_eq!(
            to_accelerator("ctrl+pageup").as_deref(),
            Some("Ctrl+PageUp")
        );
        assert_eq!(to_accelerator("ctrl+`").as_deref(), Some("Ctrl+Backquote"));
        assert_eq!(to_accelerator("esc").as_deref(), Some("Escape"));
    }

    #[test]
    fn accelerator_rejects_unmappable() {
        assert_eq!(to_accelerator(""), None);
        assert_eq!(to_accelerator("ctrl+alt"), None, "no non-modifier key");
        assert_eq!(to_accelerator("ctrl+bogus"), None);
        assert_eq!(to_accelerator("ctrl+f25"), None);
    }
}
