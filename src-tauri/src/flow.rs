//! The recording state machine (PORTING_NOTES §1), ported from the
//! original's `_state` + toggle/cancel/stop/_finish flow:
//!
//! - toggle starts or stops a take; `busy` makes overlapping presses no-ops
//! - a monotonically increasing session id is captured per take so a stale
//!   transcription finishing late can never clobber a newer one (STATE-1)
//! - cancel during recording discards audio; cancel during transcription
//!   sets a flag the worker checks before copy/log (STATE-2); idle cancel
//!   is a pure no-op
//! - the verbatim text is always logged; cleanup only affects the clipboard
//!   copy, falling back to verbatim when cleanup empties it (STATE-3)
//! - pill states, cues, and panel pushes mirror the original exactly
//!   (done pill hides after 1.1 s, error pills after 2 s)
//!
//! The cpal stream is not Send, so the live `Recording` is owned by a
//! dedicated recorder thread and driven through channel commands.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::audio::{self, Recording, Take};
use crate::clipboard;
use crate::config::ConfigStore;
use crate::cues;
use crate::store;
use crate::transcribe::{self, Transcriber};

/// The serving engine: a CPU transcriber for now; the GPU worker client
/// joins in task 3.4.
pub struct Engine {
    pub transcriber: Option<Transcriber>,
    pub model_name: String,
    /// "cpu" | "gpu" — which engine actually serves requests right now.
    pub device: String,
}

enum RecCmd {
    Start {
        mic: String,
        reply: Sender<Result<(String, u32), String>>,
    },
    Stop {
        reply: Sender<Option<Take>>,
    },
    Cancel,
}

pub struct AppCtx {
    pub cfg: Mutex<ConfigStore>,
    pub engine: Mutex<Engine>,
    rec_tx: Mutex<Sender<RecCmd>>,
    recording: AtomicBool,
    busy: AtomicBool,
    session: AtomicU64,
    cancel_xscribe: AtomicBool,
    xscribing: AtomicBool,
    active_mic: Mutex<String>,
    active_rate: AtomicU64,
    /// Bumping this cancels every pending pill hide timer.
    pill_gen: AtomicU64,
}

/// The app's working directory (config.ini, models/, vocab.txt live here,
/// like the original's APP_DIR).
pub fn app_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub(crate) fn lock<'a, T>(m: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl AppCtx {
    pub fn new() -> Self {
        let (tx, rx) = channel::<RecCmd>();
        std::thread::spawn(move || {
            let mut current: Option<Recording> = None;
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    RecCmd::Start { mic, reply } => {
                        let result = Recording::start(&mic)
                            .map(|rec| {
                                let info = (rec.mic_name().to_string(), rec.rate());
                                current = Some(rec);
                                info
                            })
                            .map_err(|e| e.to_string());
                        let _ = reply.send(result);
                    }
                    RecCmd::Stop { reply } => {
                        let _ = reply.send(current.take().map(Recording::stop));
                    }
                    RecCmd::Cancel => {
                        if let Some(rec) = current.take() {
                            drop(rec.stop());
                        }
                    }
                }
            }
        });
        let dir = app_dir();
        Self {
            cfg: Mutex::new(ConfigStore::load(dir.join("config.ini"), &dir)),
            engine: Mutex::new(Engine {
                transcriber: None,
                model_name: String::new(),
                device: "cpu".into(),
            }),
            rec_tx: Mutex::new(tx),
            recording: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            session: AtomicU64::new(0),
            cancel_xscribe: AtomicBool::new(false),
            xscribing: AtomicBool::new(false),
            active_mic: Mutex::new(String::new()),
            active_rate: AtomicU64::new(audio::SAMPLE_RATE as u64),
            pill_gen: AtomicU64::new(0),
        }
    }
}

impl Default for AppCtx {
    fn default() -> Self {
        Self::new()
    }
}

/// `engine_dict`: the chip payload. Reports the ACTUAL device; power
/// detection lands in 3.3, until then the battery-safe default.
pub fn engine_dict(ctx: &AppCtx) -> serde_json::Value {
    let engine = lock(&ctx.engine);
    let model = if engine.model_name.is_empty() {
        lock(&ctx.cfg).get("model")
    } else {
        engine.model_name.clone()
    };
    let device = if engine.device == "gpu" { "GPU" } else { "CPU" };
    json!({ "model": model, "device": device, "power": "battery" })
}

