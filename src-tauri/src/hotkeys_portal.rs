//! Wayland global hotkeys via the XDG GlobalShortcuts portal.
//!
//! The X11 passive grabs the shortcut plugin uses only see keys while an
//! XWayland surface holds keyboard focus — with a native Wayland window
//! focused the compositor delivers keys straight to that surface and the
//! grab never fires. That is the owner-visible "hotkey works only
//! sometimes" bug. The GlobalShortcuts portal is compositor-level D-Bus, so
//! it fires no matter which app has focus (and regardless of Tiro's own
//! GDK_BACKEND=x11 window backend).
//!
//! Strategy: `hotkeys::register_all` always installs the X11 grabs first
//! (instant and synchronous — the app is never hotkey-less), then calls
//! [`spawn_register`], which binds the combos through the portal on a
//! worker thread and, on success, drops the grabs so a press with an
//! XWayland window focused cannot fire twice. Portal absent or denied
//! leaves the grabs in place with a log. The portal spec has no unbind, so
//! a rebind closes the old session and binds a fresh one (close + create +
//! bind); a generation stamp keeps a slow stale pass from clobbering a
//! newer one.
//!
//! Activated/Deactivated signals carry the application-chosen shortcut id
//! (the `which` names from `hotkeys::ACTIONS`) and feed the same
//! `hotkeys::hotkey_event` the grab path uses, so tap-vs-hold push-to-talk
//! and the panel's never-drop queue behave identically on both backends.
//! Unlike the grab path, the portal delivers exactly one Activated per
//! physical press and one Deactivated per physical release — no X11
//! autorepeat pairs, no synthetic releases on focus changes — which is what
//! makes hold-to-talk immune to the premature-stop bug here.

use std::pin::{pin, Pin};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::sync::OnceLock;

use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::desktop::{CreateSessionOptions, Session};
use ashpd::zbus::export::futures_core::Stream;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

use crate::hotkeys;

/// Is this a Wayland session? Checked at the session level, NOT via the GDK
/// backend — Tiro self-forces GDK_BACKEND=x11 for its windows, but the
/// portal is D-Bus and works regardless of the window backend.
pub fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))
}

/// The live portal session (None until the first successful bind). Held
/// across rebinds so the previous session can be closed; the lock also
/// serializes whole registration passes.
static SESSION: Mutex<Option<Session<GlobalShortcuts>>> = Mutex::new(None);

/// Registration-pass generation: a pass that discovers a newer one exists
/// must neither close the newer session nor drop the newer grabs.
static GEN: AtomicU64 = AtomicU64::new(0);

/// Bind `bindings` (which, config combo string) through the portal on a
/// worker thread. On success the X11 grabs are dropped; on failure they
/// stay and the failure is logged. Returns immediately.
pub fn spawn_register(app: &AppHandle, bindings: Vec<(&'static str, String)>) {
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || register(&app, gen, &bindings));
}

