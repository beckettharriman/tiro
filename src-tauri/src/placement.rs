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
    /// Last known on-screen position of the panel GLASS — the compact
    /// 400x560 visible surface, physical px. On Windows this equals the
    /// window's outer origin (the window always matches the glass); on
    /// Linux the window is permanently at the expanded footprint and the
    /// glass hugs its right edge, so window x = glass x − the compact
    /// offset (see `glass_offset_x`). Stored as the glass rect so configs
    /// written by the old 400-wide-window builds keep rendering the glass
    /// in exactly the same screen spot. Seeded from config at startup,
    /// refreshed by every Moved event while the panel is visible, and
    /// captured right before a hide.
    panel_pos: Mutex<Option<(i32, i32)>>,
    /// Debounce generation for persisting `panel_pos` to config.
    save_gen: AtomicU64,
    /// One-time guard for `init_panel_tracking`.
    tracking: AtomicBool,
    /// Whether the UI is currently in the expanded (advanced) state. On
    /// Linux this drives the input shape (the window itself never
    /// resizes); on Windows it is what the native resize is coming FROM,
    /// owned here because geometry queries on an unmapped window are
    /// unreliable.
    expanded: AtomicBool,
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
                    // window outer x -> glass x (panel_pos stores the glass
                    // rect; on Linux the compact glass sits `off` px in)
                    let off = compact_glass_offset_x(win.scale_factor().unwrap_or(1.0));
                    note_panel_pos(&app, p.x + off, p.y, SAVE_DEBOUNCE_MS);
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
            let off = compact_glass_offset_x(w.scale_factor().unwrap_or(1.0));
            note_panel_pos(app, p.x + off, p.y, 0);
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
///
/// The panel places by its GLASS rect, never the window rect: on Linux
/// the window is permanently at the expanded footprint with the compact
/// glass hugging its right edge, so the window's left edge may
/// legitimately sit outside the work area (glass near the screen's left
/// edge) — on-screen validity and first-run centering both use the glass.
fn place_window(app: &AppHandle, label: &str) {
    let Some(w) = app.get_webview_window(label) else {
        return;
    };
    if label == "panel" {
        let scale = w.scale_factor().unwrap_or(1.0);
        let off = compact_glass_offset_x(scale);
        let (gw, gh) = compact_glass_size(scale);
        let remembered = *lock(&state(app).panel_pos);
        if let Some((gx, gy)) = remembered {
            if pos_on_screen(app, gx, gy, gw, gh) {
                let _ = w.set_position(PhysicalPosition::new(gx - off, gy));
                return;
            }
        }
        let Some(work) = active_work_area(app) else {
            return;
        };
        // First run (or the remembered spot's screen is gone): center the
        // GLASS, then push the window left of it by the compact offset.
        let Some((gx, gy)) = centered_spot(work, gw, gh) else {
            return;
        };
        let _ = w.set_position(PhysicalPosition::new(gx - off, gy));
        // The centered glass spot becomes the remembered one (in memory;
        // the Moved event it triggers handles persistence).
        *lock(&state(app).panel_pos) = Some((gx, gy));
        return;
    }
    let Ok(size) = w.outer_size() else {
        return;
    };
    let (ww, wh) = (size.width as i32, size.height as i32);
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

/// Physical-px x offset from the panel WINDOW's left edge to the COMPACT
/// glass left edge. With the fixed expanded footprint (Linux) the glass
/// hugs the window's right edge, so the compact glass sits
/// (expanded − compact) logical px in; where the window still resizes
/// natively to match the glass (Windows) the offset is zero. One rounding
/// of the whole offset, so glass↔window conversions round-trip exactly at
/// any fractional scale. Pure, extracted for tests.
pub(crate) fn glass_offset_x(fixed_footprint: bool, scale: f64) -> i32 {
    if !fixed_footprint {
        return 0;
    }
    ((f64::from(PANEL_W_EXPANDED) - f64::from(PANEL_W_COMPACT)) * scale).round() as i32
}

/// This build's compact-glass offset: the footprint is fixed on Linux only
/// (X11's non-atomic move+resize is the reason — see `set_panel_expanded`).
fn compact_glass_offset_x(scale: f64) -> i32 {
    glass_offset_x(cfg!(target_os = "linux"), scale)
}

/// The compact glass rect's physical size at `scale`.
fn compact_glass_size(scale: f64) -> (i32, i32) {
    (
        (f64::from(PANEL_W_COMPACT) * scale).round() as i32,
        (f64::from(PANEL_H) * scale).round() as i32,
    )
}

/// The panel's input region for an expand state, in GDK *window*
/// coordinates — logical px, NOT device px: GDK multiplies shape regions
/// by the integer window scale itself when converting them to X
/// rectangles (gtk3 `gdkwindow-x11.c`, `do_shape_combine_region` passes
/// `impl->window_scale` into `_gdk_x11_region_get_xrectangles`), so
/// pre-multiplying here would double-scale on HiDPI. Compact = the right
/// 400 logical px where the glass sits; expanded = the whole window.
/// Pure, extracted for tests.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn panel_input_rect(expanded: bool) -> (i32, i32, i32, i32) {
    if expanded {
        (0, 0, PANEL_W_EXPANDED as i32, PANEL_H as i32)
    } else {
        (
            (PANEL_W_EXPANDED - PANEL_W_COMPACT) as i32,
            0,
            PANEL_W_COMPACT as i32,
            PANEL_H as i32,
        )
    }
}