/// Call `window.<fn>(<json>)` in the panel — the original's `evaluate_js`.
pub fn push_panel(app: &AppHandle, func: &str, payload: serde_json::Value) {
    if let Some(w) = app.get_webview_window("panel") {
        let _ = w.eval(format!("window.{func}({payload})"));
    }
}

/// Call `window.pillSet(state, payload)` in the pill window.
fn push_pill(app: &AppHandle, state: &str, payload: Option<&str>) {
    if let Some(w) = app.get_webview_window("pill") {
        let _ = w.eval(format!(
            "window.pillSet({}, {})",
            json!(state),
            json!(payload)
        ));
    }
}

/// Show the pill window in the given state, cancelling any pending hide
/// timer first (a fresh state must not be hidden by a stale timer).
fn show_pill(app: &AppHandle, ctx: &AppCtx, state: &str, payload: Option<&str>) {
    if !lock(&ctx.cfg).get_bool("pill") {
        return;
    }
    ctx.pill_gen.fetch_add(1, Ordering::SeqCst);
    // GTK window ops must run on the main thread (callers include hotkey
    // dispatch and transcription workers).
    let a = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = a.get_webview_window("pill") {
            let _ = w.show();
        }
    });
    push_pill(app, state, payload);
    // bottom-center placement joins in task 2.4
}

/// Play the pill's exit animation, then hide the window shortly after
/// (skipped if a newer state arrived meanwhile).
fn hide_pill(app: &AppHandle, ctx: &AppCtx) {
    push_pill(app, "off", None);
    let gen = ctx.pill_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(320));
        let ctx = app.state::<AppCtx>();
        if ctx.pill_gen.load(Ordering::SeqCst) == gen {
            let a = app.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(w) = a.get_webview_window("pill") {
                    let _ = w.hide();
                }
            });
        }
    });
}

/// Arm a delayed hide (done: 1.1 s, error: 2 s), replacing any pending one.
fn arm_pill_hide(app: &AppHandle, ctx: &AppCtx, delay_ms: u64) {
    let gen = ctx.pill_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(delay_ms));
        let ctx = app.state::<AppCtx>();
        if ctx.pill_gen.load(Ordering::SeqCst) == gen {
            hide_pill(&app, &ctx);
        }
    });
}

fn play(ctx: &AppCtx, name: &str) {
    cues::play_cue(&lock(&ctx.cfg), name);
}

/// Load the CPU model per config in the background (startup / boot), then
/// push the engine chip. Failures leave the engine empty -> "Model not
/// ready" on use, like the original.
pub fn boot_engine(app: AppHandle) {
    std::thread::spawn(move || {
        let ctx = app.state::<AppCtx>();
        let (model, compute_type) = {
            let cfg = lock(&ctx.cfg);
            let m = cfg.get("model_battery");
            let model = if m.is_empty() { cfg.get("model") } else { m };
            (model, cfg.get("compute_type"))
        };
        eprintln!("Loading '{model}' on CPU ...");
        let models_dir = app_dir().join("models");
        let model_path = match transcribe::ensure_model(&models_dir, &model, &compute_type) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("model download failed: {e}");
                return;
            }
        };
        let vad_path = match transcribe::ensure_vad_model(&models_dir) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("VAD model unavailable ({e}); continuing without VAD");
                None
            }
        };
        let transcriber = match Transcriber::load(&model_path, vad_path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("model load failed: {e}");
                return;
            }
        };
        if let Err(e) = transcriber.warm_up() {
            eprintln!("warm-up failed: {e}");
            return;
        }
        {
            let mut engine = lock(&ctx.engine);
            engine.transcriber = Some(transcriber);
            engine.model_name = model.clone();
            engine.device = "cpu".into();
        }
        eprintln!("Ready on CPU.");
        push_panel(&app, "tiroSetEngine", engine_dict(&ctx));
    });
}

/// `toggle_dictation`: start when idle, stop-and-transcribe when recording.
pub fn toggle_record(app: &AppHandle) {
    let ctx = app.state::<AppCtx>();
    if ctx.busy.load(Ordering::SeqCst) {
        return;
    }
    if !ctx.recording.load(Ordering::SeqCst) {
        start_recording(app, &ctx);
        return;
    }
    if ctx
        .busy
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    stop_recording(app, &ctx);
    ctx.busy.store(false, Ordering::SeqCst);
}

