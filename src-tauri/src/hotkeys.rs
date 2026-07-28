//! Global hotkeys, ported from the original's RegisterHotKey listener:
//! the configured combos dispatch to toggle/paste/panel/cancel, a repeat
//! press of dictate/paste/cancel still running is dropped (no pile-up /
//! double-toggle), and `register_all` re-registers live after a rebind
//! (`_request_rebind`).
//!
//! The PANEL action deliberately bypasses that drop-guard: dropping a repeat
//! press is right for the recording actions but would eat the second press
//! of a rapid open-close. Panel presses queue to a dedicated worker that
//! coalesces a burst to its net intent and applies the window op
//! immediately — nothing slow (mic enumeration, fs probes) ever runs before
//! the map/unmap; the fresh state snapshot follows asynchronously.
//!
//! Registration goes through tauri-plugin-global-shortcut (Win32 on Windows,
//! X11 on Linux — the Wayland portal story is task 4.3).

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::api;
use crate::flow::{self, lock, AppCtx};

/// which -> config key, in the original's registration order (paste joined
/// the scheme in the port).
const ACTIONS: [(&str, &str); 4] = [
    ("dictate", "dictation_hotkey"),
    ("paste", "paste_hotkey"),
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

// ---- push-to-talk on the recording keys ------------------------------------
//
// Both the dictate AND the paste key get the same tap/hold treatment, one
// HoldCore per key, differing only in what `dispatch(which)` does:
//
// - dictate: tap keeps the classic toggle (press starts a clipboard take,
//   the next tap stops it); holding past HOLD_MS makes the same press
//   push-to-talk — the release stops the take and transcribes to the
//   clipboard.
// - paste: tap keeps its toggle too (an idle press starts a take; a press
//   while recording is the FINISHING press and stops-copies-pastes right
//   there); holding past HOLD_MS records while held and the release stops
//   the take, transcribes, copies AND pastes at the cursor.
//
// A hold-release finishes the take by re-dispatching the key's own action,
// so the FINISHING interaction decides the destination either way: a held
// Space release is clipboard-only, a held V release pastes at the cursor —
// no matter which key started the take.
//
// Platform delivery of ShortcutState (global-hotkey 0.8.0 sources):
// - Windows registers with MOD_NOREPEAT (no repeat Pressed) and synthesizes
//   Released by polling GetAsyncKeyState every 50 ms — both states arrive.
// - X11 forwards real KeyPress/KeyRelease. Key AUTOREPEAT arrives as
//   release+press pairs a few ms apart, so a Released only counts as the
//   real release if no Pressed of the same shortcut follows within
//   REPEAT_MS, and a Pressed while the hold is live only counts as
//   autorepeat if a Released preceded it within REPEAT_MS.
// - A platform that never delivers Released degrades to the plain toggle:
//   with no Released ever seen, every press looks like (and is treated as)
//   a fresh press, and the release logic simply never runs.

/// Hold this long (press -> release) to make the press push-to-talk.
const HOLD_MS: u64 = 400;

/// A Released and a Pressed of the same shortcut within this window are an
/// X11 autorepeat pair, not a real release + re-press.
const REPEAT_MS: u64 = 50;

/// What a Pressed event should do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PressAction {
    /// X11 autorepeat while the hold is live — do nothing.
    Swallow,
    /// Real press: run the key's action.
    Dispatch,
}

/// What a debounced Released event should do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReleaseAction {
    /// Tap, voided autorepeat, non-starting press, or the take already
    /// ended — do nothing.
    Inert,
    /// Hold-release of the press that started the live take: run the key's
    /// action again to finish it.
    Finish,
}

/// Snapshot of a Released, resolved after the REPEAT_MS debounce.
struct PendingRelease {
    generation: u64,
    held: bool,
    starts_take: bool,
}