/// New window x for a width change of `delta` physical px (new minus old)
/// that keeps the RIGHT edge fixed (the design's expand anchor is the
/// top-right corner), clamped so the whole surface stays inside the work
/// area — the left edge wins when the area is narrower than the window.
/// Physical px. Pure, extracted for tests. Windows uses it with the real
/// resize delta; Linux uses it with delta 0 as a pure work-area clamp on
/// expand (see `clamp_expanded_into_work_area`).
fn anchored_right_x(old_x: i32, delta: i32, new_w: i32, work: Option<(i32, i32, i32, i32)>) -> i32 {
    let x = old_x - delta;
    match work {
        Some((wl, _, wr, _)) => x.min(wr - new_w).max(wl),
        None => x,
    }
}

/// Work area for panel expand/collapse geometry: the monitor the PANEL is
/// on first — the cursor may be on another monitor by the time the
/// deferred collapse resize fires 560 ms after the click — falling back
/// to the cursor/primary chain only when the panel's monitor is unknown
/// (e.g. the window is unmapped).
fn panel_work_area(app: &AppHandle) -> Option<(i32, i32, i32, i32)> {
    let monitor = app
        .get_webview_window("panel")
        .and_then(|w| w.current_monitor().ok().flatten());
    match monitor {
        Some(m) => {
            let wa = m.work_area();
            Some((
                wa.position.x,
                wa.position.y,
                wa.position.x + wa.size.width as i32,
                wa.position.y + wa.size.height as i32,
            ))
        }
        None => active_work_area(app),
    }
}

/// Expand/collapse notification from the UI (the `set_expanded` invoke).
///
/// Linux: the window NEVER resizes here — it is created at the expanded
/// footprint (tauri.conf.json) and stays there. X11 has no atomic
/// move+resize: tao/GTK applies `gtk_window_move` immediately but
/// `gtk_window_resize` waits for the next layout tick, so the old
/// resize+reanchor choreography produced two separate configures and the
/// right-hugging glass visibly teleported ~a glass-width sideways for a
/// frame or two whenever the compositor sampled between them. With the
/// fixed footprint the whole expand/collapse is the CSS width transition
/// of the glass inside the transparent window; the native work left is
/// retargeting the input shape so the transparent left margin never eats
/// clicks while compact (see `panel_input_fixup`), plus — on expand only —
/// a lone atomic move when the glass was parked close enough to the work
/// area's left edge that the expanded surface would not fit (see
/// `clamp_expanded_into_work_area`).
///
/// Windows: keeps the original native resize path — instant resize with
/// the min==max constraints moved direction-aware, re-anchored so the
/// TOP-RIGHT corner stays put. DWM applies position+size in one
/// SetWindowPos, so the Linux failure mode does not exist there, and a
/// window that always matches the glass needs no input-shape counterpart
/// (per-pixel hit-testing on Win32 would be its own project).
pub fn set_panel_expanded(app: &AppHandle, on: bool) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        #[cfg(target_os = "linux")]
        set_panel_expanded_linux(&app, on);
        #[cfg(windows)]
        set_panel_expanded_windows(&app, on);
    });
}

/// Linux body of `set_panel_expanded`: bookkeeping, the work-area clamp,
/// and the input shape. Main thread only.
#[cfg(target_os = "linux")]
fn set_panel_expanded_linux(app: &AppHandle, on: bool) {
    let was = state(app).expanded.swap(on, Ordering::SeqCst);
    if on && !was {
        clamp_expanded_into_work_area(app);
    }
    apply_panel_input_shape(app);
}

