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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Manager};

use crate::audio::{self, Recording, Take};
use crate::clipboard;
use crate::config::ConfigStore;
use crate::cues;
use crate::gpu::{GpuWorker, READY_TIMEOUT_CACHED, READY_TIMEOUT_DOWNLOAD};
use crate::power;
use crate::store;
use crate::transcribe::{self, Transcriber};

/// How long to leave `gpu_ok` latched false before allowing one re-probe.
/// A GPU load can fail for transient reasons (driver waking the dGPU, a
/// busy GPU); never re-probing would strand the app on CPU until restart.
const GPU_REPROBE_SECS: u64 = 600;

/// The serving engine: an in-process CPU transcriber, or the GPU worker
/// child (never both live at once).
pub struct Engine {
    pub transcriber: Option<Transcriber>,
    pub worker: Option<GpuWorker>,
    pub model_name: String,
    /// "cpu" | "gpu" — which engine actually serves requests right now.
    pub device: String,
}

/// Successful `RecCmd::Start` reply: (device name, rate, live level meter
/// for the pill).
type StartInfo = (String, u32, Arc<audio::LevelMeter>);

enum RecCmd {
    Start {
        mic: String,
        reply: Sender<Result<StartInfo, String>>,
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
    /// Bumping this stops the previous take's level-pusher thread, so a
    /// stale pusher can never drive a newer take's pill.
    level_gen: AtomicU64,
    /// `_cuda_ok`: false once the GPU proves unavailable, to stop retrying.
    gpu_ok: AtomicBool,
    /// `_cuda_probe_ts`: last time `gpu_ok` was reset for a re-probe
    /// (millis since process start via `Instant`).
    gpu_probe: Mutex<Option<std::time::Instant>>,
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
                                let info =
                                    (rec.mic_name().to_string(), rec.rate(), rec.level_meter());
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
                worker: None,
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
            level_gen: AtomicU64::new(0),
            gpu_ok: AtomicBool::new(true),
            gpu_probe: Mutex::new(None),
        }
    }
}

/// `_maybe_reprobe_cuda`: don't latch `gpu_ok` false forever — allow one
/// GPU retry every `GPU_REPROBE_SECS` so a transient failure self-heals
/// without a restart. Only matters when the user wants the GPU at all.
fn maybe_reprobe_gpu(ctx: &AppCtx) {
    if ctx.gpu_ok.load(Ordering::SeqCst) || lock(&ctx.cfg).get("device").to_lowercase() == "cpu" {
        return;
    }
    let mut probe = lock(&ctx.gpu_probe);
    let due = probe.is_none_or(|t| t.elapsed().as_secs() >= GPU_REPROBE_SECS);
    if due {
        *probe = Some(std::time::Instant::now());
        ctx.gpu_ok.store(true, Ordering::SeqCst); // next load really tests it
    }
}

/// `resolve_target`: which device we SHOULD be on right now, honoring
/// config + power + GPU availability. Config keeps the original's "cuda"
/// value name; internally the GPU target is "gpu".
pub fn resolve_target(ctx: &AppCtx) -> &'static str {
    maybe_reprobe_gpu(ctx);
    let dev = lock(&ctx.cfg).get("device").to_lowercase();
    let gpu_ok = ctx.gpu_ok.load(Ordering::SeqCst);
    match dev.as_str() {
        "cpu" => "cpu",
        "cuda" => {
            if gpu_ok {
                "gpu"
            } else {
                "cpu"
            }
        }
        _ => {
            // auto: GPU only when it works AND we're plugged in
            if gpu_ok && power::on_ac_power() {
                "gpu"
            } else {
                "cpu"
            }
        }
    }
}

impl Default for AppCtx {
    fn default() -> Self {
        Self::new()
    }
}

/// `engine_dict`: the chip payload. Reports the ACTUAL device — "GPU"
/// requires the worker child to be alive right now; a silently-dead worker
/// must not show a green GPU chip — and the live power source.
pub fn engine_dict(ctx: &AppCtx) -> serde_json::Value {
    let mut engine = lock(&ctx.engine);
    let model = if engine.model_name.is_empty() {
        lock(&ctx.cfg).get("model")
    } else {
        engine.model_name.clone()
    };
    let gpu = engine.device == "gpu" && engine.worker.as_mut().is_some_and(GpuWorker::alive);
    let device = if gpu { "GPU" } else { "CPU" };
    let power = if power::on_ac_power() {
        "plugged"
    } else {
        "battery"
    };
    json!({ "model": model, "device": device, "power": power })
}

