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
//! XWayland window focused cannot fire twice. The bind only counts as
//! success if EVERY requested action came back bound with a real trigger
//! (KDE returns an id with an empty trigger when the preferred combo
//! conflicts with an existing global shortcut) — anything less keeps the
//! grabs, all-or-nothing. Portal absent or denied likewise leaves the
//! grabs in place with a log.
//!
//! The portal spec has no unbind, so a rebind closes the old session and
//! binds a fresh one (close + create + bind); a generation stamp keeps a
//! slow stale pass from clobbering a newer one.
//!
//! Death watch: xdg-desktop-portal restarts are a real periodic event. If a
//! signal stream ends or the portal closes our session, [`portal_lost`]
//! re-registers the X11 grabs immediately (the app must never be
//! hotkey-less) and retries the portal upgrade a few times with backoff;
//! after that the grabs stay and the next manual rebind retries. Listener
//! tasks sit behind restartable latches, so every successful upgrade pass
//! re-arms any that died.
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// The live portal session plus its Closed-signal watcher task (None until
/// the first successful bind). Held across rebinds so the previous session
/// can be closed and its watcher aborted; the lock also serializes whole
/// registration passes.
type SessionSlot = Option<(
    Arc<Session<GlobalShortcuts>>,
    tauri::async_runtime::JoinHandle<()>,
)>;
static SESSION: Mutex<SessionSlot> = Mutex::new(None);

/// Registration-pass generation: a pass that discovers a newer one exists
/// must neither close the newer session nor drop the newer grabs, and a
/// stale session's death watcher must not trigger recovery.
static GEN: AtomicU64 = AtomicU64::new(0);

/// Single-flight guard for [`portal_lost`] recovery (both signal streams
/// tend to die together — one recovery pass serves them all).
static RECOVERING: AtomicBool = AtomicBool::new(false);

/// Portal retry backoff after a loss, ~30s total; after the last attempt
/// the X11 grabs stay and the next manual rebind retries the portal.
const RETRY_DELAYS_S: [u64; 4] = [2, 5, 10, 15];

/// Bind `bindings` (which, config combo string) through the portal on a
/// worker thread. On success the X11 grabs are dropped; on failure they
/// stay and the failure is logged. Returns immediately.
pub fn spawn_register(app: &AppHandle, bindings: Vec<(&'static str, String)>) {
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        register(&app, gen, &bindings);
    });
}