/// Expanding with the compact glass parked within a glass-width of the
/// work area's left edge would push the advanced UI (the sidebar first)
/// off-screen: the expanded glass IS the whole fixed-footprint window,
/// whose left edge legitimately sits outside the work area while
/// compact. Slide the window right until it fits — the same
/// `anchored_right_x` clamp the Windows resize path applies, with a
/// zero delta ("left edge wins" on a too-narrow area). A lone move is a
/// single X configure — atomic, unlike the move+resize pair this rework
/// eliminated — so it cannot reintroduce the sideways flash.
///
/// The collapse deliberately does NOT slide back, matching the original
/// native path: there the collapse re-anchored the right edge of
/// wherever the (possibly clamped) expanded window ended up, so after
/// an expand-clamp cycle the compact glass keeps the new right edge.
/// Here the window simply never moves on collapse, which lands the
/// glass in exactly that spot; `panel_pos` is updated at the slide so
/// config and the reposition burst agree.
#[cfg(target_os = "linux")]
fn clamp_expanded_into_work_area(app: &AppHandle) {
    let Some(w) = app.get_webview_window("panel") else {
        return;
    };
    let Some(work) = panel_work_area(app) else {
        return;
    };
    let scale = w.scale_factor().unwrap_or(1.0);
    let phys_w = (f64::from(PANEL_W_EXPANDED) * scale).round() as i32;
    let off = compact_glass_offset_x(scale);
    if w.is_visible().unwrap_or(false) {
        if let Ok(pos) = w.outer_position() {
            let x = anchored_right_x(pos.x, 0, phys_w, Some(work));
            if x != pos.x {
                let _ = w.set_position(PhysicalPosition::new(x, pos.y));
                // Update the remembered GLASS spot now, not via the async
                // Moved event, so a racing reposition burst can't restore
                // the stale x (same reasoning as the Windows path).
                *lock(&state(app).panel_pos) = Some((x + off, pos.y));
            }
        }
    } else {
        // Hidden (an expand invoke racing a hotkey hide): geometry
        // queries on an unmapped window are unreliable — reconcile from
        // the remembered glass spot and persist, like the Windows
        // hidden-path does.
        let remembered = *lock(&state(app).panel_pos);
        if let Some((gx, gy)) = remembered {
            let wx = gx - off;
            let x = anchored_right_x(wx, 0, phys_w, Some(work));
            if x != wx {
                note_panel_pos(app, x + off, gy, 0);
            }
        }
    }
}

/// Windows body of `set_panel_expanded`: the original native resize.
/// The resize is deliberately INSTANT in both directions; the 520 ms
/// motion the eye tracks is the CSS width transition on the glass, which
/// app.js orders around this call so the window never moves mid-animation.
/// The window is borderless and pinned by min==max size constraints (that
/// is what keeps a `resizable: true` frameless window fixed); the
/// constraints move with the size, direction-aware so min never exceeds
/// max in between. Main thread only.
#[cfg(windows)]
fn set_panel_expanded_windows(app: &AppHandle, on: bool) {
    let Some(w) = app.get_webview_window("panel") else {
        return;
    };
    let was = state(app).expanded.swap(on, Ordering::SeqCst);
    let width = if on {
        PANEL_W_EXPANDED
    } else {
        PANEL_W_COMPACT
    };
    let old_width = if was {
        PANEL_W_EXPANDED
    } else {
        PANEL_W_COMPACT
    };
    let size = tauri::LogicalSize::new(width, PANEL_H);
    if on {
        let _ = w.set_max_size(Some(size));
        let _ = w.set_min_size(Some(size));
    } else {
        let _ = w.set_min_size(Some(size));
        let _ = w.set_max_size(Some(size));
    }
    let _ = w.set_size(size);
    if was == on {
        return; // idempotent re-apply: the width did not change
    }
    let scale = w.scale_factor().unwrap_or(1.0);
    // One rounding of the DELTA, not of each absolute width: expand and
    // collapse then shift by the same magnitude under fractional
    // scaling, so a full toggle cycle cancels to zero px of creep.
    let delta = ((f64::from(width) - f64::from(old_width)) * scale).round() as i32;
    let phys_w = (f64::from(width) * scale).round() as i32;
    let work = panel_work_area(app);
    if w.is_visible().unwrap_or(false) {
        if let Ok(pos) = w.outer_position() {
            let x = anchored_right_x(pos.x, delta, phys_w, work);
            if x != pos.x {
                let _ = w.set_position(PhysicalPosition::new(x, pos.y));
                // Update the remembered spot NOW, not via the async
                // Moved event: a reposition burst racing this re-applies
                // whatever is remembered, and the stale x would hang the
                // resized surface at the wrong spot. (On Windows the
                // glass rect IS the window rect.)
                *lock(&state(app).panel_pos) = Some((x, pos.y));
            }
        }
    } else {
        // Hidden mid-transition (e.g. the panel hotkey during the
        // 560 ms collapse settle): the window is unmapped, so
        // outer_position() is unreliable and no Moved event will fire
        // to persist anything. Reconcile from the REMEMBERED spot —
        // captured right before the hide, while still mapped — and
        // persist through note_panel_pos, so the panel reopens at the
        // user's right edge and config agrees.
        let remembered = *lock(&state(app).panel_pos);
        if let Some((rx, ry)) = remembered {
            let x = anchored_right_x(rx, delta, phys_w, work);
            if x != rx {
                note_panel_pos(app, x, ry, 0);
            }
        }
    }
}

