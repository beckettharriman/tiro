//! The `tiro --gpu-worker` subcommand: GPU Whisper inference in a
//! disposable child process, ported from the original's gpu_worker.py.
//!
//! WHY THIS EXISTS (POWER_AND_DGPU.md, "Fix 2"): the first time a process
//! touches the GPU, the driver creates a per-process context that lives
//! until the PROCESS EXITS — freeing the model frees VRAM but not the
//! context, and while any process holds one the discrete GPU can't reach
//! D3cold (~7 W of pure battery drain, measured). So the main Tiro process
//! must NEVER initialize a GPU context (CUDA then, Vulkan now). Instead it
//! spawns this worker on AC and simply kills it on the AC->battery flip:
//! the context dies with the process and the dGPU powers down within
//! seconds. No app restart, no UI interruption.
//!
//! PROTOCOL (parent <-> worker over stdin/stdout, binary, little-endian):
//!
//!   startup   argv: --gpu-worker --model NAME --models-dir DIR
//!             [--device gpu|cpu] [--compute-type CT]
//!             (--device cpu exists so the protocol can be tested on
//!             battery without waking the dGPU). The worker loads the
//!             model, warms up on 1 s of silence, then writes exactly ONE
//!             newline-terminated JSON line to stdout:
//!               {"ready": true,  "model": ..., "device": ...}  -> serving
//!               {"ready": false, "error": "..."} (then exit 1) -> failed
//!
//!   request   <u32 header_len> <header JSON, utf-8> <float32 PCM bytes>
//!             header = {"samples": N, "beam": int, "vocab": str|null,
//!             "language": "en"}; N little-endian f32 samples @ 16 kHz.
//!
//!   response  <u32 len> <JSON, utf-8>
//!             {"ok": true, "segments": ["text", ...]} or
//!             {"ok": false, "error": "..."}
//!
//!   shutdown  EOF on stdin -> clean exit 0. This is the orphan safety
//!             net: if the parent dies for ANY reason its end of the pipe
//!             closes, the blocking read returns EOF, and we exit — no
//!             orphaned process keeping the GPU awake. One request at a
//!             time; no threads.
//!
//! stdout carries ONLY protocol bytes. Diagnostics go to gpu_worker.log
//! in the working directory.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::transcribe::{self, Transcriber};

const LOG_FILE: &str = "gpu_worker.log";

/// Timestamped line to gpu_worker.log; failure-safe (logging must never
/// take the worker down — the parent treats our death as GPU-unavailable).
fn log(msg: &str) {
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("{stamp} [pid {}] {msg}\n", std::process::id());
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(LOG_FILE) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Read exactly n bytes; None on EOF or a half-frame (parent died
/// mid-write — the only sane response is a clean exit).
fn read_exact(stdin: &mut impl Read, n: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; n];
    match stdin.read_exact(&mut buf) {
        Ok(()) => Some(buf),
        Err(_) => None,
    }
}

/// Length-prefixed JSON response on stdout. An error means the parent is
/// gone — the caller exits, which is exactly right.
fn send(stdout: &mut impl Write, obj: &Value) -> std::io::Result<()> {
    let payload = serde_json::to_vec(obj)?;
    stdout.write_all(&(payload.len() as u32).to_le_bytes())?;
    stdout.write_all(&payload)?;
    stdout.flush()
}

/// One newline-terminated readiness JSON line.
fn send_line(obj: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(format!("{obj}\n").as_bytes());
    let _ = out.flush();
}

struct Opts {
    model: Option<String>,
    models_dir: Option<PathBuf>,
    device: String,
    compute_type: String,
}

/// Tiny manual parse: an arg error must produce a JSON failure line on
/// stdout, never usage text.
fn parse_args(argv: &[String]) -> Opts {
    let mut opts = Opts {
        model: None,
        models_dir: None,
        device: "gpu".into(),
        compute_type: "int8".into(),
    };
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--model" if i + 1 < argv.len() => {
                opts.model = Some(argv[i + 1].clone());
                i += 2;
            }
            "--models-dir" if i + 1 < argv.len() => {
                opts.models_dir = Some(PathBuf::from(&argv[i + 1]));
                i += 2;
            }
            "--device" if i + 1 < argv.len() => {
                opts.device = argv[i + 1].to_lowercase();
                i += 2;
            }
            "--compute-type" if i + 1 < argv.len() => {
                opts.compute_type = argv[i + 1].clone();
                i += 2;
            }
            _ => i += 1,
        }
    }
    opts
}