fn start_recording(app: &AppHandle, ctx: &AppCtx) {
    let mic = lock(&ctx.cfg).get("mic_name");
    let (reply_tx, reply_rx) = channel();
    let _ = lock(&ctx.rec_tx).send(RecCmd::Start {
        mic,
        reply: reply_tx,
    });
    let result = reply_rx
        .recv()
        .unwrap_or_else(|_| Err("recorder thread unavailable".to_string()));
    match result {
        Ok((name, rate)) => {
            *lock(&ctx.active_mic) = name.clone();
            ctx.active_rate.store(rate as u64, Ordering::SeqCst);
            ctx.recording.store(true, Ordering::SeqCst);
            play(ctx, "start");
            show_pill(app, ctx, "recording", None);
            push_panel(app, "tiroSetRecording", json!(true));
            eprintln!("● Recording on '{name}' @ {rate} Hz");
        }
        Err(err) => {
            // ERRORS-1: a total mic-open failure is a real error — surface a
            // distinct "No microphone" pill and auto-hide it after ~2 s.
            eprintln!("ERROR opening mic: {err}");
            play(ctx, "error");
            show_pill(app, ctx, "error", Some("No microphone"));
            arm_pill_hide(app, ctx, 2000);
        }
    }
}

fn stop_recording(app: &AppHandle, ctx: &AppCtx) {
    ctx.recording.store(false, Ordering::SeqCst);
    push_panel(app, "tiroSetRecording", json!(false));
    let (reply_tx, reply_rx) = channel();
    let _ = lock(&ctx.rec_tx).send(RecCmd::Stop { reply: reply_tx });
    let take = reply_rx.recv().ok().flatten();
    play(ctx, "stop");
    let Some(take) = take else {
        hide_pill(app, ctx);
        play(ctx, "cancel");
        return;
    };
    let rate = take.rate;
    if take.samples.is_empty() || audio::too_short(take.samples.len(), rate) {
        hide_pill(app, ctx);
        play(ctx, "cancel");
        return;
    }
    show_pill(app, ctx, "transcribing", None);
    let secs = take.samples.len() as f64 / rate as f64;
    let mic = lock(&ctx.active_mic).clone();
    // Fresh session id for this utterance; clear any leftover cancel flag.
    let session = ctx.session.fetch_add(1, Ordering::SeqCst) + 1;
    ctx.cancel_xscribe.store(false, Ordering::SeqCst);
    ctx.xscribing.store(true, Ordering::SeqCst);
    let app = app.clone();
    std::thread::spawn(move || transcribe_worker(app, take, secs, mic, session));
}

enum Outcome {
    Done,
    Empty,
    Error,
    Model,
    CopyFail,
    Noop,
}

/// Transcribe off the command thread, then copy + log. An error can never
/// skip `finish` — the pill must never wedge on "transcribing".
fn transcribe_worker(app: AppHandle, take: Take, secs: f64, mic: String, session: u64) {
    let ctx = app.state::<AppCtx>();
    let mut clean: Option<String> = None;
    let mut rec: Option<store::Rec> = None;
    let mut is_vault = true;
    let outcome = (|| {
        let audio16 = audio::resample_to_16k(&take.samples, take.rate);
        let (cleanup_mode, vocab) = {
            let cfg = lock(&ctx.cfg);
            (
                cfg.get("clipboard_cleanup"),
                transcribe::get_vocab_prompt(&app_dir(), &cfg),
            )
        };
        // The engine lock serializes transcription like the original's
        // _xscribe_lock (and blocks device swaps mid-take).
        let (verbatim, model_name, device) = {
            let engine = lock(&ctx.engine);
            let Some(transcriber) = engine.transcriber.as_ref() else {
                eprintln!("transcribe skipped: model not ready");
                return Outcome::Model;
            };
            let beam = if engine.device == "gpu" { 5 } else { 1 };
            match transcriber.transcribe(&audio16, beam, vocab.as_deref()) {
                Ok(text) => (text, engine.model_name.clone(), engine.device.clone()),
                Err(e) => {
                    eprintln!("transcribe failed: {e}");
                    return Outcome::Error;
                }
            }
        };
        // STATE-2: a cancel issued during transcription aborts before copy/write.
        if ctx.cancel_xscribe.load(Ordering::SeqCst)
            || ctx.session.load(Ordering::SeqCst) != session
        {
            eprintln!("transcription discarded (cancelled / superseded)");
            return Outcome::Noop;
        }
        if verbatim.is_empty() {
            return Outcome::Empty;
        }
        // STATE-3: never copy "" — all-fillers falls back to verbatim.
        let text = clipboard::clipboard_text(&verbatim, &cleanup_mode);
        if let Err(e) = clipboard::copy(&text) {
            eprintln!("clipboard copy failed: {e}");
            return Outcome::CopyFail;
        }
        clean = Some(text.clone());
        let cfg = lock(&ctx.cfg);
        let model_for_log = if model_name.is_empty() {
            cfg.get("model")
        } else {
            model_name
        };
        match store::write_log(&cfg, &verbatim, &text, &mic, &model_for_log, &device, secs) {
            Ok((vault, r)) => {
                is_vault = vault;
                rec = Some(r);
            }
            Err(e) => {
                // clipboard already has the text; still finish "done"
                eprintln!("log write failed (unexpected): {e}");
            }
        }
        Outcome::Done
    })();
    // Only clear the in-flight flag if we are still the current session.
    if ctx.session.load(Ordering::SeqCst) == session {
        ctx.xscribing.store(false, Ordering::SeqCst);
    }
    finish(&app, &ctx, outcome, clean, rec, is_vault, session);
}