/// Linux/X11: with the fixed expanded footprint the left 400 logical px
/// of the compact panel window are pure transparency, but they would
/// still hit-test to us and eat clicks meant for whatever is behind. The
/// X Shape extension's INPUT shape fixes that: restrict the input region
/// to the glass rect while compact, the full window while expanded —
/// clicks on the transparent margin fall through to the window below.
///
/// Same access pattern as `pill_no_input_fixup` (gtk_window → widget →
/// gdk window), and like it the shape is re-asserted on `realize` (a
/// realize creates a fresh GdkWindow with no shape) and `map` (every
/// show), plus once immediately for an already-realized window.
#[cfg(target_os = "linux")]
pub fn panel_input_fixup(app: &tauri::App) {
    use gtk::prelude::*;
    let Some(panel) = app.webview_windows().get("panel").cloned() else {
        return;
    };
    let Ok(gtk_win) = panel.gtk_window() else {
        return;
    };
    let h1 = app.handle().clone();
    gtk_win.connect_realize(move |w| {
        apply_input_shape(w.upcast_ref::<gtk::Widget>(), expanded_now(&h1));
    });
    let h2 = app.handle().clone();
    gtk_win.connect_map(move |w| {
        apply_input_shape(w.upcast_ref::<gtk::Widget>(), expanded_now(&h2));
    });
    if gtk_win.is_realized() {
        apply_input_shape(
            gtk_win.upcast_ref::<gtk::Widget>(),
            expanded_now(app.handle()),
        );
    }
}

#[cfg(target_os = "linux")]
fn expanded_now(app: &AppHandle) -> bool {
    state(app).expanded.load(Ordering::SeqCst)
}

/// Retarget the panel's input shape for the current expand state. No-op
/// while the window is unrealized (the realize hook re-asserts). Main
/// thread only.
#[cfg(target_os = "linux")]
fn apply_panel_input_shape(app: &AppHandle) {
    let Some(w) = app.get_webview_window("panel") else {
        return;
    };
    let Ok(gtk_win) = w.gtk_window() else {
        return;
    };
    use gtk::prelude::*;
    apply_input_shape(gtk_win.upcast_ref::<gtk::Widget>(), expanded_now(app));
}

