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
//! X11 on Linux); on Wayland sessions the GlobalShortcuts portal then takes
//! over delivery (see `hotkeys_portal`), feeding the same `hotkey_event`
//! entry point.

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
/// the scheme in the port). The `which` names double as the stable shortcut
/// ids the Wayland portal binds.
pub(crate) const ACTIONS: [(&str, &str); 4] = [
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

/// Map one lowercase config hotkey part to the xkb keysym name that XDG
/// "shortcuts"-spec triggers use ("space" -> "space", "v" -> "v",
/// "pageup" -> "Page_Up"). Same key domain as `key_code`.
fn keysym_name(part: &str) -> Option<String> {
    let named = match part {
        "space" => "space",
        "enter" | "return" => "Return",
        "tab" => "Tab",
        "esc" | "escape" => "Escape",
        "backspace" => "BackSpace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Page_Up",
        "pagedown" => "Page_Down",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "`" => "grave",
        "-" => "minus",
        "=" => "equal",
        "[" => "bracketleft",
        "]" => "bracketright",
        "\\" => "backslash",
        ";" => "semicolon",
        "'" => "apostrophe",
        "," => "comma",
        "." => "period",
        "/" => "slash",
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
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c.to_string()),
        _ => None,
    }
}

/// A config hotkey string ("ctrl+alt+c") -> the XDG "shortcuts" spec trigger
/// form the GlobalShortcuts portal takes as a preferred trigger
/// ("CTRL+ALT+c"). Accepts exactly the combo strings `to_accelerator`
/// accepts; like it, the last mappable non-modifier part wins and an
/// unmappable one rejects the combo.
pub fn to_portal_trigger(hotkey: &str) -> Option<String> {
    let mut mods: Vec<&str> = Vec::new();
    let mut key: Option<String> = None;
    for p in hotkey.to_lowercase().split('+') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        match p {
            "ctrl" | "control" => {
                if !mods.contains(&"CTRL") {
                    mods.push("CTRL");
                }
            }
            "alt" => {
                if !mods.contains(&"ALT") {
                    mods.push("ALT");
                }
            }
            "shift" => {
                if !mods.contains(&"SHIFT") {
                    mods.push("SHIFT");
                }
            }
            "win" | "windows" | "meta" | "cmd" => {
                if !mods.contains(&"LOGO") {
                    mods.push("LOGO");
                }
            }
            _ => key = Some(keysym_name(p)?),
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
///
/// Hostile input it must survive (all real under KDE Wayland + XWayland
/// grabs): when the compositor moves keyboard focus mid-hold (e.g. the pill
/// window mapping), XWayland synthesizes a KeyRelease for every held key to
/// the grab client, with no matching re-press. That synthetic Released is
/// indistinguishable from the real one and commits after the debounce — but
/// the key is still physically down, so autorepeat later resumes as
/// Released+Pressed pairs whose Pressed would otherwise look like a fresh
/// press and toggle the take OFF mid-hold. The `resurrect` machinery below
/// detects that shape (a repeat-shaped Pressed with no live press) and
/// revives the committed hold instead of dispatching.
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
    /// `press_at` of the last take-starting hold whose release committed
    /// while the recording kept going. If that "release" was synthetic
    /// (XWayland focus change), the still-held key's autorepeat will show up
    /// as a repeat-shaped Pressed — which revives this hold so the eventual
    /// real release still finishes the take.
    resurrect: Option<Instant>,
}

impl HoldCore {
    /// A Pressed arrived. `starts_take` is whether dispatching this key
    /// right now would START a recording (sampled by the caller just before
    /// the dispatch).
    fn press(&mut self, now: Instant, starts_take: bool) -> PressAction {
        self.generation += 1; // voids any pending release check
        let repeat_shaped = self
            .release_at
            .is_some_and(|t| now.duration_since(t) <= Duration::from_millis(REPEAT_MS));
        if self.press_at.is_some() {
            if repeat_shaped {
                // Autorepeat Pressed while the hold is live — swallow it
                // (the bump above already voided its paired Released).
                return PressAction::Swallow;
            }
            // A press is "live" with no recent Released: the platform never
            // delivered the previous release (degrade-to-toggle). Falls
            // through to a fresh press.
        } else if repeat_shaped {
            // Second half of a Released+Pressed autorepeat pair with NO
            // live press: the paired Released hit a hold that was already
            // (wrongly) committed — the key is evidently still physically
            // down, so this press must never dispatch (dispatching here is
            // what toggled takes off mid-hold). If the committed hold
            // started the take, revive it.
            if let Some(press_at) = self.resurrect {
                self.press_at = Some(press_at);
                self.starts_take = true;
            }
            return PressAction::Swallow;
        }
        self.press_at = Some(now);
        self.starts_take = starts_take;
        self.resurrect = None;
        PressAction::Dispatch
    }

    /// A Released arrived: snapshot it for resolution after the debounce.
    /// None = stray release (e.g. the shortcut registered mid-hold, or the
    /// hold was already committed by a synthetic release) — still recorded
    /// in `release_at`, so a Pressed hot on its heels reads as the second
    /// half of an autorepeat pair.
    fn release(&mut self, now: Instant) -> Option<PendingRelease> {
        self.release_at = Some(now);
        let press_at = self.press_at?;
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
        let press_at = self.press_at.take(); // the real release (or so it seems)
        if pending.held && pending.starts_take && recording {
            // KNOWN LIMIT (X11-grab path only): a synthetic Released from
            // an XWayland focus loss arriving AFTER the hold threshold is
            // indistinguishable from the real release — the same focus loss
            // stops autorepeat, so no later event ever contradicts it, and
            // a dispatched Finish cannot be un-finished. It resolves as a
            // false push-to-talk finish and truncates the take. The
            // `resurrect` guard below can only cover the pre-threshold
            // shape. In practice the portal path (no synthetic edges)
            // replaces the grabs on Wayland, and the pill's no-focus fix
            // removes the main mid-hold focus steal.
            self.resurrect = None;
            ReleaseAction::Finish
        } else {
            // A quick tap keeps recording — today's toggle; the next tap
            // stops it. Remember a take-starting hold that leaves its
            // recording running: if this "release" was synthetic, the
            // still-held key's autorepeat resurrects the hold.
            self.resurrect = match press_at {
                Some(at) if pending.starts_take && recording => Some(at),
                _ => None,
            };
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

/// One hotkey edge from EITHER delivery backend — the X11/Win32 grab
/// (tauri-plugin-global-shortcut) or the Wayland GlobalShortcuts portal
/// (Activated = pressed, Deactivated = released). The dictate and paste
/// keys feed the tap-vs-hold core with both edges; the others fire on the
/// press edge only (panel presses go through `dispatch` into the
/// never-drop panel queue).
pub(crate) fn hotkey_event(app: &AppHandle, which: &'static str, pressed: bool) {
    if which == "dictate" || which == "paste" {
        let state = if pressed {
            ShortcutState::Pressed
        } else {
            ShortcutState::Released
        };
        hold_event(app, which, state);
    } else if pressed {
        eprintln!("hotkey fired: {which}");
        dispatch(app, which);
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
// coalesces to its net intent against the panel's REAL state, so no press
// is ever dropped or double-applied.
//
// A press does not blindly flip visibility: the panel can be VISIBLE yet
// BURIED under another window (the user clicked something on top of it),
// and hiding an invisible panel feels like the press did nothing — it then
// takes a second press to get the panel back. Each press instead advances
// a three-state cycle:
//
//     hidden            -> shown at the remembered spot, front and focused
//     visible + buried  -> raised to front and focused, SAME position
//     visible + focused -> hidden
//
// so hiding always takes exactly one press from the panel the user is
// actually looking at (the active window). Focused means REAL focus:
// `is_focused` is GTK's `is_active` on Linux and the Win32 focus flag on
// Windows, both driven by the server's focus events and never by our own
// set_focus request (unlike `is_visible`, which is GTK-local — see
// `apply_cmds`). In the short gap between our raise and the WM granting
// focus the panel still reads as buried, so a press there re-raises
// instead of hiding — harmless and self-correcting once focus lands.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PanelCmd {
    /// Hotkey / tray click: advance the hidden -> front -> hidden cycle.
    Toggle,
    /// Second launch: always end visible and in front, never hide.
    Summon,
}

/// The panel's press-relevant window state, read on the main thread right
/// before a burst is applied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PanelState {
    /// Not mapped.
    Hidden,
    /// Mapped but not the active window (buried under another window, or
    /// visible-but-unfocused).
    Buried,
    /// Mapped and the active (really-focused) window.
    Focused,
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
/// state. Every press advances the cycle in order — hidden -> focused,
/// buried -> focused, focused -> hidden — so a queued burst folds
/// deterministically: buried + press = raise; buried + press-press = hide
/// (press 1 raises, press 2 hides); hidden + press-press = stay hidden
/// (open-then-close, net nothing); hidden + press-press-press = a single
/// show. The plan re-maps only when the net visibility changed, and raises
/// whenever a non-empty burst ends with the panel front-and-focused.
fn coalesce(start: PanelState, cmds: &[PanelCmd]) -> PanelPlan {
    let mut cur = start;
    for cmd in cmds {
        cur = match (cmd, cur) {
            (PanelCmd::Toggle, PanelState::Focused) => PanelState::Hidden,
            (PanelCmd::Toggle, _) => PanelState::Focused,
            (PanelCmd::Summon, _) => PanelState::Focused,
        };
    }
    let started_visible = start != PanelState::Hidden;
    let ends_visible = cur != PanelState::Hidden;
    PanelPlan {
        map: (ends_visible != started_visible).then_some(ends_visible),
        front: !cmds.is_empty() && cur == PanelState::Focused,
    }
}

/// What the main-thread closure actually did, for the follow-ups.
struct Applied {
    shown: bool,
    front: bool,
}

/// Main-thread only: read the real visibility and focus, coalesce the burst
/// against them, and apply the map change. Nothing slow may run here —
/// position math and set_position/show/hide only.
fn apply_cmds(app: &AppHandle, cmds: &[PanelCmd]) -> Option<Applied> {
    let w = app.get_webview_window("panel")?;
    let visible = w.is_visible().unwrap_or(false);
    // is_focused is REAL focus on both backends (GTK is_active / Win32 focus
    // flag, both server-event-driven — see the section comment). A failed
    // query falls back to Focused so the legacy visible -> hide toggle still
    // applies: hiding must never cost extra presses because a backend
    // couldn't answer.
    let state = if !visible {
        PanelState::Hidden
    } else if w.is_focused().unwrap_or(true) {
        PanelState::Focused
    } else {
        PanelState::Buried
    };
    let plan = coalesce(state, cmds);
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

/// `toggle_panel`: advance the panel's show/raise/hide cycle NOW — the
/// window op runs with nothing slow in front of it; the fresh state
/// snapshot follows.
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
///
/// On a Wayland session the grabs registered here are only a bridge: they
/// go up instantly (so the app is never hotkey-less) and the GlobalShortcuts
/// portal then rebinds the same combos compositor-level on a worker thread,
/// dropping the grabs on success (see `hotkeys_portal`). Rebinds re-run the
/// whole pass, which on the portal side closes the old session and binds a
/// fresh one.
pub fn register_all(app: &AppHandle) {
    // Piggyback on the startup registration pass: seed the panel's
    // remembered position from config and start move-tracking (idempotent —
    // rebind passes are no-ops).
    crate::placement::init_panel_tracking(app);
    register_grabs(app, &configured_bindings(app));
    #[cfg(target_os = "linux")]
    if crate::hotkeys_portal::wayland_session() {
        let mappable = portal_bindings(app);
        if !mappable.is_empty() {
            eprintln!(
                "Wayland session: binding hotkeys through the GlobalShortcuts \
                 portal (the X11 grabs stay up until it succeeds)"
            );
            crate::hotkeys_portal::spawn_register(app, mappable);
        }
    }
}

/// All configured (which, combo) pairs, unmappable ones included — the
/// registration paths do their own filtering and logging.
fn configured_bindings(app: &AppHandle) -> Vec<(&'static str, String)> {
    let ctx = app.state::<AppCtx>();
    let cfg = lock(&ctx.cfg);
    ACTIONS
        .iter()
        .map(|(which, key)| (*which, cfg.get(key)))
        .collect()
}

/// The bindings the portal should bind: everything that parses. An
/// unparsable combo is skipped exactly like the grab path skips it.
#[cfg(target_os = "linux")]
pub(crate) fn portal_bindings(app: &AppHandle) -> Vec<(&'static str, String)> {
    configured_bindings(app)
        .into_iter()
        .filter(|(_, hk)| api::parse_hotkey(hk))
        .collect()
}

/// Portal-recovery path: put the X11 grabs back NOW (the portal just died;
/// the app must never be hotkey-less) without spawning another portal pass —
/// the caller runs its own bounded retries.
#[cfg(target_os = "linux")]
pub(crate) fn reregister_grabs(app: &AppHandle) {
    register_grabs(app, &configured_bindings(app));
}

/// Register `bindings` as plugin shortcuts (Win32 hooks on Windows, X11
/// grabs on Linux), replacing whatever was registered before.
fn register_grabs(app: &AppHandle, bindings: &[(&'static str, String)]) {
    let gs = app.global_shortcut();
    if let Err(e) = gs.unregister_all() {
        eprintln!("hotkey unregister_all failed: {e}");
    }
    for (which, hk) in bindings {
        let which = *which;
        if !api::parse_hotkey(hk) {
            eprintln!("hotkey '{which}' = '{hk}' is unmappable; skipped");
            continue;
        }
        let Some(accel) = to_accelerator(hk) else {
            eprintln!("hotkey '{which}' = '{hk}' is unmappable; skipped");
            continue;
        };
        let result = gs.on_shortcut(accel.as_str(), move |app, _shortcut, event| {
            hotkey_event(app, which, event.state == ShortcutState::Pressed);
        });
        match result {
            Ok(()) => eprintln!("hotkey registered: {which} = {hk}"),
            Err(e) => {
                eprintln!(
                    "hotkey registration FAILED for {which} = {hk} ({e}; in use by another app?)"
                );
                if which == "panel" {
                    panel_hotkey_warning(app, hk);
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

    #[test]
    fn portal_triggers_for_the_defaults() {
        assert_eq!(
            to_portal_trigger("ctrl+alt+space").as_deref(),
            Some("CTRL+ALT+space")
        );
        assert_eq!(
            to_portal_trigger("ctrl+alt+v").as_deref(),
            Some("CTRL+ALT+v")
        );
        assert_eq!(
            to_portal_trigger("ctrl+alt+c").as_deref(),
            Some("CTRL+ALT+c")
        );
        assert_eq!(
            to_portal_trigger("ctrl+alt+x").as_deref(),
            Some("CTRL+ALT+x")
        );
    }

    #[test]
    fn portal_trigger_key_forms() {
        // letters and digits keep their xkb keysym names verbatim
        assert_eq!(to_portal_trigger("shift+a").as_deref(), Some("SHIFT+a"));
        assert_eq!(to_portal_trigger("win+5").as_deref(), Some("LOGO+5"));
        assert_eq!(to_portal_trigger("meta+9").as_deref(), Some("LOGO+9"));
        assert_eq!(to_portal_trigger("shift+f12").as_deref(), Some("SHIFT+F12"));
        assert_eq!(
            to_portal_trigger("ctrl+pageup").as_deref(),
            Some("CTRL+Page_Up")
        );
        assert_eq!(to_portal_trigger("ctrl+`").as_deref(), Some("CTRL+grave"));
        assert_eq!(to_portal_trigger("esc").as_deref(), Some("Escape"));
        assert_eq!(
            to_portal_trigger("ctrl+backspace").as_deref(),
            Some("CTRL+BackSpace")
        );
        // duplicate/alias modifiers collapse, mixed case accepted
        assert_eq!(
            to_portal_trigger("Ctrl+Control+Shift+C").as_deref(),
            Some("CTRL+SHIFT+c")
        );
    }

    #[test]
    fn portal_trigger_rejects_what_the_accelerator_rejects() {
        for combo in ["", "ctrl+alt", "ctrl+bogus", "ctrl+f25"] {
            assert_eq!(to_portal_trigger(combo), None, "combo {combo:?}");
            assert_eq!(to_accelerator(combo), None, "combo {combo:?}");
        }
        // and both accept the same valid domain
        for combo in ["ctrl+alt+space", "win+f1", "shift+.", "ctrl+alt+enter"] {
            assert!(to_portal_trigger(combo).is_some(), "combo {combo:?}");
            assert!(to_accelerator(combo).is_some(), "combo {combo:?}");
        }
    }

    use PanelCmd::{Summon, Toggle};
    use PanelState::{Buried, Focused, Hidden};

    fn plan(state: PanelState, cmds: &[PanelCmd]) -> PanelPlan {
        coalesce(state, cmds)
    }

    #[test]
    fn single_press_advances_the_cycle() {
        assert_eq!(
            plan(Hidden, &[Toggle]),
            PanelPlan {
                map: Some(true),
                front: true
            },
            "hidden + press = show and bring to front"
        );
        assert_eq!(
            plan(Buried, &[Toggle]),
            PanelPlan {
                map: None,
                front: true
            },
            "buried + press = raise and focus only — no re-map, no hide"
        );
        assert_eq!(
            plan(Focused, &[Toggle]),
            PanelPlan {
                map: Some(false),
                front: false
            },
            "focused + press = hide, no raise"
        );
    }

    #[test]
    fn rapid_presses_coalesce_to_net_intent() {
        // press-press from buried: raise-then-hide — each press advances
        // the cycle, so the burst nets out to a single hide
        assert_eq!(
            plan(Buried, &[Toggle, Toggle]),
            PanelPlan {
                map: Some(false),
                front: false
            }
        );
        // press-press from hidden: open-then-close, net nothing, ends hidden
        assert_eq!(
            plan(Hidden, &[Toggle, Toggle]),
            PanelPlan {
                map: None,
                front: false
            }
        );
        // press-press-press from hidden ends visible (one show, one raise)
        assert_eq!(
            plan(Hidden, &[Toggle, Toggle, Toggle]),
            PanelPlan {
                map: Some(true),
                front: true
            }
        );
        // press-press from focused: close-then-open — no map change, but
        // the panel ends (stays) visible and is raised
        assert_eq!(
            plan(Focused, &[Toggle, Toggle]),
            PanelPlan {
                map: None,
                front: true
            }
        );
        // press-press-press from buried: raise, hide, show — already
        // mapped, so no map change, but it ends front-and-focused
        assert_eq!(
            plan(Buried, &[Toggle, Toggle, Toggle]),
            PanelPlan {
                map: None,
                front: true
            }
        );
        // four presses from focused: even parity, ends (stays) visible and
        // raised because the last press turned it back on
        assert_eq!(
            plan(Focused, &[Toggle, Toggle, Toggle, Toggle]),
            PanelPlan {
                map: None,
                front: true
            }
        );
        // odd parity from focused ends hidden
        assert_eq!(
            plan(Focused, &[Toggle, Toggle, Toggle]),
            PanelPlan {
                map: Some(false),
                front: false
            }
        );
    }

    #[test]
    fn summon_always_ends_visible_and_in_front() {
        assert_eq!(
            plan(Hidden, &[Summon]),
            PanelPlan {
                map: Some(true),
                front: true
            }
        );
        assert_eq!(
            plan(Focused, &[Summon]),
            PanelPlan {
                map: None,
                front: true
            },
            "already visible: raise only, never re-map"
        );
        assert_eq!(
            plan(Buried, &[Summon]),
            PanelPlan {
                map: None,
                front: true
            },
            "a buried panel is raised, never hidden, by a summon"
        );
        // a toggle after a summon still wins — strict press order
        assert_eq!(
            plan(Focused, &[Summon, Toggle]),
            PanelPlan {
                map: Some(false),
                front: false
            }
        );
        // and a summon after a hide-toggle rescues visibility
        assert_eq!(
            plan(Focused, &[Toggle, Summon]),
            PanelPlan {
                map: None,
                front: true
            }
        );
    }

    #[test]
    fn empty_burst_is_a_noop() {
        for state in [Hidden, Buried, Focused] {
            assert_eq!(
                plan(state, &[]),
                PanelPlan {
                    map: None,
                    front: false
                },
                "{state:?}"
            );
        }
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
    fn focus_steal_synthetic_release_does_not_stop_the_hold() {
        // THE KDE Wayland premature-stop bug: hold Ctrl+Alt+V, the take
        // starts, the pill maps and the compositor moves keyboard focus —
        // XWayland synthesizes a Released for the held chord (no re-press
        // reaches the grab). It commits as if the user let go. When
        // autorepeat resumes at the repeat delay as Released+Pressed pairs,
        // the stray Released pairs with a Pressed that used to look like a
        // fresh press and toggled the take OFF while the key was still
        // physically held.
        let mut c = HoldCore::default();
        let t0 = Instant::now();
        // the hold starts the take
        assert_eq!(c.press(t0, true), PressAction::Dispatch);
        // ~150ms in: synthetic Released from the focus change, then silence
        let synth = c.release(t0 + ms(150)).expect("hold is live");
        assert!(!synth.held);
        // the debounce expires with recording still on: it commits
        assert_eq!(c.resolve(&synth, true), ReleaseAction::Inert);
        // autorepeat resumes at the 600ms repeat delay: a Released+Pressed
        // pair whose Released finds no live press...
        let t1 = t0 + ms(600);
        assert!(c.release(t1).is_none(), "stray release: hold was committed");
        // ...and whose Pressed is repeat-shaped: swallowed, hold revived
        assert_eq!(c.press(t1 + ms(3), false), PressAction::Swallow);
        // further repeat pairs behave like normal autorepeat against the
        // revived hold
        let mut t = t1 + ms(40);
        let mut voided = Vec::new();
        for _ in 0..3 {
            let p = c.release(t).expect("hold is live again");
            assert_eq!(c.press(t + ms(3), false), PressAction::Swallow);
            voided.push(p);
            t += ms(40);
        }
        for p in &voided {
            assert_eq!(c.resolve(p, true), ReleaseAction::Inert, "voided repeat");
        }
        // the real physical release finally lands — push-to-talk finishes,
        // with the hold measured from the ORIGINAL press
        let real = c.release(t + ms(100)).expect("hold is live");
        assert!(real.held, "hold duration measured from the original press");
        assert_eq!(c.resolve(&real, true), ReleaseAction::Finish);
    }

    #[test]
    fn repeat_shaped_press_never_dispatches() {
        // A Pressed hard on the heels of a Released is the second half of
        // an autorepeat pair even when there is no live press and nothing
        // to resurrect — it must never dispatch (a dispatch here is a
        // spurious take toggle).
        let mut c = HoldCore::default();
        let t0 = Instant::now();
        assert!(c.release(t0).is_none()); // stray release
        assert_eq!(c.press(t0 + ms(10), true), PressAction::Swallow);
        // a press a human-scale gap later is genuine
        assert_eq!(c.press(t0 + ms(300), true), PressAction::Dispatch);
    }

    #[test]
    fn synthetic_release_after_cancel_stays_dead() {
        // Synthetic release commits mid-hold, the take is then cancelled
        // elsewhere (recording = false at commit): nothing to resurrect —
        // later repeat-shaped presses stay swallowed without reviving a
        // hold, and the eventual real release is inert.
        let mut c = HoldCore::default();
        let t0 = Instant::now();
        assert_eq!(c.press(t0, true), PressAction::Dispatch);
        let synth = c.release(t0 + ms(150)).expect("hold is live");
        assert_eq!(c.resolve(&synth, false), ReleaseAction::Inert); // take already dead
        let t1 = t0 + ms(600);
        assert!(c.release(t1).is_none());
        assert_eq!(c.press(t1 + ms(3), false), PressAction::Swallow);
        assert!(c.release(t1 + ms(40)).is_none(), "no hold was revived");
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