/// Tap-vs-hold decision core for one hotkey. Pure — timestamps and live
/// recording state are passed in — so the decision table is unit-testable.
#[derive(Default)]
struct HoldCore {
    /// When the live press started; None = key is (logically) up.
    press_at: Option<Instant>,
    /// Whether that press is the one that STARTED the recording — only such
    /// a press may stop the take on hold-release (a finishing press held
    /// long, or a press that failed to open the mic, must not re-toggle).
    starts_take: bool,
    /// Bumped on every Pressed; lets a pending release check detect that an
    /// autorepeat Pressed voided its paired Released.
    generation: u64,
    /// When the last Released arrived; a Pressed hot on its heels is X11
    /// autorepeat rather than a new press.
    release_at: Option<Instant>,
}

impl HoldCore {
    /// A Pressed arrived. `starts_take` is whether dispatching this key
    /// right now would START a recording (sampled by the caller just before
    /// the dispatch).
    fn press(&mut self, now: Instant, starts_take: bool) -> PressAction {
        self.generation += 1; // voids any pending release check
        if self.press_at.is_some()
            && self
                .release_at
                .is_some_and(|t| now.duration_since(t) <= Duration::from_millis(REPEAT_MS))
        {
            // Autorepeat Pressed while the hold is live — swallow it (the
            // bump above already voided its paired Released).
            return PressAction::Swallow;
        }
        // Either the key was up, or a press is "live" with no recent
        // Released — meaning the platform never delivered the previous
        // release (degrade-to-toggle). Both are a fresh press.
        self.press_at = Some(now);
        self.starts_take = starts_take;
        PressAction::Dispatch
    }

    /// A Released arrived: snapshot it for resolution after the debounce.
    /// None = stray release (e.g. the shortcut registered mid-hold).
    fn release(&mut self, now: Instant) -> Option<PendingRelease> {
        let press_at = self.press_at?;
        self.release_at = Some(now);
        Some(PendingRelease {
            generation: self.generation,
            held: now.duration_since(press_at) >= Duration::from_millis(HOLD_MS),
            starts_take: self.starts_take,
        })
    }

    /// REPEAT_MS after the Released: commit it unless an autorepeat Pressed
    /// superseded it, and decide whether it finishes the take. `recording`
    /// is the live state — a take that already ended (mic-open failure,
    /// another key finished it) must not be re-toggled by this release.
    fn resolve(&mut self, pending: &PendingRelease, recording: bool) -> ReleaseAction {
        if self.generation != pending.generation {
            return ReleaseAction::Inert; // autorepeat — the hold is still live
        }
        self.press_at = None; // the real release
        if pending.held && pending.starts_take && recording {
            ReleaseAction::Finish
        } else {
            // A quick tap keeps recording — today's toggle; the next tap
            // stops it.
            ReleaseAction::Inert
        }
    }
}

fn hold_core(which: &str) -> &'static Mutex<HoldCore> {
    static DICTATE: OnceLock<Mutex<HoldCore>> = OnceLock::new();
    static PASTE: OnceLock<Mutex<HoldCore>> = OnceLock::new();
    let cell = match which {
        "paste" => &PASTE,
        _ => &DICTATE,
    };
    cell.get_or_init(|| Mutex::new(HoldCore::default()))
}

/// Would dispatching `which` right now START a recording? Dictate also
/// starts one during a transcription (overlap is legal; session ids keep
/// takes apart), while paste ARMS the in-flight take instead of starting
/// anything — so a paste press during transcription is not take-starting
/// and its hold-release must stay inert.
fn press_starts_take(ctx: &AppCtx, which: &str) -> bool {
    match which {
        "paste" => !ctx.is_recording() && !ctx.is_transcribing(),
        _ => !ctx.is_recording(),
    }
}

/// Shared state handler for the dictate and paste keys: toggle on tap,
/// push-to-talk on hold.
fn hold_event(app: &AppHandle, which: &'static str, state: ShortcutState) {
    match state {
        ShortcutState::Pressed => {
            let action = {
                let ctx = app.state::<AppCtx>();
                let starts = press_starts_take(&ctx, which);
                lock(hold_core(which)).press(Instant::now(), starts)
            };
            if action == PressAction::Dispatch {
                eprintln!("hotkey fired: {which}");
                dispatch(app, which);
            }
        }
        ShortcutState::Released => {
            let Some(pending) = lock(hold_core(which)).release(Instant::now()) else {
                return;
            };
            // Don't act yet: X11 autorepeat delivers release+press pairs a
            // few ms apart. Wait REPEAT_MS; a Pressed arriving meanwhile
            // bumps the generation and this release turns out to be fake.
            let app = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(REPEAT_MS));
                let recording = app.state::<AppCtx>().is_recording();
                let action = lock(hold_core(which)).resolve(&pending, recording);
                if action == ReleaseAction::Finish {
                    eprintln!("{which} released after hold: push-to-talk finish");
                    dispatch(&app, which);
                }
            });
        }
    }
}

