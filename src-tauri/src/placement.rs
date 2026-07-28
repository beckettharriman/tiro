//! Multi-monitor window placement, ported from the original's Win32 layer
//! (`_active_work_area` / `_place_window` / `_reposition_burst` /
//! `_summon_front`, WIN32-1):
//!
//! - windows land on the ACTIVE monitor's work area, not always the primary:
//!   the monitor under the cursor, falling back to the panel's monitor, then
//!   the primary (the original preferred the foreground window's monitor — a
//!   Win32-only signal; the cursor is its own documented fallback and the
//!   portable equivalent)
//! - pill: bottom-center, ~110 px up from the work-area bottom
//! - panel: reappears at its REMEMBERED position — the spot the user last
//!   left it, tracked live while visible, captured again right before a
//!   hide, and persisted in config (`panel_pos`, "x,y") so it survives
//!   restarts. Centering (nudged mh/16 below dead-center) happens only when
//!   there is no remembered position yet or the remembered one is off every
//!   current monitor (unplugged screen / changed layout). This deliberately
//!   replaces the original's "center until first drag" behavior.
//! - summon raises the panel over the active window via a TOPMOST->NOTOPMOST
//!   flip (permanent only when pinned) and focuses it
//!
//! All window ops are proxied to the main thread (GTK requirement); math is
//! in physical pixels like the original.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, PhysicalPosition};

use crate::flow::{lock, AppCtx};

/// How long a drag must settle before the remembered position is written to
/// config (saves are atomic fsync writes — never one per motion event).
const SAVE_DEBOUNCE_MS: u64 = 800;

/// Placement state: the pin flag and the panel's remembered position.
#[derive(Default)]
pub struct Placement {
    pinned: AtomicBool,
    /// Last known on-screen position of the panel (physical px, outer/frame
    /// origin). Seeded from config at startup, refreshed by every Moved
    /// event while the panel is visible, and captured right before a hide.
    panel_pos: Mutex<Option<(i32, i32)>>,
    /// Debounce generation for persisting `panel_pos` to config.
    save_gen: AtomicU64,
    /// One-time guard for `init_panel_tracking`.
    tracking: AtomicBool,
}

fn state(app: &AppHandle) -> tauri::State<'_, Placement> {
    app.state::<Placement>()
}

pub fn pinned(app: &AppHandle) -> bool {
    state(app).pinned.load(Ordering::SeqCst)
}

/// The pin button: remember the flag and apply always-on-top.
pub fn set_pin(app: &AppHandle, on: bool) {
    state(app).pinned.store(on, Ordering::SeqCst);
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(w) = app.get_webview_window("panel") {
            let _ = w.set_always_on_top(on);
        }
    });
}

/// Parse a persisted `panel_pos` config value ("x,y"). Anything that is not
/// two integers is treated as unset (garbage in the file must never place
/// the window somewhere wild or crash).
pub(crate) fn parse_panel_pos(s: &str) -> Option<(i32, i32)> {
    let (x, y) = s.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Seed the remembered panel position from config and start tracking moves.
/// Idempotent; called from hotkey registration at startup.
pub fn init_panel_tracking(app: &AppHandle) {
    let st = state(app);
    if st
        .tracking
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let saved = {
        let ctx = app.state::<AppCtx>();
        let s = lock(&ctx.cfg).get("panel_pos");
        parse_panel_pos(&s)
    };
    *lock(&st.panel_pos) = saved;
    // Every move of the VISIBLE panel — a user drag or our own placement —
    // updates the remembered position; persisting debounces so a drag
    // writes config once after it settles. This also covers hide paths that
    // bypass the hotkey layer (the panel's close button), because the drag
    // was already recorded while the window was still visible.
    if let Some(w) = app.get_webview_window("panel") {
        let win = w.clone();
        let app = app.clone();
        w.on_window_event(move |ev| {
            if let tauri::WindowEvent::Moved(p) = ev {
                if win.is_visible().unwrap_or(false) {
                    note_panel_pos(&app, p.x, p.y, SAVE_DEBOUNCE_MS);
                }
            }
        });
    }
}

/// Record the panel position in memory now; persist to config on a worker
/// after `delay_ms` unless a newer note supersedes this one.
fn note_panel_pos(app: &AppHandle, x: i32, y: i32, delay_ms: u64) {
    let st = state(app);
    *lock(&st.panel_pos) = Some((x, y));
    let gen = st.save_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        if delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        let st = state(&app);
        if st.save_gen.load(Ordering::SeqCst) != gen {
            return; // a newer position owns the save
        }
        let Some((x, y)) = *lock(&st.panel_pos) else {
            return;
        };
        let value = format!("{x},{y}");
        let ctx = app.state::<AppCtx>();
        let mut cfg = lock(&ctx.cfg);
        if cfg.get("panel_pos") != value {
            cfg.set("panel_pos", &value);
        }
    });
}

