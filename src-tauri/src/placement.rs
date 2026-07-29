//! Multi-monitor window placement, ported from the original's Win32 layer
//! (`_active_work_area` / `_place_window` / `_reposition_burst` /
//! `_summon_front`, WIN32-1):
//!
//! - windows land on the ACTIVE monitor's work area, not always the primary:
//!   the monitor under the cursor, falling back to the panel's monitor, then
//!   the primary (the original preferred the foreground window's monitor — a
//!   Win32-only signal; the cursor is its own documented fallback and the
//!   portable equivalent)
//! - pill: centered horizontally, docked to the top or bottom work-area edge
//!   per `pill_position` / `pill_padding` (default: bottom, 110 px up — the
//!   original's fixed spot, so untouched configs behave exactly as before)
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

/// The original's fixed pill offset: ~110 px up from the work-area bottom.
/// Doubles as the `pill_padding` default so untouched configs are identical.
const PILL_PADDING_DEFAULT: i32 = 110;

/// Resolve the pill placement config values to (dock at top?, padding px).
/// Anything unrecognized falls back to the historical default — bottom,
/// 110 px — and a garbage padding never crashes (invalid -> default,
/// negative -> 0). Pure, extracted for tests.
pub(crate) fn resolve_pill_placement(position: &str, padding: &str) -> (bool, i32) {
    let top = position.trim().eq_ignore_ascii_case("top");
    let pad =
        crate::api::clamp_int_str(padding, 0, 100_000, i64::from(PILL_PADDING_DEFAULT)) as i32;
    (top, pad)
}

/// Where the pill of size (ww, wh) goes on the work area (l, t, r, b):
/// centered horizontally, docked `padding` px from the top or bottom edge
/// (padding clamped to half the work-area height so it can never push the
/// pill past the middle). Pure math, extracted for tests.
fn pill_spot(
    work: (i32, i32, i32, i32),
    ww: i32,
    wh: i32,
    top: bool,
    padding: i32,
) -> Option<(i32, i32)> {
    let (ml, mt, mr, mb) = work;
    let (mw, mh) = (mr - ml, mb - mt);
    if mw <= 0 || mh <= 0 {
        return None;
    }
    let pad = padding.clamp(0, mh / 2);
    let x = ml + (mw - ww) / 2;
    let y = if top { mt + pad } else { mb - wh - pad };
    Some((x, y))
}

/// Where a window of size (ww, wh) goes on the work area (l, t, r, b):
/// centered a touch below dead-center. Pure math, extracted for tests.
fn centered_spot(work: (i32, i32, i32, i32), ww: i32, wh: i32) -> Option<(i32, i32)> {
    let (ml, mt, mr, mb) = work;
    let (mw, mh) = (mr - ml, mb - mt);
    if mw <= 0 || mh <= 0 {
        return None;
    }
    let x = ml + (mw - ww) / 2;
    // centered windows sit a little below dead-center — reads better than
    // the exact middle and keeps the panel clear of the very top.
    let y = mt + (mh - wh) / 2 + mh / 16;
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
/// area (the pill instead docks to the configured edge via `pill_spot`).
/// Must run on the main thread.
fn place_window(app: &AppHandle, label: &str) {
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
    let spot = if label == "pill" {
        let (top, pad) = {
            let ctx = app.state::<AppCtx>();
            let cfg = lock(&ctx.cfg);
            resolve_pill_placement(&cfg.get("pill_position"), &cfg.get("pill_padding"))
        };
        pill_spot(work, ww, wh, top, pad)
    } else {
        centered_spot(work, ww, wh)
    };
    let Some((x, y)) = spot else {
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
    place_window(app, label);
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

/// Re-apply the pill's configured spot immediately if it is on screen right
/// now — a placement-setting change must not wait for the next show. A
/// hidden pill is left alone (the next `show_pill` burst places it fresh).
pub fn reposition_pill_if_visible(app: &AppHandle) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        if let Some(w) = app.get_webview_window("pill") {
            if w.is_visible().unwrap_or(false) {
                position(&app, "pill");
            }
        }
    });
}

/// `_summon_front`: bring the panel above the active window and focus it.
/// A TOPMOST->NOTOPMOST flip raises it without leaving it always-on-top
/// (unless pinned). Harmless if the panel was hidden again meanwhile:
/// Panel geometry (logical px): the design's compact and expanded surfaces.
/// Height never changes; only the width doubles for the advanced area.
pub const PANEL_W_COMPACT: u32 = 400;
pub const PANEL_W_EXPANDED: u32 = 800;
pub const PANEL_H: u32 = 560;