/// `_dispatch_hotkey`: run the action OFF the listener thread; drop a repeat
/// press of the same action while the prior one is still running. The tray
/// reuses this (the original's `_tray_dispatch` had the same shape).
///
/// The panel is the exception: its presses must NEVER be dropped (a rapid
/// open-close relies on every press landing), so they go to the panel
/// worker's queue instead of the inflight guard.
pub(crate) fn dispatch(app: &AppHandle, which: &'static str) {
    if which == "panel" {
        panel_request(app, PanelCmd::Toggle);
        return;
    }
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
            "paste" => flow::paste_take(&app),
            "cancel" => flow::cancel_record(&app),
            _ => {}
        }
        inflight()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(which);
    });
}

// ---- panel toggling --------------------------------------------------------
//
// The press path must be INSTANT: the map/unmap happens on the main thread
// with nothing slow in front of it. The full `get_state` snapshot (mic
// enumeration via cpal, log-dir probe, today's transcript read — easily a
// second) is computed AFTERWARDS on a worker and pushed via tiroApplyState;
// until it lands the panel shows its last-known content. Presses are
// serialized through a dedicated worker: each burst drains the queue and
// coalesces to its net intent against the panel's REAL visibility, so
// press-press ends hidden, press-press-press ends visible, and no press is
// ever dropped or double-applied.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PanelCmd {
    /// Hotkey / tray click: flip visibility.
    Toggle,
    /// Second launch: always end visible and in front, never hide.
    Summon,
}

/// The net effect of a burst of queued panel commands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct PanelPlan {
    /// Some(true) = show, Some(false) = hide, None = leave the map alone.
    map: Option<bool>,
    /// Raise + focus afterwards (only ever set when the plan ends visible).
    front: bool,
}

/// Fold a burst of commands into one plan, starting from the panel's actual
/// visibility. Every press flips the intended state in order, so e.g.
/// hidden + press-press coalesces to "stay hidden" (open-then-close, net
/// nothing) and press-press-press to a single show.
fn coalesce(visible: bool, cmds: &[PanelCmd]) -> PanelPlan {
    let mut want = visible;
    let mut front = false;
    for cmd in cmds {
        match cmd {
            PanelCmd::Toggle => {
                want = !want;
                front = want;
            }
            PanelCmd::Summon => {
                want = true;
                front = true;
            }
        }
    }
    PanelPlan {
        map: (want != visible).then_some(want),
        front: front && want,
    }
}

/// What the main-thread closure actually did, for the follow-ups.
struct Applied {
    shown: bool,
    front: bool,
}

/// Main-thread only: read the real visibility, coalesce the burst against
/// it, and apply the map change. Nothing slow may run here — position math
/// and set_position/show/hide only.
fn apply_cmds(app: &AppHandle, cmds: &[PanelCmd]) -> Option<Applied> {
    let w = app.get_webview_window("panel")?;
    let visible = w.is_visible().unwrap_or(false);
    let plan = coalesce(visible, cmds);
    let mut shown = false;
    match plan.map {
        Some(true) => {
            // Place BEFORE mapping (remembered spot, else centered) so the
            // panel appears where it belongs instead of jumping there; the
            // reposition burst afterwards wins any race with the WM.
            crate::placement::position(app, "panel");
            let _ = w.show();
            shown = true;
        }
        Some(false) => {
            // Capture the position while the window is still mapped, then
            // unmap. The config write happens on a worker.
            crate::placement::remember_panel_now(app);
            let _ = w.hide();
        }
        None => {}
    }
    Some(Applied {
        shown,
        front: plan.front,
    })
}