/// Capture the panel's position right before a hide, while the window is
/// still mapped (an unmapped X11 window may report a stale position), and
/// persist it without the drag debounce. The disk write itself still runs
/// on a worker — nothing slow on the main thread. Main-thread only.
pub(crate) fn remember_panel_now(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("panel") {
        if let Ok(p) = w.outer_position() {
            note_panel_pos(app, p.x, p.y, 0);
        }
    }
}

/// `_active_work_area`: (left, top, right, bottom) of the active monitor's
/// work area in physical pixels; None if every monitor query fails.
fn active_work_area(app: &AppHandle) -> Option<(i32, i32, i32, i32)> {
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| {
            app.get_webview_window("panel")
                .and_then(|w| w.current_monitor().ok().flatten())
        })
        .or_else(|| app.primary_monitor().ok().flatten())?;
    let wa = monitor.work_area();
    Some((
        wa.position.x,
        wa.position.y,
        wa.position.x + wa.size.width as i32,
        wa.position.y + wa.size.height as i32,
    ))
}

/// Where a window of size (ww, wh) goes on the work area (l, t, r, b):
/// centered a touch below dead-center, or bottom-center for the pill.
/// Pure math, extracted for tests.
fn centered_spot(work: (i32, i32, i32, i32), ww: i32, wh: i32, bottom: bool) -> Option<(i32, i32)> {
    let (ml, mt, mr, mb) = work;
    let (mw, mh) = (mr - ml, mb - mt);
    if mw <= 0 || mh <= 0 {
        return None;
    }
    let x = ml + (mw - ww) / 2;
    // centered windows sit a little below dead-center — reads better than
    // the exact middle and keeps the panel clear of the very top.
    let y = if bottom {
        mt + mh - wh - 110
    } else {
        mt + (mh - wh) / 2 + mh / 16
    };
    Some((x, y))
}

/// Whether a window rect's center point lands inside any of the given work
/// areas. Pure, extracted for tests.
fn center_on_any(areas: &[(i32, i32, i32, i32)], x: i32, y: i32, ww: i32, wh: i32) -> bool {
    let (cx, cy) = (x + ww / 2, y + wh / 2);
    areas
        .iter()
        .any(|(l, t, r, b)| cx >= *l && cx < *r && cy >= *t && cy < *b)
}

/// A remembered position is only trusted while its window would still be on
/// SOME current monitor's work area (a spot on an unplugged screen must not
/// strand the panel off-screen).
fn pos_on_screen(app: &AppHandle, x: i32, y: i32, ww: i32, wh: i32) -> bool {
    let Ok(monitors) = app.available_monitors() else {
        return false;
    };
    let areas: Vec<(i32, i32, i32, i32)> = monitors
        .iter()
        .map(|m| {
            let wa = m.work_area();
            (
                wa.position.x,
                wa.position.y,
                wa.position.x + wa.size.width as i32,
                wa.position.y + wa.size.height as i32,
            )
        })
        .collect();
    center_on_any(&areas, x, y, ww, wh)
}