/// Load the engine for `target` into `engine` (held under the engine
/// lock). "gpu" spawns the worker child — this process never touches the
/// GPU (POWER_AND_DGPU.md); any worker failure latches `gpu_ok` false and
/// falls back to CPU, exactly like the original's in-process CUDA failure.
fn load_engine(ctx: &AppCtx, engine: &mut Engine, target: &str) {
    let models_dir = app_dir().join("models");
    let (model_ac, model_battery, compute_type) = {
        let cfg = lock(&ctx.cfg);
        let fallback = cfg.get("model");
        let ac = {
            let m = cfg.get("model_ac");
            if m.is_empty() {
                fallback.clone()
            } else {
                m
            }
        };
        let bat = {
            let m = cfg.get("model_battery");
            if m.is_empty() {
                fallback.clone()
            } else {
                m
            }
        };
        (ac, bat, cfg.get("compute_type"))
    };
    if target == "gpu" {
        eprintln!("Starting GPU worker for '{model_ac}' ...");
        let cached = models_dir
            .join(transcribe::model_file_name(&model_ac, &compute_type))
            .exists();
        let timeout = if cached {
            READY_TIMEOUT_CACHED
        } else {
            READY_TIMEOUT_DOWNLOAD
        };
        match GpuWorker::spawn(&model_ac, &models_dir, &compute_type, "gpu", timeout) {
            Ok(w) => {
                eprintln!("Ready on GPU (worker).");
                engine.transcriber = None;
                engine.worker = Some(w);
                engine.model_name = model_ac;
                engine.device = "gpu".into();
                return;
            }
            Err(e) => {
                eprintln!("GPU worker unavailable ({e}); falling back to CPU.");
                ctx.gpu_ok.store(false, Ordering::SeqCst);
            }
        }
    }
    eprintln!("Loading '{model_battery}' on CPU ...");
    let loaded = transcribe::ensure_model(&models_dir, &model_battery, &compute_type)
        .and_then(|model_path| {
            let vad = transcribe::ensure_vad_model(&models_dir)
                .map_err(|e| {
                    eprintln!("VAD model unavailable ({e}); continuing without VAD");
                    e
                })
                .ok();
            Transcriber::load(&model_path, vad)
        })
        .and_then(|t| t.warm_up().map(|()| t));
    match loaded {
        Ok(t) => {
            engine.transcriber = Some(t);
            engine.model_name = model_battery;
            engine.device = "cpu".into();
            eprintln!("Ready on CPU.");
        }
        Err(e) => {
            // Leave the engine empty -> "Model not ready" on use.
            eprintln!("CPU model load failed: {e}");
            engine.transcriber = None;
            engine.model_name = String::new();
            engine.device = "cpu".into();
        }
    }
}

/// `ensure_device`: swap the serving engine to `target` if needed. Healthy
/// means: for cpu the model is loaded; for gpu the worker child is ALIVE
/// (a silently-crashed worker must not count as "already on gpu" or
/// dictation would dead-end). The outgoing (or dead) worker is killed
/// BEFORE the replacement load — on the AC->battery flip the dGPU should
/// be asleep during the seconds the CPU model spends loading, not after.
pub fn ensure_device(app: &AppHandle, target: &str) {
    let ctx = app.state::<AppCtx>();
    {
        let mut engine = lock(&ctx.engine);
        let healthy = engine.device == target
            && match target {
                "gpu" => engine.worker.as_mut().is_some_and(GpuWorker::alive),
                _ => engine.transcriber.is_some(),
            };
        if healthy {
            return;
        }
        if let Some(mut w) = engine.worker.take() {
            w.stop();
        }
        load_engine(&ctx, &mut engine, target);
        // the in-process CPU model (if any) drops here, freeing its RAM
    }
    push_panel(app, "tiroSetEngine", engine_dict(&ctx));
}

/// Latch the GPU unavailable (dead worker seen by the watcher); the
/// periodic re-probe in `resolve_target` can lift it later.
pub fn latch_gpu_off(ctx: &AppCtx) {
    ctx.gpu_ok.store(false, Ordering::SeqCst);
}

/// Kill the GPU worker synchronously (quit/restart path): its exit is what
/// releases the GPU context, so it must die BEFORE this process goes away.
pub fn stop_worker(app: &AppHandle) {
    let ctx = app.state::<AppCtx>();
    let worker = lock(&ctx.engine).worker.take();
    if let Some(mut w) = worker {
        w.stop();
    }
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

/// Push a live input level (0..1) to the pill's meter. The `&&` guard keeps
/// the eval harmless against a pill build without `pillLevel`.
fn push_pill_level(app: &AppHandle, level: f32) {
    if let Some(w) = app.get_webview_window("pill") {
        let _ = w.eval(format!("window.pillLevel&&window.pillLevel({level:.3})"));
    }
}

/// While a take is recording, poll the level meter at ~15 Hz and feed the
/// pill. Exits when recording stops or a newer take bumps `level_gen`
/// (the generation guard: a stale pusher must never touch a newer pill).
fn start_level_pusher(app: &AppHandle, ctx: &AppCtx, meter: Arc<audio::LevelMeter>) {
    let gen = ctx.level_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(66));
        let ctx = app.state::<AppCtx>();
        if ctx.level_gen.load(Ordering::SeqCst) != gen || !ctx.recording.load(Ordering::SeqCst) {
            return;
        }
        push_pill_level(&app, audio::perceptual_level(meter.take_peak()));
    });
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
    // dock bottom-center after the show settles
    crate::placement::reposition_burst(app, "pill");
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

