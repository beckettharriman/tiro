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
//! - panel: centered, nudged mh/16 below dead-center — until the user drags
//!   it, after which summon leaves it wherever they put it
//! - summon raises the panel over the active window via a TOPMOST->NOTOPMOST
//!   flip (permanent only when pinned) and focuses it
//!
//! All window ops are proxied to the main thread (GTK requirement); math is
//! in physical pixels like the original.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, PhysicalPosition};

/// Placement state: the pin flag and the drag-tracking for the panel.
#[derive(Default)]
pub struct Placement {
    pinned: AtomicBool,
    moved: AtomicBool,
    /// Last position WE set the panel to — drift from it means the user
    /// dragged the panel (there is no drag-end callback to hook instead).
    last_panel_pos: Mutex<Option<(i32, i32)>>,
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

/// `_panel_moved`: true once the user has dragged the panel; summon stops
/// auto-centering it. Detected as drift from the last position we set.
pub fn panel_moved(app: &AppHandle) -> bool {
    let st = state(app);
    if st.moved.load(Ordering::SeqCst) {
        return true;
    }
    let last = *st.last_panel_pos.lock().unwrap_or_else(|e| e.into_inner());
    if let (Some((lx, ly)), Some(w)) = (last, app.get_webview_window("panel")) {
        if let Ok(p) = w.outer_position() {
            if (p.x - lx).abs() + (p.y - ly).abs() > 3 {
                st.moved.store(true, Ordering::SeqCst);
                return true;
            }
        }
    }
    false
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

/// `_place_window`: center `label` on the active work area; `bottom` docks
/// it bottom-center ~110 px up instead. Must run on the main thread.
fn place_window(app: &AppHandle, label: &str, bottom: bool) {
    let Some(w) = app.get_webview_window(label) else {
        return;
    };
    let Ok(size) = w.outer_size() else {
        return;
    };
    let (ww, wh) = (size.width as i32, size.height as i32);
    let Some((ml, mt, mr, mb)) = active_work_area(app) else {
        return;
    };
    let (mw, mh) = (mr - ml, mb - mt);
    if mw <= 0 || mh <= 0 {
        return;
    }
    let x = ml + (mw - ww) / 2;
    // centered windows sit a little below dead-center — reads better than
    // the exact middle and keeps the panel clear of the very top.
    let y = if bottom {
        mt + mh - wh - 110
    } else {
        mt + (mh - wh) / 2 + mh / 16
    };
    let _ = w.set_position(PhysicalPosition::new(x, y));
    if label == "panel" {
        *state(app)
            .last_panel_pos
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((x, y));
    }
}

/// `_position_pill` / `_position_panel` as one main-thread entry.
fn position(app: &AppHandle, label: &'static str) {
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
/// (unless pinned).
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