/// `_finish`: resolve pill/cue/panel after a take, never clobbering a newer
/// recording's pill (STATE-1).
fn finish(
    app: &AppHandle,
    ctx: &AppCtx,
    outcome: Outcome,
    clean: Option<String>,
    rec: Option<store::Rec>,
    is_vault: bool,
    session: u64,
) {
    if matches!(outcome, Outcome::Noop) {
        return;
    }
    let stale =
        ctx.recording.load(Ordering::SeqCst) || ctx.session.load(Ordering::SeqCst) != session;

    if let (Outcome::Done, Some(clean)) = (&outcome, &clean) {
        play(ctx, "done");
        eprintln!("✓ Copied: {clean}");
        {
            let cfg = lock(&ctx.cfg);
            if let Some(rec) = &rec {
                push_panel(app, "tiroAddEntry", store::entry_from_rec(rec, &cfg));
            }
            let path = store::log_dir(&cfg)
                .map(|(p, _)| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            push_panel(
                app,
                "tiroSetStorage",
                json!({ "fallback": !is_vault, "path": path }),
            );
        }
        if stale {
            return; // a newer recording owns the pill now
        }
        show_pill(app, ctx, "done", None);
        arm_pill_hide(app, ctx, 1100);
        return;
    }

    if stale {
        return;
    }

    match outcome {
        Outcome::Error => {
            play(ctx, "error");
            eprintln!("(transcription error)");
            show_pill(app, ctx, "error", Some("Transcription failed"));
            arm_pill_hide(app, ctx, 2000);
        }
        Outcome::Model => {
            play(ctx, "error");
            eprintln!("(model not ready)");
            show_pill(app, ctx, "error", Some("Model not ready"));
            arm_pill_hide(app, ctx, 2000);
        }
        Outcome::CopyFail => {
            play(ctx, "error");
            eprintln!("(clipboard copy failed)");
            show_pill(app, ctx, "error", Some("Copy failed"));
            arm_pill_hide(app, ctx, 2000);
        }
        _ => {
            play(ctx, "cancel");
            eprintln!("(no speech detected)");
            hide_pill(app, ctx);
        }
    }
}

/// `cancel_dictation`: discard the current recording, or flag an in-flight
/// transcription to abort; idle press is a no-op.
pub fn cancel_record(app: &AppHandle) {
    let ctx = app.state::<AppCtx>();
    if ctx.busy.load(Ordering::SeqCst) {
        return;
    }
    if !ctx.recording.load(Ordering::SeqCst) {
        if !ctx.xscribing.load(Ordering::SeqCst) {
            return;
        }
        ctx.cancel_xscribe.store(true, Ordering::SeqCst);
        hide_pill(app, &ctx);
        play(&ctx, "cancel");
        eprintln!("transcription cancelled");
        return;
    }
    ctx.recording.store(false, Ordering::SeqCst);
    let _ = lock(&ctx.rec_tx).send(RecCmd::Cancel);
    push_panel(app, "tiroSetRecording", json!(false));
    hide_pill(app, &ctx);
    play(&ctx, "cancel");
    eprintln!("recording cancelled");
}