/// `_place_window`: put `label` where it belongs — the panel's remembered
/// position when there is a valid one, else centered on the active work
/// area (`bottom` docks the pill bottom-center ~110 px up instead). Must
/// run on the main thread.
fn place_window(app: &AppHandle, label: &str, bottom: bool) {
    let Some(w) = app.get_webview_window(label) else {
        return;
    };
    let Ok(size) = w.outer_size() else {
        return;
    };
    let (ww, wh) = (size.width as i32, size.height as i32);
    if label == "panel" {
        let remembered = *lock(&state(app).panel_pos);
        if let Some((x, y)) = remembered {
            if pos_on_screen(app, x, y, ww, wh) {
                let _ = w.set_position(PhysicalPosition::new(x, y));
                return;
            }
        }
    }
    let Some(work) = active_work_area(app) else {
        return;
    };
    let Some((x, y)) = centered_spot(work, ww, wh, bottom) else {
        return;
    };
    let _ = w.set_position(PhysicalPosition::new(x, y));
    if label == "panel" {
        // The centered spot becomes the remembered one (in memory; the Moved
        // event it triggers handles persistence).
        *lock(&state(app).panel_pos) = Some((x, y));
    }
}

/// `_position_pill` / `_position_panel` as one main-thread entry.
pub(crate) fn position(app: &AppHandle, label: &'static str) {
    let bottom = label == "pill";
    place_window(app, label, bottom);
    if label == "panel" {
        // honor the pin without touching it elsewhere in the burst
        if let Some(w) = app.get_webview_window("panel") {
            let _ = w.set_always_on_top(pinned(app));
        }
    }
}

/// `_reposition_burst`: showing is async and the WM places the window
/// itself, so a single move can lose the race — re-apply a few times over
/// ~300 ms; our position is the last writer and wins.
pub fn reposition_burst(app: &AppHandle, label: &'static str) {
    let app = app.clone();
    std::thread::spawn(move || {
        for _ in 0..6 {
            let a = app.clone();
            let _ = app.run_on_main_thread(move || position(&a, label));
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

/// `_summon_front`: bring the panel above the active window and focus it.
/// A TOPMOST->NOTOPMOST flip raises it without leaving it always-on-top
/// (unless pinned). Harmless if the panel was hidden again meanwhile:
/// set_focus is a no-op on a non-visible window (tao GTK checks), so this
/// can never re-map a window a later press hid.
pub fn summon_front(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        let a = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(w) = a.get_webview_window("panel") {
                let _ = w.set_always_on_top(true);
                if !pinned(&a) {
                    let _ = w.set_always_on_top(false);
                }
                let _ = w.set_focus();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_pos_parses_x_comma_y() {
        assert_eq!(parse_panel_pos("100,200"), Some((100, 200)));
        assert_eq!(parse_panel_pos(" -5 , 30 "), Some((-5, 30)));
        assert_eq!(parse_panel_pos("0,0"), Some((0, 0)));
    }

    #[test]
    fn panel_pos_garbage_is_unset() {
        for s in ["", ",", "abc", "12", "12,", ",34", "1,2,3", "1.5,2", "x,y"] {
            assert_eq!(parse_panel_pos(s), None, "{s:?} must be unset");
        }
    }

    #[test]
    fn centered_spot_matches_original_math() {
        // 1920x1040 work area at (0,40): a 560x640 panel centers with the
        // mh/16 downward nudge; a 300x88 pill docks bottom-center 110 up.
        let work = (0, 40, 1920, 1080);
        assert_eq!(
            centered_spot(work, 560, 640, false),
            Some((680, 40 + (1040 - 640) / 2 + 1040 / 16))
        );
        assert_eq!(
            centered_spot(work, 300, 88, true),
            Some((810, 40 + 1040 - 88 - 110))
        );
        assert_eq!(centered_spot((0, 0, 0, 0), 560, 640, false), None);
    }

    #[test]
    fn remembered_position_validity_across_monitors() {
        let two = [(0, 0, 1920, 1080), (1920, 0, 3840, 1080)];
        // window centered on the second monitor: valid
        assert!(center_on_any(&two, 2500, 300, 560, 640));
        // straddling the seam but center on monitor 1: valid
        assert!(center_on_any(&two, 1700, 300, 560, 640));
        // far off every monitor (unplugged screen at negative x): invalid
        assert!(!center_on_any(&two, -2000, 300, 560, 640));
        assert!(!center_on_any(&two, 100, 2000, 560, 640));
        // no monitors reported at all: invalid, callers fall back to center
        assert!(!center_on_any(&[], 100, 100, 560, 640));
    }
}