/// Resize the panel window for the advanced (expanded) surface. The window
/// is borderless and pinned by min==max size constraints (that is what
/// keeps a `resizable: true` frameless window fixed on every WM), so the
/// constraints and the size move together. Expanding can push the right
/// edge past the work area — clamp x so the whole surface stays visible
/// (the Moved event this triggers persists the shift like any drag).
pub fn set_panel_expanded(app: &AppHandle, on: bool) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        let Some(w) = app.get_webview_window("panel") else {
            return;
        };
        let width = if on {
            PANEL_W_EXPANDED
        } else {
            PANEL_W_COMPACT
        };
        let size = tauri::LogicalSize::new(width, PANEL_H);
        let _ = w.set_min_size(Some(size));
        let _ = w.set_max_size(Some(size));
        let _ = w.set_size(size);
        if on {
            let scale = w.scale_factor().unwrap_or(1.0);
            let phys_w = (f64::from(width) * scale).round() as i32;
            if let (Ok(pos), Some((wl, _, wr, _))) = (w.outer_position(), active_work_area(&app)) {
                // left-align when the work area is narrower than the panel
                let x = pos.x.min(wr - phys_w).max(wl);
                if x != pos.x {
                    let _ = w.set_position(PhysicalPosition::new(x, pos.y));
                }
            }
        }
    });
}

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

/// X11 (Linux): finish the pill's ICCCM "No Input" contract so no window
/// manager ever focuses or activates it.
///
/// `focusable: false` gets tao/GTK as far as WM_HINTS `input = False`, but
/// GDK unconditionally advertises WM_TAKE_FOCUS in WM_PROTOCOLS when it
/// realizes a toplevel (gtk3 `gdk/x11/gdkwindow-x11.c`, `set_wm_protocols`).
/// Per ICCCM 4.1.7, `input = False` *with* WM_TAKE_FOCUS is the "Globally
/// Active" input model, and KWin counts such a window as focus-wanting
/// (`wantsInput()` is `acceptsFocus() || supportsProtocol(TakeFocus)`), so
/// it still activates the pill when it maps — deactivating the app being
/// dictated into and breaking paste-at-cursor, even though the client never
/// grabs X focus itself. Removing WM_TAKE_FOCUS turns the pair
/// (`input = False`, no WM_TAKE_FOCUS) into the "No Input" model, which
/// window managers must never focus or activate.
///
/// GDK rewrites WM_PROTOCOLS on every realize, so this hooks `realize`
/// (strip before the map that follows) and `map` (re-assert on every show),
/// plus one immediate strip for the already-realized hidden window. Windows
/// needs none of this: `focusable: false` maps to WS_EX_NOACTIVATE there.
#[cfg(target_os = "linux")]
pub fn pill_no_input_fixup(app: &tauri::App) {
    use gtk::prelude::*;
    let Some(pill) = app.webview_windows().get("pill").cloned() else {
        return;
    };
    let Ok(gtk_win) = pill.gtk_window() else {
        return;
    };
    gtk_win.connect_realize(|w| strip_wm_take_focus(w.upcast_ref::<gtk::Widget>()));
    gtk_win.connect_map(|w| strip_wm_take_focus(w.upcast_ref::<gtk::Widget>()));
    if gtk_win.is_realized() {
        strip_wm_take_focus(gtk_win.upcast_ref::<gtk::Widget>());
    }
}

/// Rewrite the widget's WM_PROTOCOLS without WM_TAKE_FOCUS (no-op when the
/// property is absent or already clean). Runs on the GTK main thread only.
#[cfg(target_os = "linux")]
fn strip_wm_take_focus(widget: &gtk::Widget) {
    use gtk::gdk;
    use gtk::glib::translate::ToGlibPtr;
    use gtk::prelude::*;

    let Some(gdk_win) = widget.window() else {
        return;
    };
    let wm_protocols = gdk::Atom::intern("WM_PROTOCOLS");
    let atom_type = gdk::Atom::intern("ATOM");
    let take_focus = gdk::Atom::intern("WM_TAKE_FOCUS");

    // gdk_property_get with type ATOM returns the entries converted to
    // GdkAtom (pointer-sized each); actual_length is in bytes.
    let mut actual_type: gdk::ffi::GdkAtom = std::ptr::null_mut();
    let mut actual_format: std::os::raw::c_int = 0;
    let mut actual_length: std::os::raw::c_int = 0;
    let mut data: *mut u8 = std::ptr::null_mut();
    let found = unsafe {
        gdk::ffi::gdk_property_get(
            gdk_win.to_glib_none().0,
            wm_protocols.to_glib_none().0,
            atom_type.to_glib_none().0,
            0,
            1024, // plenty: GDK writes at most 4 protocol atoms
            0,    // pdelete = false
            &mut actual_type,
            &mut actual_format,
            &mut actual_length,
            &mut data,
        )
    } != gtk::glib::ffi::GFALSE;
    if !found || data.is_null() {
        return;
    }
    let count = actual_length as usize / std::mem::size_of::<gdk::ffi::GdkAtom>();
    let atoms: Vec<usize> = unsafe {
        std::slice::from_raw_parts(data as *const gdk::ffi::GdkAtom, count)
            .iter()
            .map(|&a| a as usize)
            .collect()
    };
    unsafe { gtk::glib::ffi::g_free(data as *mut _) };

    let Some(kept) = without_protocol(&atoms, take_focus.value()) else {
        return; // WM_TAKE_FOCUS was not advertised; nothing to do
    };
    // gdk_property_change with type ATOM expects GdkAtom values and converts
    // them back to X atoms; c_ulong and GdkAtom are both pointer-sized here.
    let kept: Vec<std::os::raw::c_ulong> = kept
        .into_iter()
        .map(|a| a as std::os::raw::c_ulong)
        .collect();
    gdk::property_change(
        &gdk_win,
        &wm_protocols,
        &atom_type,
        32,
        gdk::PropMode::Replace,
        gdk::ChangeData::ULongs(&kept),
    );
}