fn register(app: &AppHandle, gen: u64, bindings: &[(&'static str, String)]) {
    let mut slot = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if GEN.load(Ordering::SeqCst) != gen {
        return; // a newer pass is queued behind us; let it do the work
    }
    match tauri::async_runtime::block_on(register_async(&mut slot, bindings)) {
        Ok(bound) => {
            if GEN.load(Ordering::SeqCst) != gen {
                // Superseded mid-bind: the newer pass (waiting on the lock)
                // will close this session and bind its own.
                return;
            }
            // The portal owns the hotkeys now: drop the X11 grabs so a
            // press with an XWayland window focused cannot fire twice.
            if let Err(e) = app.global_shortcut().unregister_all() {
                eprintln!("portal shortcuts: dropping the X11 grabs failed: {e}");
            }
            ensure_listeners(app);
            for line in &bound {
                eprintln!("portal shortcut bound: {line}");
            }
        }
        Err(e) => {
            eprintln!(
                "GlobalShortcuts portal unavailable ({e}); keeping the X11-grab \
                 hotkeys. These only fire while an XWayland window has focus — \
                 bind DE-level shortcuts to `tiro --toggle` / `tiro --panel` / \
                 `tiro --cancel` instead (see BUILDING.md)."
            );
        }
    }
}

/// One full portal pass: close the previous session (rebind path), create a
/// fresh one, bind. On any failure the fresh session is closed too — a
/// half-bound session must not linger on the bus.
async fn register_async(
    slot: &mut Option<Session<GlobalShortcuts>>,
    bindings: &[(&'static str, String)],
) -> Result<Vec<String>, String> {
    let proxy = GlobalShortcuts::new()
        .await
        .map_err(|e| format!("no portal proxy: {e}"))?;
    // The portal has no unbind: rebinding means a fresh session.
    if let Some(old) = slot.take() {
        if let Err(e) = old.close().await {
            eprintln!("portal shortcuts: closing the previous session failed: {e}");
        }
    }
    let session = proxy
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| format!("create_session: {e}"))?;
    match bind(&proxy, &session, bindings).await {
        Ok(bound) => {
            *slot = Some(session);
            Ok(bound)
        }
        Err(e) => {
            if let Err(e2) = session.close().await {
                eprintln!("portal shortcuts: closing the failed session failed: {e2}");
            }
            Err(e)
        }
    }
}

async fn bind(
    proxy: &GlobalShortcuts,
    session: &Session<GlobalShortcuts>,
    bindings: &[(&'static str, String)],
) -> Result<Vec<String>, String> {
    let shortcuts: Vec<NewShortcut> = bindings
        .iter()
        .map(|(which, hk)| {
            let s = NewShortcut::new(*which, description(which));
            match hotkeys::to_portal_trigger(hk) {
                // Preferred trigger = the user's configured combo, so a
                // compositor that honors preferences (KDE) binds without a
                // dialog on first run.
                Some(trigger) => s.preferred_trigger(trigger.as_str()),
                None => s,
            }
        })
        .collect();
    let response = proxy
        .bind_shortcuts(session, &shortcuts, None, BindShortcutsOptions::default())
        .await
        .map_err(|e| format!("bind_shortcuts: {e}"))?
        .response()
        .map_err(|e| format!("bind_shortcuts response: {e}"))?;
    if response.shortcuts().is_empty() {
        return Err("the portal bound no shortcuts".into());
    }
    Ok(response
        .shortcuts()
        .iter()
        .map(|s| format!("{} ({})", s.id(), s.trigger_description()))
        .collect())
}

/// KDE surfaces these in System Settings under the app entry — expected
/// portal behavior, and the reason the descriptions are user-facing prose.
fn description(which: &str) -> &'static str {
    match which {
        "dictate" => "Start or stop dictation (hold for push-to-talk)",
        "paste" => "Dictate and paste at the cursor (hold to talk)",
        "panel" => "Show or hide the Tiro panel",
        "cancel" => "Cancel the current recording",
        _ => "Tiro shortcut",
    }
}

/// Portal shortcut id -> the static `which` name `hotkeys::hotkey_event`
/// expects. Ids are application-chosen at bind time (we use the `which`
/// names themselves), so an unknown id means a foreign/stale signal: drop
/// it.
fn action_for(id: &str) -> Option<&'static str> {
    hotkeys::ACTIONS
        .iter()
        .map(|(which, _)| *which)
        .find(|which| *which == id)
}

/// Start the two signal-listener tasks exactly once per process. They
/// outlive rebinds: shortcut ids are stable across sessions, so events keep
/// routing correctly, and after a fallback to grabs no signals arrive at
/// all.
fn ensure_listeners(app: &AppHandle) {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let a = app.clone();
        tauri::async_runtime::spawn(async move {
            match listen_activated(a).await {
                Ok(()) => eprintln!("portal shortcuts: Activated stream ended"),
                Err(e) => eprintln!("portal shortcuts: Activated listener failed: {e}"),
            }
        });
        let a = app.clone();
        tauri::async_runtime::spawn(async move {
            match listen_deactivated(a).await {
                Ok(()) => eprintln!("portal shortcuts: Deactivated stream ended"),
                Err(e) => eprintln!("portal shortcuts: Deactivated listener failed: {e}"),
            }
        });
    });
}

/// Drive a pinned stream without a futures-util dependency.
async fn next_item<S: Stream>(mut stream: Pin<&mut S>) -> Option<S::Item> {
    std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await
}

async fn listen_activated(app: AppHandle) -> Result<(), ashpd::Error> {
    let proxy = GlobalShortcuts::new().await?;
    let stream = proxy.receive_activated().await?;
    let mut stream = pin!(stream);
    while let Some(ev) = next_item(stream.as_mut()).await {
        if let Some(which) = action_for(ev.shortcut_id()) {
            hotkeys::hotkey_event(&app, which, true);
        }
    }
    Ok(())
}

async fn listen_deactivated(app: AppHandle) -> Result<(), ashpd::Error> {
    let proxy = GlobalShortcuts::new().await?;
    let stream = proxy.receive_deactivated().await?;
    let mut stream = pin!(stream);
    while let Some(ev) = next_item(stream.as_mut()).await {
        if let Some(which) = action_for(ev.shortcut_id()) {
            hotkeys::hotkey_event(&app, which, false);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_id_resolves_to_itself() {
        // The portal echoes back the ids we bind; each must route to its
        // own action, exactly as the grab path dispatches it.
        for (which, _) in hotkeys::ACTIONS {
            assert_eq!(action_for(which), Some(which));
        }
    }

    #[test]
    fn foreign_ids_are_dropped() {
        assert_eq!(action_for(""), None);
        assert_eq!(action_for("Dictate"), None, "ids are case-sensitive");
        assert_eq!(action_for("quit"), None);
    }

    #[test]
    fn every_action_has_a_real_description() {
        for (which, _) in hotkeys::ACTIONS {
            let d = description(which);
            assert!(!d.is_empty() && d != "Tiro shortcut", "{which}: {d}");
        }
    }
}