fn panel_loop(app: AppHandle, rx: Receiver<PanelCmd>) {
    while let Ok(first) = rx.recv() {
        let mut cmds = vec![first];
        while let Ok(more) = rx.try_recv() {
            cmds.push(more);
        }
        let (ack_tx, ack_rx) = channel::<Option<Applied>>();
        let a = app.clone();
        if app
            .run_on_main_thread(move || {
                let applied = apply_cmds(&a, &cmds);
                let _ = ack_tx.send(applied);
            })
            .is_err()
        {
            continue;
        }
        // The window op is instant; waiting for it only serializes bursts —
        // presses arriving meanwhile queue up and coalesce on the next pass
        // against the visibility this op just established. The timeout keeps
        // a wedged main thread from deadlocking the panel forever.
        let applied = match ack_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Some(applied)) => applied,
            _ => continue,
        };
        if applied.shown {
            crate::placement::reposition_burst(&app, "panel");
            refresh_panel_state(&app);
        }
        if applied.front {
            crate::placement::summon_front(&app);
        }
    }
}

fn panel_sender(app: &AppHandle) -> &'static Mutex<Sender<PanelCmd>> {
    static TX: OnceLock<Mutex<Sender<PanelCmd>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = channel::<PanelCmd>();
        let app = app.clone();
        std::thread::spawn(move || panel_loop(app, rx));
        Mutex::new(tx)
    })
}

/// Queue a panel command. Instant (a channel send) — safe from the shortcut
/// listener, the tray handler, and the single-instance callback alike.
pub(crate) fn panel_request(app: &AppHandle, cmd: PanelCmd) {
    let _ = lock(panel_sender(app)).send(cmd);
}

/// Compute a fresh `get_state` snapshot OFF the press path and push it to
/// the just-shown panel (which re-renders on `tiroApplyState`). Generation-
/// stamped: only the latest requested snapshot may apply, so a slow one
/// computed for an older show can never clobber the panel after a newer
/// hide→show cycle. The push mutex makes check-and-eval atomic — a stale
/// eval can never be issued after a fresh one.
fn refresh_panel_state(app: &AppHandle) {
    static GEN: AtomicU64 = AtomicU64::new(0);
    static PUSH: Mutex<()> = Mutex::new(());
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        let state = api::get_state(&app); // slow: mic enumeration, fs probes
        let _guard = PUSH.lock().unwrap_or_else(|e| e.into_inner());
        if GEN.load(Ordering::SeqCst) == gen {
            flow::push_panel(&app, "tiroApplyState", state);
        }
    });
}

/// `toggle_panel`: flip the panel's visibility NOW — the window op runs with
/// nothing slow in front of it; the fresh state snapshot follows.
pub fn toggle_panel(app: &AppHandle) {
    panel_request(app, PanelCmd::Toggle);
}