/// Pure core of the WM_PROTOCOLS strip: drop `unwanted` from `atoms`.
/// Returns `None` when `unwanted` is not present (callers skip the X write —
/// rewriting an unchanged property every map would just spam PropertyNotify).
#[cfg(target_os = "linux")]
fn without_protocol(atoms: &[usize], unwanted: usize) -> Option<Vec<usize>> {
    if !atoms.contains(&unwanted) {
        return None;
    }
    Some(atoms.iter().copied().filter(|&a| a != unwanted).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn wm_protocols_strip_drops_only_take_focus() {
        // Atom values are opaque ids; stand-ins are fine for the pure core.
        let (delete, take_focus, ping, sync) = (11, 22, 33, 44);
        // The exact list GDK writes at realize -> TAKE_FOCUS removed, order kept.
        assert_eq!(
            without_protocol(&[delete, take_focus, ping, sync], take_focus),
            Some(vec![delete, ping, sync])
        );
        // Already clean -> None, so callers never rewrite the property.
        assert_eq!(without_protocol(&[delete, ping, sync], take_focus), None);
        // Empty / absent property -> None.
        assert_eq!(without_protocol(&[], take_focus), None);
    }

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
        // mh/16 downward nudge.
        let work = (0, 40, 1920, 1080);
        assert_eq!(
            centered_spot(work, 560, 640),
            Some((680, 40 + (1040 - 640) / 2 + 1040 / 16))
        );
        assert_eq!(centered_spot((0, 0, 0, 0), 560, 640), None);
    }

    #[test]
    fn pill_default_matches_original_spot() {
        // Fresh config (no pill_position/pill_padding values set): the pill
        // must land exactly where the fixed bottom-center ~110-up math put it.
        let work = (0, 40, 1920, 1080);
        let (top, pad) = resolve_pill_placement("bottom", "110");
        assert_eq!((top, pad), (false, 110));
        assert_eq!(
            pill_spot(work, 300, 88, top, pad),
            Some((810, 40 + 1040 - 88 - 110)),
            "default placement must be byte-identical to the original"
        );
    }

    #[test]
    fn pill_spot_docks_to_either_edge() {
        // work area (0,40)-(1920,1080): mh = 1040
        let work = (0, 40, 1920, 1080);
        // top, padding P: pill top edge P px below the work-area top
        assert_eq!(pill_spot(work, 300, 88, true, 24), Some((810, 40 + 24)));
        // bottom, padding P: pill bottom edge P px above the work-area bottom
        assert_eq!(
            pill_spot(work, 300, 88, false, 24),
            Some((810, 1080 - 88 - 24))
        );
        // padding 0 hugs the edge exactly
        assert_eq!(pill_spot(work, 300, 88, true, 0), Some((810, 40)));
        assert_eq!(pill_spot(work, 300, 88, false, 0), Some((810, 1080 - 88)));
        // secondary monitor offset carries through (work area not at 0,0)
        let second = (1920, 0, 3840, 1080);
        assert_eq!(pill_spot(second, 300, 88, true, 50), Some((2730, 50)));
        // degenerate work area: no spot
        assert_eq!(pill_spot((0, 0, 0, 0), 300, 88, false, 110), None);
    }

    #[test]
    fn pill_padding_clamps_to_half_the_work_area() {
        let work = (0, 40, 1920, 1080); // mh = 1040 -> cap 520
        assert_eq!(
            pill_spot(work, 300, 88, true, 9999),
            Some((810, 40 + 520)),
            "excessive padding stops at half the work-area height"
        );
        assert_eq!(
            pill_spot(work, 300, 88, false, 9999),
            Some((810, 1080 - 88 - 520))
        );
        assert_eq!(
            pill_spot(work, 300, 88, true, -50),
            Some((810, 40)),
            "negative padding behaves as 0"
        );
    }

    #[test]
    fn pill_placement_resolution_defaults_and_garbage() {
        // untouched config -> the historical spot
        assert_eq!(resolve_pill_placement("bottom", "110"), (false, 110));
        // top is case/space-insensitive
        assert_eq!(resolve_pill_placement(" Top ", "24"), (true, 24));
        assert_eq!(resolve_pill_placement("TOP", "0"), (true, 0));
        // anything unrecognized -> bottom (the default), never a crash
        assert_eq!(resolve_pill_placement("middle", "110"), (false, 110));
        assert_eq!(resolve_pill_placement("", "110"), (false, 110));
        // garbage padding -> the 110 default; negatives floor at 0
        assert_eq!(resolve_pill_placement("bottom", "garbage"), (false, 110));
        assert_eq!(resolve_pill_placement("bottom", ""), (false, 110));
        assert_eq!(resolve_pill_placement("top", "-30"), (true, 0));
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