/// Bring up the engine for the resolved target in the background (startup),
/// then push the engine chip. Failures leave the engine empty -> "Model not
/// ready" on use, like the original.
pub fn boot_engine(app: AppHandle) {
    std::thread::spawn(move || {
        let target = resolve_target(&app.state::<AppCtx>());
        ensure_device(&app, target);
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
        Ok((name, rate, meter)) => {
            *lock(&ctx.active_mic) = name.clone();
            ctx.active_rate.store(rate as u64, Ordering::SeqCst);
            ctx.recording.store(true, Ordering::SeqCst);
            play(ctx, "start");
            show_pill(app, ctx, "recording", None);
            start_level_pusher(app, ctx, meter);
            crate::set_tray_state(app, "recording");
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
        crate::set_tray_state(app, "idle");
        hide_pill(app, ctx);
        play(ctx, "cancel");
        return;
    };
    let rate = take.rate;
    if take.samples.is_empty() || audio::too_short(take.samples.len(), rate) {
        crate::set_tray_state(app, "idle");
        hide_pill(app, ctx);
        play(ctx, "cancel");
        return;
    }
    show_pill(app, ctx, "transcribing", None);
    crate::set_tray_state(app, "transcribing");
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
        // _xscribe_lock (and blocks device swaps mid-take). GPU can afford
        // accuracy (beam 5); CPU stays fast (beam 1).
        let mut engine_flipped = false;
        let (verbatim, model_name, device) = {
            let mut engine = lock(&ctx.engine);
            if engine.device == "gpu" {
                let result = engine
                    .worker
                    .as_mut()
                    .ok_or_else(|| "GPU worker already gone".to_string())
                    .and_then(|w| w.transcribe(&audio16, 5, vocab.as_deref()));
                match result {
                    Ok(text) => (text, engine.model_name.clone(), "gpu".to_string()),
                    Err(e) => {
                        // The worker crashed / timed out MID-TAKE. The take
                        // must not be lost: kill the worker, latch the GPU
                        // off (the periodic re-probe allows a respawn
                        // later), load the battery model in-process, and
                        // transcribe the SAME audio on CPU right here.
                        eprintln!(
                            "gpu-worker: request failed ({e}); \
                             falling back to in-process CPU for this take"
                        );
                        ctx.gpu_ok.store(false, Ordering::SeqCst);
                        if let Some(mut w) = engine.worker.take() {
                            w.stop();
                        }
                        load_engine(&ctx, &mut engine, "cpu");
                        engine_flipped = true;
                        let Some(t) = engine.transcriber.as_ref() else {
                            eprintln!("CPU fallback load failed too");
                            return Outcome::Error;
                        };
                        match t.transcribe(&audio16, 1, vocab.as_deref()) {
                            Ok(text) => (text, engine.model_name.clone(), "cpu".to_string()),
                            Err(e) => {
                                eprintln!("transcribe failed: {e}");
                                return Outcome::Error;
                            }
                        }
                    }
                }
            } else {
                let Some(transcriber) = engine.transcriber.as_ref() else {
                    eprintln!("transcribe skipped: model not ready");
                    return Outcome::Model;
                };
                match transcriber.transcribe(&audio16, 1, vocab.as_deref()) {
                    Ok(text) => (text, engine.model_name.clone(), "cpu".to_string()),
                    Err(e) => {
                        eprintln!("transcribe failed: {e}");
                        return Outcome::Error;
                    }
                }
            }
        };
        if engine_flipped {
            // chip: GPU -> CPU, immediately (after releasing the lock)
            push_panel(&app, "tiroSetEngine", engine_dict(&ctx));
        }
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
    if !stale {
        // a newer recording owns the tray, otherwise this take is over
        crate::set_tray_state(app, "idle");
    }

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
        crate::set_tray_state(app, "idle");
        hide_pill(app, &ctx);
        play(&ctx, "cancel");
        eprintln!("transcription cancelled");
        return;
    }
    ctx.recording.store(false, Ordering::SeqCst);
    let _ = lock(&ctx.rec_tx).send(RecCmd::Cancel);
    crate::set_tray_state(app, "idle");
    push_panel(app, "tiroSetRecording", json!(false));
    hide_pill(app, &ctx);
    play(&ctx, "cancel");
    eprintln!("recording cancelled");
}