/// Combine the panel's input shape down to `panel_input_rect` — the rect
/// is in GDK window coordinates (logical px); GDK applies the integer
/// HiDPI window scale itself when it hands X the rectangles, so no
/// device-pixel conversion happens here (see `panel_input_rect`).
#[cfg(target_os = "linux")]
fn apply_input_shape(widget: &gtk::Widget, expanded: bool) {
    use gtk::prelude::*;
    let Some(gdk_win) = widget.window() else {
        return;
    };
    let (x, y, w, h) = panel_input_rect(expanded);
    let rect = gtk::cairo::RectangleInt::new(x, y, w, h);
    let region = gtk::cairo::Region::create_rectangle(&rect);
    gdk_win.input_shape_combine_region(&region, 0, 0);
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

/// Pin the webview page zoom to 1.0 on both windows, now and on every
/// monitor-scale change.
///
/// WebView2 keeps a per-profile ZoomFactor that is SEPARATE from the DPI
/// rasterization scale, persists across runs, and is known to end up stuck
/// above 1.0 after DPI transitions (docking to a monitor with a different
/// scale, remote sessions). Nothing in the app ever wants page zoom: the
/// UI is a fixed-pixel design, so a stuck 105% zoom shrinks the CSS
/// viewport below the window and shears content off the left and bottom
/// edges (the glass anchors right/top). Seen live on the Blade 14 at 150%
/// display scale: window, client area and webview bounds all exactly
/// 800x560 logical while the page still clipped — the shrink was the
/// renderer's zoom, nothing native. Re-asserting on ScaleFactorChanged
/// covers the moment the disease is born; asserting at setup cures an
/// already-poisoned profile. On Linux WebKitGTK zoom already defaults to
/// 1.0 and this is a harmless re-assert.
pub fn pin_webview_zoom(app: &tauri::App) {
    for label in ["panel", "pill"] {
        let Some(w) = app.webview_windows().get(label).cloned() else {
            continue;
        };
        let _ = w.set_zoom(1.0);
        let win = w.clone();
        w.on_window_event(move |ev| {
            if matches!(ev, tauri::WindowEvent::ScaleFactorChanged { .. }) {
                let _ = win.set_zoom(1.0);
            }
        });
    }
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
    fn expand_anchors_the_top_right_corner() {
        let work = Some((0, 0, 1920, 1080));
        // 400 -> 800 (delta +400): x drops by the delta, right edge fixed.
        assert_eq!(anchored_right_x(1000, 400, 800, work), 600);
        // collapse (delta -400) restores the exact pre-expand spot
        assert_eq!(anchored_right_x(600, -400, 400, work), 1000);
        // no width change -> same x
        assert_eq!(anchored_right_x(600, 0, 800, work), 600);
    }

    #[test]
    fn expand_clamps_into_the_work_area() {
        let work = Some((0, 0, 1920, 1080));
        // no room to the left: pin to the work-area left edge
        assert_eq!(anchored_right_x(100, 400, 800, work), 0);
        // never past the right edge either
        assert_eq!(anchored_right_x(1900, 400, 800, work), 1120);
        // work area narrower than the panel: left edge wins
        assert_eq!(anchored_right_x(300, 400, 800, Some((0, 0, 600, 400))), 0);
        // secondary monitor offsets carry through
        assert_eq!(
            anchored_right_x(2000, 400, 800, Some((1920, 0, 3840, 1080))),
            1920
        );
        // no monitor info: raw anchor math, no clamp
        assert_eq!(anchored_right_x(50, 400, 800, None), -350);
    }

    #[test]
    fn fractional_scale_cycle_has_no_creep() {
        // 1.25 scale: the shift is one rounding of the DELTA (400 * 1.25),
        // applied with opposite signs — expand + collapse must return to
        // exactly the starting x, whatever the rounding did.
        for scale in [1.25_f64, 1.5, 1.1, 2.0, 1.33] {
            let delta = (400.0 * scale).round() as i32;
            let w800 = (800.0 * scale).round() as i32;
            let w400 = (400.0 * scale).round() as i32;
            let work = Some((0, 0, 4000, 1600));
            let expanded = anchored_right_x(2000, delta, w800, work);
            let back = anchored_right_x(expanded, -delta, w400, work);
            assert_eq!(back, 2000, "creep at scale {scale}");
        }
    }

    #[test]
    fn glass_offset_is_zero_when_window_matches_glass() {
        // Windows: the window resizes natively and always equals the glass.
        for scale in [1.0, 1.25, 1.5, 2.0] {
            assert_eq!(glass_offset_x(false, scale), 0);
        }
    }

    #[test]
    fn glass_offset_rounds_the_whole_offset_once() {
        assert_eq!(glass_offset_x(true, 1.0), 400);
        assert_eq!(glass_offset_x(true, 1.25), 500);
        assert_eq!(glass_offset_x(true, 1.5), 600);
        assert_eq!(glass_offset_x(true, 2.0), 800);
        // fractional scale with an odd rounding still round-trips exactly
        // because save and load use the SAME rounded offset
        assert_eq!(glass_offset_x(true, 1.33), 532);
    }

    #[test]
    fn old_config_positions_render_the_glass_unmoved() {
        // panel_pos written by the old 400-wide-window builds was the
        // compact window's outer origin — which WAS the glass rect. The
        // fixed-footprint window is placed at glass − offset, and the
        // glass renders offset px inside it: same screen spot, any scale,
        // and a drag save (window + offset) reproduces the stored value.
        for scale in [1.0, 1.25, 1.5, 2.0, 1.33] {
            let off = glass_offset_x(true, scale);
            let (gx, gy) = (642, -128); // an old saved panel_pos
            let window_x = gx - off; // load: where the window is placed
            assert_eq!(window_x + off, gx, "glass unmoved at scale {scale}");
            assert_eq!(gy, -128, "y never shifts (only x has an offset)");
            // Moved-event save path: stored value is bit-identical again
            let resaved = window_x + off;
            assert_eq!(resaved, gx, "save round-trip at scale {scale}");
        }
    }

    #[test]
    fn first_run_centers_the_glass_not_the_window() {
        let work = (0, 40, 1920, 1080);
        assert_eq!(compact_glass_size(1.0), (400, 560));
        assert_eq!(compact_glass_size(2.0), (800, 1120));
        let (gw, gh) = compact_glass_size(1.0);
        let (gx, _gy) = centered_spot(work, gw, gh).unwrap();
        // the GLASS centers exactly like the old 400-wide window did …
        assert_eq!(gx, (1920 - 400) / 2);
        // … and the fixed-footprint window sits a full glass-width left
        assert_eq!(gx - glass_offset_x(true, 1.0), gx - 400);
    }

    #[test]
    fn compact_glass_at_screen_left_puts_window_off_area_legally() {
        // Validity is judged by the GLASS rect: a glass hugging the work
        // area's left edge is fine even though the window's x is then a
        // whole glass-width outside the work area.
        let areas = [(0, 0, 1920, 1080)];
        let (gx, gy) = (0, 300);
        assert!(center_on_any(&areas, gx, gy, 400, 560));
        assert_eq!(gx - glass_offset_x(true, 1.0), -400);
    }

    #[test]
    fn fixed_footprint_expand_clamps_into_the_work_area() {
        // Linux fixed footprint: the clamp is anchored_right_x with a zero
        // delta on the WINDOW rect (== the expanded glass rect).
        let work = Some((0, 0, 1920, 1080));
        for scale in [1.0, 2.0] {
            let off = glass_offset_x(true, scale);
            let phys_w = (800.0 * scale).round() as i32;
            // reviewer scenario: compact glass at the work-area left edge
            // (gx=0) -> window spans -off..+off -> expand must slide the
            // window right to 0 so the whole advanced UI (sidebar first)
            // stays on screen; the remembered glass follows the slide.
            let gx = 0;
            let wx = gx - off;
            let big = Some((0, 0, 4000 * scale as i32, 2000));
            let x = anchored_right_x(wx, 0, phys_w, big);
            assert_eq!(x, 0, "slides to the work-area left at scale {scale}");
            assert_eq!(x + off, off, "compact glass lands at +off after the cycle");
            // glass just inside the danger zone (gx < off) still slides
            let wx2 = (off / 2) - off;
            assert_eq!(anchored_right_x(wx2, 0, phys_w, big), 0);
        }
        // expanded surface already fully inside the area: untouched
        assert_eq!(anchored_right_x(500, 0, 800, work), 500);
        assert_eq!(anchored_right_x(0, 0, 800, work), 0);
        // never past the right edge either
        assert_eq!(anchored_right_x(1200, 0, 800, work), 1120);
        // area narrower than the window: left edge wins
        assert_eq!(anchored_right_x(-100, 0, 800, Some((0, 0, 600, 400))), 0);
        // no monitor info: no clamp, no move
        assert_eq!(anchored_right_x(-400, 0, 800, None), -400);
    }

    #[test]
    fn input_region_tracks_the_glass() {
        // compact: only the right 400 logical px (the glass) take input
        assert_eq!(panel_input_rect(false), (400, 0, 400, 560));
        // expanded: the whole window does
        assert_eq!(panel_input_rect(true), (0, 0, 800, 560));
        // The rect is in GDK window coordinates at EVERY scale — GDK
        // multiplies by the integer window scale itself; the device-px
        // region X ends up with is scale × rect. Spelled out for scale 1
        // and 2 so nobody "fixes" the missing multiplication into a
        // double-scale.
        for scale in [1, 2] {
            let (x, y, w, h) = panel_input_rect(false);
            assert_eq!(
                (x * scale, y * scale, w * scale, h * scale),
                (400 * scale, 0, 400 * scale, 560 * scale)
            );
            let (x, y, w, h) = panel_input_rect(true);
            assert_eq!(
                (x * scale, y * scale, w * scale, h * scale),
                (0, 0, 800 * scale, 560 * scale)
            );
        }
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
