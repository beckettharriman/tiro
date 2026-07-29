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
use std::sync::{Arc, Mutex, TryLockError};
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
///
/// LOCK LAW (the tray-Restart freeze of record): `AppCtx::engine` may be
/// held across engine-length work (model load, warm-up, GPU spawn, the
/// GPU-crash CPU reload) by BACKGROUND threads only — and a transcription
/// itself never holds it at all (`transcribe_worker` clones the Arcs under
/// a short lock and runs unlocked). Everything reachable from the main
/// thread (`engine_dict` via the get_state/set_setting IPC, `stop_worker`
/// via the tray) uses `try_lock` + the `EngineStatus` snapshot and returns
/// promptly no matter what the engine is doing.
pub struct Engine {
    pub transcriber: Option<Arc<Transcriber>>,
    pub worker: Option<Arc<GpuWorker>>,
    pub model_name: String,
    /// "cpu" | "gpu" — which engine actually serves requests right now.
    pub device: String,
}

/// Last-known engine-chip state: refreshed under the engine lock at every
/// engine transition and at every take start, served lock-free (well,
/// short-lock) to main-thread readers while the engine lock is busy.
#[derive(Clone, Default)]
pub struct EngineStatus {
    pub model: String,
    /// "cpu" | "gpu" — the ACTUAL device (worker-alive-checked when the
    /// snapshot was taken; design rule 8). For an in-flight take this is
    /// the take-start truth.
    pub device: String,
}

/// Recompute the chip truth from the live engine (caller holds the engine
/// lock) and store it as the last-known snapshot.
fn refresh_status(ctx: &AppCtx, engine: &Engine) -> EngineStatus {
    let gpu = engine.device == "gpu" && engine.worker.as_ref().is_some_and(|w| w.alive());
    let status = EngineStatus {
        model: engine.model_name.clone(),
        device: if gpu { "gpu" } else { "cpu" }.into(),
    };
    *lock(&ctx.status) = status.clone();
    status
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
    /// Last-known chip state for `engine_dict` (see `EngineStatus`).
    /// Always short-held; lock order is engine -> status, never reversed.
    status: Mutex<EngineStatus>,
    /// Serializes transcriptions like the original's `_xscribe_lock`.
    /// Taken ONLY by `transcribe_worker` threads — nothing main-thread-
    /// reachable can ever queue behind a take on this lock.
    xscribe: Mutex<()>,
    /// Set on the tray Quit/Restart path (`stop_worker`): the process is
    /// exiting, so the crash-fallback must not load a CPU model into a
    /// dying process — the mid-take audio is abandoned, per the original's
    /// shutdown contract (PORTING_NOTES §8: never block exit on cleanup).
    shutting_down: AtomicBool,
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
    /// Session id whose completed take should also be pasted at the cursor
    /// (0 = none). Consumed by `finish` ONLY while that session is current —
    /// a superseded take must never paste.
    paste_session: AtomicU64,
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
    /// Whether a take is being recorded right now (read-only view for the
    /// hotkey layer's push-to-talk logic).
    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::SeqCst)
    }

    /// Whether a transcription is in flight (read-only view for the hotkey
    /// layer: a paste press during transcription ARMS the in-flight take
    /// instead of starting a recording, so such a press must not count as a
    /// take-starting press for push-to-talk).
    pub fn is_transcribing(&self) -> bool {
        self.xscribing.load(Ordering::SeqCst)
    }

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
            status: Mutex::new(EngineStatus {
                model: String::new(),
                device: "cpu".into(),
            }),
            xscribe: Mutex::new(()),
            shutting_down: AtomicBool::new(false),
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
            paste_session: AtomicU64::new(0),
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
/// requires the worker child to be alive; a silently-dead worker must not
/// show a green GPU chip — and the live power source.
///
/// NEVER BLOCKS: this is reached from the main thread (get_state /
/// set_setting IPC — the freeze of record wedged the whole GTK loop here,
/// queued behind an in-flight transcription; a minutes-long CPU model
/// load at boot wedged first paint the same way). When the engine lock is
/// free this refreshes and returns the live truth, alive-check included;
/// when a load or take holds it, this returns the last-known snapshot —
/// which for an in-flight take is the take-start truth (design rule 8).
pub fn engine_dict(ctx: &AppCtx) -> serde_json::Value {
    let status = match ctx.engine.try_lock() {
        Ok(engine) => refresh_status(ctx, &engine),
        Err(TryLockError::Poisoned(p)) => refresh_status(ctx, &p.into_inner()),
        Err(TryLockError::WouldBlock) => lock(&ctx.status).clone(),
    };
    let model = if status.model.is_empty() {
        lock(&ctx.cfg).get("model")
    } else {
        status.model
    };
    let device = if status.device == "gpu" { "GPU" } else { "CPU" };
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
                engine.worker = Some(Arc::new(w));
                engine.model_name = model_ac;
                engine.device = "gpu".into();
                refresh_status(ctx, engine);
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
            engine.transcriber = Some(Arc::new(t));
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
    refresh_status(ctx, engine);
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
                "gpu" => engine.worker.as_ref().is_some_and(|w| w.alive()),
                _ => engine.transcriber.is_some(),
            };
        if healthy {
            refresh_status(&ctx, &engine);
            return;
        }
        if let Some(w) = engine.worker.take() {
            // With a request in flight this kills immediately; the take
            // retries on CPU (never-lose-a-take) — and on the AC->battery
            // flip the dGPU no longer has to wait out the take to sleep.
            w.stop();
        }
        // The worker is gone RIGHT NOW: refresh the chip snapshot before
        // the (possibly minutes-long) load, so a mid-load get_state can
        // never claim "GPU" over a dead worker (design rule 8).
        refresh_status(&ctx, &engine);
        load_engine(&ctx, &mut engine, target);
        // The outgoing in-process CPU model (if any) drops here — or, when
        // an in-flight take still holds its Arc, when that take finishes.
    }
    push_panel(app, "tiroSetEngine", engine_dict(&ctx));
}