/// One full portal pass; true = the portal now owns the hotkeys.
fn register(app: &AppHandle, gen: u64, bindings: &[(&'static str, String)]) -> bool {
    let mut slot = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if GEN.load(Ordering::SeqCst) != gen {
        return false; // a newer pass is queued behind us; let it do the work
    }
    match tauri::async_runtime::block_on(register_async(app, gen, &mut slot, bindings)) {
        Ok(bound) => {
            if GEN.load(Ordering::SeqCst) != gen {
                // Superseded mid-bind: the newer pass (waiting on the lock)
                // will close this session and bind its own.
                return false;
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
            true
        }
        Err(e) => {
            eprintln!(
                "GlobalShortcuts portal bind failed ({e}); keeping the X11-grab \
                 hotkeys. These only fire while an XWayland window has focus — \
                 bind DE-level shortcuts to `tiro --toggle` / `tiro --panel` / \
                 `tiro --cancel` instead (see BUILDING.md)."
            );
            false
        }
    }
}

/// Close the previous session (rebind path), create a fresh one, bind, and
/// start the session's death watcher. On any failure the fresh session is
/// closed too — a half-bound session must not linger on the bus.
async fn register_async(
    app: &AppHandle,
    gen: u64,
    slot: &mut SessionSlot,
    bindings: &[(&'static str, String)],
) -> Result<Vec<String>, String> {
    let proxy = GlobalShortcuts::new()
        .await
        .map_err(|e| format!("no portal proxy: {e}"))?;
    // The portal has no unbind: rebinding means a fresh session. Abort the
    // old session's watcher first — this close is OURS, not a portal death.
    if let Some((old, watcher)) = slot.take() {
        watcher.abort();
        if let Err(e) = old.close().await {
            eprintln!("portal shortcuts: closing the previous session failed: {e}");
        }
    }
    let session = proxy
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| format!("create_session: {e}"))?;
    let session = Arc::new(session);
    match bind(&proxy, session.as_ref(), bindings).await {
        Ok(bound) => {
            let watcher = watch_session_closed(app, gen, Arc::clone(&session));
            *slot = Some((session, watcher));
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
    let bound: Vec<(String, String)> = response
        .shortcuts()
        .iter()
        .map(|s| (s.id().to_owned(), s.trigger_description().to_owned()))
        .collect();
    let requested: Vec<&'static str> = bindings.iter().map(|(which, _)| *which).collect();
    validate_bound(&requested, &bound)
}

/// All-or-nothing bind check: every requested action id must come back
/// bound WITH a trigger. The XDG spec lets the compositor return a subset,
/// and KDE returns an id with an EMPTY trigger when the preferred combo
/// conflicts with an existing global shortcut (it shows up unassigned in
/// System Settings). Counting either as success would drop the X11 grabs
/// and leave that action — possibly the panel, the only way into an
/// otherwise-invisible app — with no hotkey anywhere. On success returns
/// one log line per requested action.
fn validate_bound(
    requested: &[&'static str],
    bound: &[(String, String)],
) -> Result<Vec<String>, String> {
    let mut lines = Vec::with_capacity(requested.len());
    let mut problems: Vec<String> = Vec::new();
    for want in requested {
        match bound.iter().find(|(id, _)| id == want) {
            Some((id, trigger)) if !trigger.trim().is_empty() => {
                lines.push(format!("{id} ({trigger})"));
            }
            Some(_) => problems.push(format!(
                "'{want}' bound without a trigger (combo taken by another global shortcut?)"
            )),
            None => problems.push(format!("'{want}' missing from the bind response")),
        }
    }
    if problems.is_empty() {
        Ok(lines)
    } else {
        Err(problems.join("; "))
    }
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

// ---- death watch and recovery ----------------------------------------------

/// One-runner-at-a-time latch for a listener task. Unlike a OnceLock, it
/// re-arms after the runner exits, so a listener whose signal stream ended
/// (portal restart) is replaced on the next upgrade pass instead of being
/// dead forever.
struct Latch(AtomicBool);

impl Latch {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// True = the caller owns the (re)start; false = a runner is live.
    fn try_arm(&self) -> bool {
        !self.0.swap(true, Ordering::SeqCst)
    }

    /// The runner exited; the next `try_arm` starts a replacement.
    fn disarm(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

static ACTIVATED: Latch = Latch::new();
static DEACTIVATED: Latch = Latch::new();

/// (Re)start any signal listener that is not currently running. Called on
/// every successful upgrade pass, so a listener that died between passes is
/// re-armed rather than latched off forever.
fn ensure_listeners(app: &AppHandle) {
    if ACTIVATED.try_arm() {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            match listen_activated(app.clone()).await {
                Ok(()) => eprintln!("portal shortcuts: Activated stream ended"),
                Err(e) => eprintln!("portal shortcuts: Activated listener failed: {e}"),
            }
            ACTIVATED.disarm();
            portal_lost(&app, "the Activated signal stream ended");
        });
    }
    if DEACTIVATED.try_arm() {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            match listen_deactivated(app.clone()).await {
                Ok(()) => eprintln!("portal shortcuts: Deactivated stream ended"),
                Err(e) => eprintln!("portal shortcuts: Deactivated listener failed: {e}"),
            }
            DEACTIVATED.disarm();
            portal_lost(&app, "the Deactivated signal stream ended");
        });
    }
}

/// Watch for the portal closing our session out from under us (portal
/// shutdown/restart). Gen-stamped AND aborted on rebind: our OWN close of
/// the old session must never trigger recovery.
fn watch_session_closed(
    app: &AppHandle,
    gen: u64,
    session: Arc<Session<GlobalShortcuts>>,
) -> tauri::async_runtime::JoinHandle<()> {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let stream = match session.receive_closed().await {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("portal shortcuts: no session-Closed watcher: {e}");
                return;
            }
        };
        let mut stream = pin!(stream);
        // Some(_) = the portal actively closed the session. None (stream
        // end) = connection-level death, which the signal listeners also
        // see and report — don't double-trigger recovery from here.
        if next_item(stream.as_mut()).await.is_some() && GEN.load(Ordering::SeqCst) == gen {
            portal_lost(&app, "the portal closed the shortcut session");
        }
    })
}

/// The portal path just died (stream end or session closed). Re-register
/// the X11 grabs NOW — the app must never be hotkey-less — then retry the
/// portal upgrade a few times with backoff. Single-flight; in-flight stale
/// passes and watchers are invalidated by the generation bump.
fn portal_lost(app: &AppHandle, why: &str) {
    if RECOVERING.swap(true, Ordering::SeqCst) {
        return; // a recovery pass is already running
    }
    eprintln!("portal shortcuts lost ({why}): re-registering the X11 grabs, then retrying");
    GEN.fetch_add(1, Ordering::SeqCst); // invalidate in-flight passes/watchers
    let app = app.clone();
    std::thread::spawn(move || {
        hotkeys::reregister_grabs(&app);
        for delay in RETRY_DELAYS_S {
            std::thread::sleep(Duration::from_secs(delay));
            let bindings = hotkeys::portal_bindings(&app);
            if bindings.is_empty() {
                break;
            }
            let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
            if register(&app, gen, &bindings) {
                RECOVERING.store(false, Ordering::SeqCst);
                return;
            }
        }
        eprintln!(
            "portal shortcuts: retries exhausted; the X11 grabs stay (a hotkey \
             rebind in the panel retries the portal)"
        );
        RECOVERING.store(false, Ordering::SeqCst);
    });
}

// ---- signal listeners ------------------------------------------------------

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

    fn pair(id: &str, trigger: &str) -> (String, String) {
        (id.to_owned(), trigger.to_owned())
    }

    #[test]
    fn full_bind_response_passes() {
        let requested = ["dictate", "panel"];
        let bound = vec![
            pair("dictate", "CTRL+ALT+SPACE"),
            pair("panel", "CTRL+ALT+C"),
        ];
        let lines = validate_bound(&requested, &bound).expect("full coverage");
        assert_eq!(
            lines,
            vec!["dictate (CTRL+ALT+SPACE)", "panel (CTRL+ALT+C)"],
            "one log line per requested action, requested order"
        );
    }

    #[test]
    fn missing_action_fails_the_whole_bind() {
        // The XDG spec lets the compositor return a subset; a partial bind
        // must NOT drop the X11 grabs (the missing action — possibly the
        // panel — would have no hotkey anywhere).
        let requested = ["dictate", "paste", "panel", "cancel"];
        let bound = vec![
            pair("dictate", "CTRL+ALT+SPACE"),
            pair("paste", "CTRL+ALT+V"),
            pair("cancel", "CTRL+ALT+X"),
        ];
        let err = validate_bound(&requested, &bound).expect_err("panel missing");
        assert!(err.contains("'panel'"), "names the missing action: {err}");
        assert!(err.contains("missing"), "{err}");
    }

    #[test]
    fn empty_trigger_fails_the_whole_bind() {
        // KDE binds the id with NO trigger when the preferred combo
        // conflicts with an existing global shortcut — the shortcut exists
        // but can never fire, so it must count as a failure.
        let requested = ["dictate", "panel"];
        let bound = vec![pair("dictate", "CTRL+ALT+SPACE"), pair("panel", "")];
        let err = validate_bound(&requested, &bound).expect_err("empty trigger");
        assert!(err.contains("'panel'"), "names the unbound action: {err}");
        assert!(err.contains("without a trigger"), "{err}");
        // whitespace-only is just as unbound
        let bound = vec![pair("dictate", "CTRL+ALT+SPACE"), pair("panel", "  ")];
        assert!(validate_bound(&requested, &bound).is_err());
    }

    #[test]
    fn multiple_problems_are_all_reported() {
        let requested = ["dictate", "paste", "panel"];
        let bound = vec![pair("paste", "")];
        let err = validate_bound(&requested, &bound).expect_err("two missing, one empty");
        for name in ["'dictate'", "'paste'", "'panel'"] {
            assert!(err.contains(name), "{err} should name {name}");
        }
    }

    #[test]
    fn foreign_extra_ids_in_the_response_are_ignored() {
        let requested = ["dictate"];
        let bound = vec![pair("dictate", "CTRL+ALT+SPACE"), pair("mystery", "META+M")];
        let lines = validate_bound(&requested, &bound).expect("extras are harmless");
        assert_eq!(lines, vec!["dictate (CTRL+ALT+SPACE)"]);
    }

    #[test]
    fn latch_rearms_after_disarm() {
        // The D2 shape: a OnceLock latch left dead listeners dead forever
        // after their signal stream ended. The latch must hand ownership to
        // exactly one runner at a time but re-arm once that runner exits.
        let latch = Latch::new();
        assert!(latch.try_arm(), "first arm starts a runner");
        assert!(!latch.try_arm(), "no second runner while one is live");
        assert!(!latch.try_arm());
        latch.disarm(); // runner exited (stream ended)
        assert!(latch.try_arm(), "a replacement may start");
        assert!(!latch.try_arm());
    }
}