/// Summon (never hide): a second app launch must always end with the panel
/// visible and in front.
pub fn summon_panel(app: &AppHandle) {
    panel_request(app, PanelCmd::Summon);
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
    // Piggyback on the startup registration pass: seed the panel's
    // remembered position from config and start move-tracking (idempotent —
    // rebind passes are no-ops).
    crate::placement::init_panel_tracking(app);
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
            if which == "dictate" || which == "paste" {
                // Tap = toggle, hold = push-to-talk; needs both states.
                hold_event(app, which, event.state);
            } else if event.state == ShortcutState::Pressed {
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
    // Global hotkeys are X11 grabs on Linux; in a Wayland session they can
    // only fire while an XWayland window has focus (or not at all with no
    // X server). Point at the documented DE-shortcut fallback.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        eprintln!(
            "Wayland session detected: global hotkeys use X11 grabs and may \
             not fire while native Wayland apps have focus. Bind DE-level \
             shortcuts to `tiro --toggle` / `tiro --panel` / `tiro --cancel` \
             instead (see BUILDING.md)."
        );
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

    use PanelCmd::{Summon, Toggle};

    fn plan(visible: bool, cmds: &[PanelCmd]) -> PanelPlan {
        coalesce(visible, cmds)
    }

    #[test]
    fn single_press_toggles_each_way() {
        assert_eq!(
            plan(false, &[Toggle]),
            PanelPlan {
                map: Some(true),
                front: true
            },
            "hidden + press = show and bring to front"
        );
        assert_eq!(
            plan(true, &[Toggle]),
            PanelPlan {
                map: Some(false),
                front: false
            },
            "visible + press = hide, no raise"
        );
    }

    #[test]
    fn rapid_presses_coalesce_to_net_intent() {
        // press-press from hidden: open-then-close, net nothing, ends hidden
        assert_eq!(
            plan(false, &[Toggle, Toggle]),
            PanelPlan {
                map: None,
                front: false
            }
        );
        // press-press-press from hidden ends visible (one show, one raise)
        assert_eq!(
            plan(false, &[Toggle, Toggle, Toggle]),
            PanelPlan {
                map: Some(true),
                front: true
            }
        );
        // press-press from visible: close-then-open — no map change, but the
        // panel ends (stays) visible and is raised
        assert_eq!(
            plan(true, &[Toggle, Toggle]),
            PanelPlan {
                map: None,
                front: true
            }
        );
        // four presses from visible: even parity, ends (stays) visible and
        // raised because the last press turned it back on
        assert_eq!(
            plan(true, &[Toggle, Toggle, Toggle, Toggle]),
            PanelPlan {
                map: None,
                front: true
            }
        );
        // odd parity from visible ends hidden
        assert_eq!(
            plan(true, &[Toggle, Toggle, Toggle]),
            PanelPlan {
                map: Some(false),
                front: false
            }
        );
    }

    #[test]
    fn summon_always_ends_visible_and_in_front() {
        assert_eq!(
            plan(false, &[Summon]),
            PanelPlan {
                map: Some(true),
                front: true
            }
        );
        assert_eq!(
            plan(true, &[Summon]),
            PanelPlan {
                map: None,
                front: true
            },
            "already visible: raise only, never re-map"
        );
        // a toggle after a summon still wins — strict press order
        assert_eq!(
            plan(true, &[Summon, Toggle]),
            PanelPlan {
                map: Some(false),
                front: false
            }
        );
        // and a summon after a hide-toggle rescues visibility
        assert_eq!(
            plan(true, &[Toggle, Summon]),
            PanelPlan {
                map: None,
                front: true
            }
        );
    }

    #[test]
    fn empty_burst_is_a_noop() {
        assert_eq!(
            plan(true, &[]),
            PanelPlan {
                map: None,
                front: false
            }
        );
        assert_eq!(
            plan(false, &[]),
            PanelPlan {
                map: None,
                front: false
            }
        );
    }

    // ---- tap/hold state machine (shared by the dictate and paste keys) ----
    //
    // The same HoldCore drives both keys; only the dispatched action
    // differs (dictate = clipboard toggle/stop, paste = stop-and-paste), so
    // one set of decision-table tests covers both.

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn tap_dispatches_on_press_and_its_release_is_inert() {
        let mut c = HoldCore::default();
        let t0 = Instant::now();
        assert_eq!(c.press(t0, true), PressAction::Dispatch); // starts the take
        let p = c.release(t0 + ms(120)).expect("live press");
        assert!(!p.held);
        // recording is live (the tap started it) but a tap never PTT-stops:
        // today's toggle — the NEXT tap stops it
        assert_eq!(c.resolve(&p, true), ReleaseAction::Inert);
        assert_eq!(c.press(t0 + ms(2000), false), PressAction::Dispatch);
    }

    #[test]
    fn hold_release_finishes_the_take_it_started() {
        let mut c = HoldCore::default();
        let t0 = Instant::now();
        assert_eq!(c.press(t0, true), PressAction::Dispatch);
        let p = c.release(t0 + ms(HOLD_MS)).expect("live press");
        assert!(p.held);
        // dictate: clipboard stop; paste: stop-copy-paste — same decision
        assert_eq!(c.resolve(&p, true), ReleaseAction::Finish);
        // and the core is ready for the next take
        assert_eq!(c.press(t0 + ms(HOLD_MS + 500), true), PressAction::Dispatch);
    }

    #[test]
    fn finishing_press_held_long_has_an_inert_release() {
        // Edge 1: V pressed while a take (started by Space or an earlier V
        // tap) is live stops-and-pastes ON PRESS (starts_take = false);
        // holding that press and releasing must not fire anything more.
        let mut c = HoldCore::default();
        let t0 = Instant::now();
        assert_eq!(c.press(t0, false), PressAction::Dispatch); // the finishing press
        let p = c.release(t0 + ms(700)).expect("live press");
        assert!(p.held);
        // by release time the press's dispatch has stopped the take
        assert_eq!(c.resolve(&p, false), ReleaseAction::Inert);
        // ...and even if ANOTHER key started a new take meanwhile, a
        // non-starting press's release still must not touch it
        let mut c2 = HoldCore::default();
        assert_eq!(c2.press(t0, false), PressAction::Dispatch);
        let p2 = c2.release(t0 + ms(700)).expect("live press");
        assert_eq!(c2.resolve(&p2, true), ReleaseAction::Inert);
    }

    #[test]
    fn tap_then_later_hold_finishes_on_the_second_press() {
        // Edge 2 as a sequence: tap V starts the take; a later hold of V is
        // a finishing press (recording is live, so starts_take = false) —
        // stop-and-paste fires on the press, the release stays inert.
        let mut c = HoldCore::default();
        let t0 = Instant::now();
        assert_eq!(c.press(t0, true), PressAction::Dispatch); // tap: starts take
        let p = c.release(t0 + ms(100)).expect("live press");
        assert_eq!(c.resolve(&p, true), ReleaseAction::Inert); // still recording
        let t1 = t0 + ms(3000);
        assert_eq!(c.press(t1, false), PressAction::Dispatch); // finishing press
        let p = c.release(t1 + ms(900)).expect("live press");
        assert!(p.held);
        assert_eq!(c.resolve(&p, false), ReleaseAction::Inert);
    }

    #[test]
    fn hold_through_mic_failure_release_is_inert() {
        // Edge 3: the idle press claimed starts_take, but the mic never
        // opened, so recording is false at release time — nothing to stop,
        // and the release must not toggle a fresh recording on.
        let mut c = HoldCore::default();
        let t0 = Instant::now();
        assert_eq!(c.press(t0, true), PressAction::Dispatch);
        let p = c.release(t0 + ms(600)).expect("live press");
        assert!(p.held);
        assert_eq!(c.resolve(&p, false), ReleaseAction::Inert);
    }

    #[test]
    fn autorepeat_never_stops_the_take_early() {
        // Edge 4: X11 autorepeat while held arrives as release+press pairs
        // a few ms apart. Every repeat Released is voided by its paired
        // Pressed's generation bump; every repeat Pressed is swallowed; the
        // eventual real release still finishes the take.
        let mut c = HoldCore::default();
        let t0 = Instant::now();
        assert_eq!(c.press(t0, true), PressAction::Dispatch);
        let mut t = t0 + ms(500); // autorepeat kicks in
        let mut voided = Vec::new();
        for _ in 0..5 {
            let p = c.release(t).expect("hold is live");
            assert_eq!(c.press(t + ms(3), false), PressAction::Swallow);
            voided.push(p);
            t += ms(35);
        }
        for p in &voided {
            assert_eq!(c.resolve(p, true), ReleaseAction::Inert, "voided repeat");
        }
        // the real release (no paired Pressed follows) finishes the take
        let p = c.release(t).expect("hold still live");
        assert!(p.held);
        assert_eq!(c.resolve(&p, true), ReleaseAction::Finish);
    }

    #[test]
    fn missing_released_degrades_to_toggle() {
        // A platform that never delivers Released: no Released ever
        // precedes a press, so every press is a fresh press and the key
        // behaves as the plain toggle.
        let mut c = HoldCore::default();
        let t0 = Instant::now();
        assert_eq!(c.press(t0, true), PressAction::Dispatch); // starts the take
        assert_eq!(c.press(t0 + ms(5000), false), PressAction::Dispatch); // stops it
        assert_eq!(c.press(t0 + ms(9000), true), PressAction::Dispatch); // starts again
    }

    #[test]
    fn stray_release_is_ignored() {
        // e.g. the shortcut registered while the key was already down
        let mut c = HoldCore::default();
        assert!(c.release(Instant::now()).is_none());
    }
}