fn load(opts: &Opts) -> Result<Transcriber, String> {
    let name = opts.model.as_deref().ok_or("--model is required")?;
    let models_dir = opts
        .models_dir
        .as_deref()
        .ok_or("--models-dir is required")?;
    let use_gpu = opts.device == "gpu";
    if use_gpu && !cfg!(feature = "gpu") {
        // Built without the Vulkan feature: claiming a GPU here would just
        // serve silent CPU inference while the chip says GPU. Fail loudly
        // so the parent latches GPU off and serves on CPU honestly.
        return Err("built without GPU support (rebuild with --features gpu)".into());
    }
    let model_path = transcribe::ensure_model(models_dir, name, &opts.compute_type)?;
    let vad_path = match transcribe::ensure_vad_model(models_dir) {
        Ok(p) => Some(p),
        Err(e) => {
            log(&format!("VAD model unavailable ({e}); continuing without"));
            None
        }
    };
    let t = Transcriber::load_on(&model_path, vad_path, use_gpu)?;
    // Warm-up on 1 s of silence: pays kernel/pipeline init NOW instead of
    // on the first real dictation, and proves the device works before we
    // claim readiness.
    t.warm_up()?;
    Ok(t)
}

/// Entry point for `tiro --gpu-worker ...`; returns the process exit code.
pub fn run(argv: &[String]) -> i32 {
    let opts = parse_args(argv);
    log(&format!(
        "starting: model={:?} device={:?} models_dir={:?} gpu_built={}",
        opts.model,
        opts.device,
        opts.models_dir,
        cfg!(feature = "gpu")
    ));

    let transcriber = match load(&opts) {
        Ok(t) => t,
        Err(e) => {
            log(&format!("LOAD FAILED: {e}"));
            send_line(&json!({ "ready": false, "error": e }));
            return 1;
        }
    };
    let model_name = opts.model.as_deref().unwrap_or_default();
    send_line(&json!({ "ready": true, "model": model_name, "device": opts.device }));
    log(&format!("ready: {model_name:?} on {}", opts.device));

    // Serve one request at a time until stdin closes (EOF: parent closed
    // the pipe or died — exit cleanly).
    let mut stdin = std::io::stdin().lock();
    while let Some(raw) = read_exact(&mut stdin, 4) {
        let hlen = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
        let Some(hdr) = read_exact(&mut stdin, hlen) else {
            break;
        };
        let Ok(req) = serde_json::from_slice::<Value>(&hdr) else {
            // A malformed frame means the stream is desynced; there is no
            // way to find the next frame boundary, so die and let the
            // parent's CPU fallback take over.
            log("PROTOCOL ERROR: bad header JSON");
            break;
        };
        let samples = req["samples"].as_u64().unwrap_or(0) as usize;
        let Some(pcm) = read_exact(&mut stdin, samples * 4) else {
            break;
        };
        let audio: Vec<f32> = pcm
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let beam = req["beam"].as_u64().unwrap_or(5) as usize;
        let vocab = req["vocab"].as_str().filter(|v| !v.is_empty());

        // Report a failure but stay alive — one bad request shouldn't cost
        // the parent a worker respawn.
        let reply = match transcriber.transcribe(&audio, beam, vocab) {
            Ok(text) => json!({ "ok": true, "segments": [text] }),
            Err(e) => {
                log(&format!("TRANSCRIBE ERROR: {e}"));
                json!({ "ok": false, "error": e })
            }
        };
        let mut out = std::io::stdout().lock();
        if send(&mut out, &reply).is_err() {
            break; // parent gone mid-reply
        }
    }
    log("exiting (stdin closed)");
    0
}