/// Latch the GPU unavailable (dead worker seen by the watcher); the
/// periodic re-probe in `resolve_target` can lift it later.
pub fn latch_gpu_off(ctx: &AppCtx) {
    ctx.gpu_ok.store(false, Ordering::SeqCst);
}

/// Tear down whatever serves right now — worker stopped (immediately if a
/// request is in flight), CPU model dropped — and refresh the chip
/// snapshot in the same breath: the owner must never see "GPU" while the
/// worker is dead, even for the minutes a follow-up load takes (design
/// rule 8). Clearing `model_name` makes the chip fall back to the
/// configured model — the existing "not ready" presentation — until the
/// reload lands. Callers bring the replacement up via `ensure_device`.
pub fn teardown_engine(ctx: &AppCtx) {
    let mut engine = lock(&ctx.engine);
    if let Some(w) = engine.worker.take() {
        w.stop();
    }
    engine.transcriber = None;
    engine.model_name = String::new();
    refresh_status(ctx, &engine);
}

/// Kill the GPU worker synchronously (quit/restart path): its exit is what
/// releases the GPU context, so it must die BEFORE this process goes away.
///
/// Runs on the MAIN THREAD (tray menu), so it must never wait: `try_lock`
/// — if the engine lock is busy (a model load / GPU spawn / crash-fallback
/// reload) there is no serving worker to take anyway (loads kill the
/// outgoing worker first), and a child mid-spawn is covered by the
/// stdin-EOF orphan backstop when this process exits. `GpuWorker::stop`
/// itself kills immediately when a request is in flight; the mid-transcribe
/// take is abandoned, matching the original's shutdown contract
/// (PORTING_NOTES §8: "never block exit on cleanup").
pub fn stop_worker(app: &AppHandle) {
    let ctx = app.state::<AppCtx>();
    // Mark the shutdown FIRST: the crash-fallback of any take our kill
    // interrupts checks this and abandons the take instead of loading a
    // CPU model into a dying process (PORTING_NOTES §8).
    ctx.shutting_down.store(true, Ordering::SeqCst);
    let worker = match ctx.engine.try_lock() {
        Ok(mut engine) => engine.worker.take(),
        Err(TryLockError::Poisoned(p)) => p.into_inner().worker.take(),
        Err(TryLockError::WouldBlock) => None,
    };
    if let Some(w) = worker {
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
    // dock at the configured edge (default bottom-center) after the show settles
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
    stop_recording(app, &ctx, false);
    ctx.busy.store(false, Ordering::SeqCst);
}

/// The paste hotkey ("put my words here"). The key that FINISHES a take
/// decides its destination, regardless of which key started it:
/// - idle: start a recording, identical to the dictation key (same pill,
///   cues, level meter) — whether it pastes is decided at stop time
/// - mid-recording: stop the take now; when it completes it is copied (as
///   always) AND pasted at the cursor. (The dictation key stopping the same
///   take makes it clipboard-only instead — see `toggle_record`.)
/// - while transcribing: arm the in-flight take to paste on completion
///
/// Like the dictation key, holding it past the push-to-talk threshold
/// records while held; the hotkey layer re-dispatches here on the release,
/// which lands in the mid-recording arm above — stop, transcribe, copy,
/// paste (see `hotkeys::hold_event`).
pub fn paste_take(app: &AppHandle) {
    let ctx = app.state::<AppCtx>();
    if ctx.busy.load(Ordering::SeqCst) {
        return;
    }
    if ctx.recording.load(Ordering::SeqCst) {
        if ctx
            .busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        stop_recording(app, &ctx, true);
        ctx.busy.store(false, Ordering::SeqCst);
        return;
    }
    if ctx.xscribing.load(Ordering::SeqCst) {
        // Arm the in-flight take; `finish` consumes this only if that take's
        // session is still current when it completes.
        ctx.paste_session
            .store(ctx.session.load(Ordering::SeqCst), Ordering::SeqCst);
        eprintln!("paste armed for the in-flight take");
        return;
    }
    // Idle: start a take (paste intent). The stop key decides the
    // destination, so this start is exactly a normal start.
    start_recording(app, &ctx);
}

/// Deliver the synthetic Ctrl+V. Returns false on any failure — the callers
/// fall back to the "On clipboard" pill, never an exception.
///
/// The portal restore token is read and persisted by the injection layer
/// itself, through these closures, under its paste lock: two overlapping
/// finishes must never interleave reads/writes of the single-use token.
fn inject_paste(app: &AppHandle, ctx: &AppCtx) -> bool {
    let _ = app; // signature symmetry; the injection needs no window handle
    let result = crate::inject::paste_at_cursor(
        || lock(&ctx.cfg).get("portal_restore_token"),
        |t| lock(&ctx.cfg).set("portal_restore_token", t),
    );
    match result {
        Ok(()) => true,
        Err(e) => {
            eprintln!("paste injection failed ({e}); text stays on the clipboard");
            false
        }
    }
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

fn stop_recording(app: &AppHandle, ctx: &AppCtx, paste: bool) {
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
    // Arm (paste stop) or disarm (plain stop — a stale armed paste from an
    // earlier, now-superseded take must never leak onto this one).
    ctx.paste_session
        .store(if paste { session } else { 0 }, Ordering::SeqCst);
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

/// What a failed GPU take retries on (never-lose-a-take).
enum Retry {
    Gpu(Arc<GpuWorker>),
    Cpu(Arc<Transcriber>),
}

/// The crash-fallback's decision core, pure so it can be unit-tested (the
/// full interleaving needs live worker child processes, which headless
/// tests cannot spawn). Inputs describe the engine at fallback time:
/// - `ours`: the engine still serves (or has an empty slot for) the very
///   worker whose request just failed,
/// - `replacement_alive`: a DIFFERENT, live worker was installed meanwhile
///   (a concurrent device swap / model change),
/// - `allow_gpu`: this take has not burnt its one GPU retry yet,
/// - `has_transcriber`: a CPU model is loaded.
#[derive(Debug, PartialEq)]
enum FallbackRoute {
    /// Reap the failed/absent worker, load the CPU model, retry there.
    TeardownLoadCpu,
    /// Retry through the live replacement worker.
    ReplacementGpu,
    /// Retry on the already-loaded CPU model.
    ExistingCpu,
    /// A live replacement exists but this take may not use the GPU again,
    /// and no CPU model is loaded: killing a healthy worker to load one
    /// would punish an engine that did nothing wrong — give up (Error is
    /// allowed only when no engine can be obtained; two GPU engines have
    /// already failed this take by then).
    GiveUp,
}

fn fallback_route(
    ours: bool,
    replacement_alive: bool,
    allow_gpu: bool,
    has_transcriber: bool,
) -> FallbackRoute {
    if ours {
        FallbackRoute::TeardownLoadCpu
    } else if replacement_alive && allow_gpu {
        FallbackRoute::ReplacementGpu
    } else if has_transcriber {
        FallbackRoute::ExistingCpu
    } else if replacement_alive {
        FallbackRoute::GiveUp
    } else {
        FallbackRoute::TeardownLoadCpu
    }
}

/// Apply `fallback_route` under the engine lock and hand back the engine
/// this take retries on. Returns `None` only when no engine could be
/// obtained at all (CPU load failed, or `GiveUp`). Holding the engine
/// lock across the CPU load here is safe: nothing main-thread-reachable
/// ever waits on it (LOCK LAW above).
fn fallback_target(
    ctx: &AppCtx,
    failed: Option<&Arc<GpuWorker>>,
    allow_gpu: bool,
    model_name: &mut String,
    flipped: &mut bool,
) -> Option<Retry> {
    let mut engine = lock(&ctx.engine);
    let ours = match (&engine.worker, failed) {
        (Some(cur), Some(f)) => Arc::ptr_eq(cur, f),
        (None, _) => engine.device == "gpu",
        (Some(_), None) => false,
    };
    let replacement_alive = !ours && engine.worker.as_ref().is_some_and(|w| w.alive());
    match fallback_route(
        ours,
        replacement_alive,
        allow_gpu,
        engine.transcriber.is_some(),
    ) {
        FallbackRoute::TeardownLoadCpu => {
            if let Some(w) = engine.worker.take() {
                w.stop();
            }
            // The worker is gone RIGHT NOW — the chip must stop claiming
            // GPU for the whole CPU load that follows (design rule 8).
            refresh_status(ctx, &engine);
            load_engine(ctx, &mut engine, "cpu");
            *flipped = true;
            *model_name = engine.model_name.clone();
            engine.transcriber.clone().map(Retry::Cpu)
        }
        FallbackRoute::ReplacementGpu => {
            *model_name = engine.model_name.clone();
            engine.worker.clone().map(Retry::Gpu)
        }
        FallbackRoute::ExistingCpu => {
            *model_name = engine.model_name.clone();
            engine.transcriber.clone().map(Retry::Cpu)
        }
        FallbackRoute::GiveUp => None,
    }
}

/// Transcribe off the command thread, then copy + log. An error can never
/// skip `finish` — the pill must never wedge on "transcribing": every exit
/// from the closure maps to an `Outcome`, and a panic anywhere inside it
/// (whisper bindings, clipboard, log IO) is caught and treated as an error
/// rather than killing the thread with the pill still showing.
fn transcribe_worker(app: AppHandle, take: Take, secs: f64, mic: String, session: u64) {
    let ctx = app.state::<AppCtx>();
    let mut clean: Option<String> = None;
    let mut rec: Option<store::Rec> = None;
    let mut is_vault = true;
    let worker = std::panic::AssertUnwindSafe(|| {
        // Serialize takes like the original's `_xscribe_lock`. Only sibling
        // transcribe workers ever contend here — the engine lock is NOT held
        // across the whisper call (the freeze of record: a main-thread
        // get_state IPC queued behind it and wedged the whole GTK loop,
        // pill, tray and exit path included).
        let _serial = lock(&ctx.xscribe);
        let audio16 = audio::resample_to_16k(&take.samples, take.rate);
        let (cleanup_mode, vocab) = {
            let cfg = lock(&ctx.cfg);
            (
                cfg.get("clipboard_cleanup"),
                transcribe::get_vocab_prompt(&app_dir(), &cfg),
            )
        };
        let corrections = crate::vocab::read_corrections(&app_dir());
        /// What serves this take, cloned out of the engine under a short
        /// lock; the transcription runs on the Arcs with the lock released.
        enum Serving {
            Gpu(Option<Arc<GpuWorker>>),
            Cpu(Option<Arc<Transcriber>>),
        }
        let (serving, mut model_name) = {
            let engine = lock(&ctx.engine);
            // The chip snapshot becomes this take's start-time truth
            // (design rule 8) for any get_state that lands mid-take.
            refresh_status(&ctx, &engine);
            let serving = if engine.device == "gpu" {
                Serving::Gpu(engine.worker.clone())
            } else {
                Serving::Cpu(engine.transcriber.clone())
            };
            (serving, engine.model_name.clone())
        };
        // GPU can afford accuracy (beam 5); CPU stays fast (beam 1).
        let mut engine_flipped = false;
        let (verbatim, device) = match serving {
            Serving::Cpu(None) => {
                eprintln!("transcribe skipped: model not ready");
                return Outcome::Model;
            }
            Serving::Cpu(Some(t)) => match t.transcribe(&audio16, 1, vocab.as_deref()) {
                Ok(text) => (text, "cpu".to_string()),
                Err(e) => {
                    eprintln!("transcribe failed: {e}");
                    return Outcome::Error;
                }
            },
            Serving::Gpu(gpu) => {
                let result = gpu
                    .as_deref()
                    .ok_or_else(|| "GPU worker already gone".to_string())
                    .and_then(|w| w.transcribe(&audio16, 5, vocab.as_deref()));
                match result {
                    Ok(text) => (text, "gpu".to_string()),
                    Err(e) => {
                        // The worker died MID-TAKE — crashed, timed out, or
                        // deliberately killed (device swap, model change,
                        // shutdown). The take must not be lost: retry the
                        // SAME audio on whatever the engine serves now
                        // (`fallback_target`); at most one retry lands on a
                        // replacement GPU worker, after that it goes CPU.
                        let mut err = e;
                        let mut failed = gpu;
                        let mut allow_gpu = true;
                        loop {
                            if ctx.shutting_down.load(Ordering::SeqCst) {
                                // Exit in progress: do not load models into
                                // a dying process. PORTING_NOTES §8: never
                                // block exit on cleanup — take abandoned.
                                eprintln!(
                                    "gpu-worker: request failed ({err}); \
                                     take abandoned at exit"
                                );
                                return Outcome::Noop;
                            }
                            // Only a real crash latches the GPU off (the
                            // periodic re-probe can lift it later). A
                            // deliberate kill must not cost 10 minutes of
                            // CPU-only service; an unattributable failure
                            // (worker already gone) means a deliberate
                            // taker was involved, so no latch either — the
                            // power watcher still latches a silently-dead
                            // worker it finds on its own.
                            let crashed = failed.as_deref().is_some_and(|w| !w.was_stopped());
                            if crashed {
                                ctx.gpu_ok.store(false, Ordering::SeqCst);
                            }
                            eprintln!(
                                "gpu-worker: request failed ({err}); \
                                 retrying this take on the serving engine"
                            );
                            match fallback_target(
                                &ctx,
                                failed.as_ref(),
                                allow_gpu,
                                &mut model_name,
                                &mut engine_flipped,
                            ) {
                                None => {
                                    eprintln!("no engine available for the retry");
                                    return Outcome::Error;
                                }
                                Some(Retry::Cpu(t)) => {
                                    match t.transcribe(&audio16, 1, vocab.as_deref()) {
                                        Ok(text) => break (text, "cpu".to_string()),
                                        Err(e) => {
                                            eprintln!("transcribe failed: {e}");
                                            return Outcome::Error;
                                        }
                                    }
                                }
                                Some(Retry::Gpu(w)) => {
                                    match w.transcribe(&audio16, 5, vocab.as_deref()) {
                                        Ok(text) => break (text, "gpu".to_string()),
                                        Err(e) => {
                                            // One GPU retry per take; the
                                            // next round goes CPU-ward.
                                            err = e;
                                            failed = Some(w);
                                            allow_gpu = false;
                                        }
                                    }
                                }
                            }
                        }
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
        // Vocabulary corrections apply to the clipboard/clean layer only;
        // the verbatim transcript below is logged unchanged.
        let text = crate::vocab::apply_corrections(
            &clipboard::clipboard_text(&verbatim, &cleanup_mode),
            &corrections,
        );
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
    });
    let outcome = std::panic::catch_unwind(worker).unwrap_or_else(|_| {
        eprintln!("transcribe worker panicked; treating as transcription error");
        Outcome::Error
    });
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
    // Consume the armed paste — for ANY outcome, so a failed take can't leave
    // it lingering — but only while this take's session is still current; a
    // superseded take must never paste (its swap here targets a dead id).
    let wants_paste = !stale && ctx.paste_session.swap(0, Ordering::SeqCst) == session;

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
        if wants_paste && !inject_paste(app, ctx) {
            // Injection failed — the text is safely on the clipboard; say so
            // instead of the normal done pill, then hide on the done timing.
            show_pill(app, ctx, "error", Some("On clipboard"));
            arm_pill_hide(app, ctx, 1100);
            return;
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
        ctx.paste_session.store(0, Ordering::SeqCst); // cancelled take never pastes
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

#[cfg(test)]
mod lock_tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Instant;

    /// Regression guard for the tray-Restart freeze: `engine_dict` is
    /// reached from the main thread (get_state IPC) and must return
    /// promptly — with the last-known snapshot — while the engine lock is
    /// held by engine-length work (a transcription-era hold, a CPU model
    /// load, a GPU spawn). On the old shape `engine_dict` blocked on
    /// `lock(&ctx.engine)` and this test fails on the elapsed-time assert.
    #[test]
    fn engine_dict_returns_snapshot_while_engine_lock_is_held() {
        let ctx = AppCtx::new();
        {
            let mut engine = lock(&ctx.engine);
            engine.model_name = "large-v3-turbo".into();
            engine.device = "cpu".into();
            refresh_status(&ctx, &engine);
        }
        let ctx = &ctx;
        std::thread::scope(|s| {
            let (locked_tx, locked_rx) = channel();
            s.spawn(move || {
                // Simulate a load/take holding the engine lock for 3 s.
                let _engine = lock(&ctx.engine);
                locked_tx.send(()).expect("test channel");
                std::thread::sleep(Duration::from_secs(3));
            });
            locked_rx.recv().expect("holder thread locked");
            let t0 = Instant::now();
            let dict = engine_dict(ctx);
            assert!(
                t0.elapsed() < Duration::from_secs(2),
                "engine_dict blocked on the busy engine lock ({}s)",
                t0.elapsed().as_secs_f64()
            );
            assert_eq!(dict["model"], "large-v3-turbo", "snapshot model served");
            assert_eq!(dict["device"], "CPU", "snapshot device served");
        });
    }

    /// D1 decision table for the crash-fallback. The full interleaving
    /// (kill a live worker mid-request, install a replacement, drive the
    /// fallback) needs real worker child processes, which a headless test
    /// cannot spawn — so the extracted pure core carries the coverage.
    #[test]
    fn fallback_route_never_discards_a_recoverable_take() {
        use FallbackRoute::*;
        // Engine still serves the failed worker (or its empty slot):
        // reap + load CPU, regardless of anything else.
        assert_eq!(fallback_route(true, false, true, false), TeardownLoadCpu);
        assert_eq!(fallback_route(true, false, false, true), TeardownLoadCpu);
        // The D1 incident shape: a live replacement worker was installed
        // between the deliberate kill and the fallback's lock — the take
        // retries THROUGH it, it is not an error.
        assert_eq!(fallback_route(false, true, true, false), ReplacementGpu);
        // Replacement present but this take already burnt its GPU retry:
        // prefer a loaded CPU model; with none, give up rather than kill
        // a healthy worker.
        assert_eq!(fallback_route(false, true, false, true), ExistingCpu);
        assert_eq!(fallback_route(false, true, false, false), GiveUp);
        // Engine flipped to CPU meanwhile: retry there.
        assert_eq!(fallback_route(false, false, true, true), ExistingCpu);
        // Nothing serves at all: load CPU and retry.
        assert_eq!(fallback_route(false, false, true, false), TeardownLoadCpu);
        assert_eq!(fallback_route(false, false, false, false), TeardownLoadCpu);
    }

    /// D2: a teardown must flip the chip snapshot IMMEDIATELY — the owner
    /// must never see "GPU"/the old model while the worker is dead and a
    /// replacement load is still running.
    #[test]
    fn teardown_refreshes_the_chip_snapshot_at_once() {
        let ctx = AppCtx::new();
        {
            let mut engine = lock(&ctx.engine);
            engine.model_name = "large-v3-turbo".into();
            engine.device = "gpu".into();
        }
        // Seed the snapshot as if a live worker had been serving (a real
        // one can't exist headlessly — refresh would see it dead).
        *lock(&ctx.status) = EngineStatus {
            model: "large-v3-turbo".into(),
            device: "gpu".into(),
        };
        teardown_engine(&ctx);
        let s = lock(&ctx.status).clone();
        assert_eq!(s.device, "cpu", "dead worker must not read GPU");
        assert_eq!(s.model, "", "model falls back to the configured one");
    }

    /// When the engine lock is free, `engine_dict` must serve (and cache)
    /// the LIVE truth, not a stale snapshot — a silently-changed engine
    /// shows through on the next idle read.
    #[test]
    fn engine_dict_refreshes_snapshot_when_engine_is_free() {
        let ctx = AppCtx::new();
        lock(&ctx.engine).model_name = "base.en".into();
        let dict = engine_dict(&ctx);
        assert_eq!(dict["model"], "base.en");
        assert_eq!(dict["device"], "CPU", "no live worker -> CPU");
        assert_eq!(lock(&ctx.status).model, "base.en", "snapshot refreshed");
    }

    /// The exit path (`stop_worker`, main thread via tray Restart/Quit)
    /// must never wait on a busy engine lock either.
    #[test]
    fn stop_worker_shape_never_waits_on_a_busy_engine() {
        // stop_worker needs an AppHandle, so exercise the same primitive it
        // uses: try_lock on a lock held elsewhere must yield WouldBlock
        // immediately rather than queueing. This pins the contract that the
        // exit path is built on.
        let ctx = AppCtx::new();
        let ctx = &ctx;
        std::thread::scope(|s| {
            let (locked_tx, locked_rx) = channel();
            let (done_tx, done_rx) = channel::<()>();
            s.spawn(move || {
                let _engine = lock(&ctx.engine);
                locked_tx.send(()).expect("test channel");
                done_rx.recv().expect("release signal");
            });
            locked_rx.recv().expect("holder thread locked");
            let t0 = Instant::now();
            assert!(
                matches!(ctx.engine.try_lock(), Err(TryLockError::WouldBlock)),
                "expected WouldBlock from a busy engine lock"
            );
            assert!(t0.elapsed() < Duration::from_millis(500));
            done_tx.send(()).expect("test channel");
        });
    }
}
