//! Parent-side client for the `tiro --gpu-worker` child (PORTING_NOTES §6).
//!
//! The client re-execs our own binary with the `--gpu-worker` subcommand
//! and speaks the framed stdin/stdout protocol (see gpu_worker.rs). A
//! dedicated reader thread owns the child's stdout and forwards frames
//! over a channel so every wait can carry a timeout: ready 30 s with a
//! cached model / 120 s when the worker may be downloading it, and per
//! request 2x realtime + 30 s. Stop is stdin-close (EOF -> clean child
//! exit), escalating to kill after 3 s. If this process dies uncleanly,
//! the pipes close on their own and the child's EOF exit is the orphan
//! safety net — no extra machinery needed.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::audio;
use crate::flow::lock;

pub const READY_TIMEOUT_CACHED: Duration = Duration::from_secs(30);
pub const READY_TIMEOUT_DOWNLOAD: Duration = Duration::from_secs(120);

enum Frame {
    Ready(Value),
    Response(Value),
    /// stdout closed or went undecodable — the worker is done for.
    Dead(String),
}

/// The request pipe state: held for the WHOLE framed request/response, so
/// it lives under its own lock — a transcription-length hold that nothing
/// short-lived (alive checks, exit-path kills) may ever queue behind.
struct WorkerIo {
    stdin: Option<ChildStdin>,
    frames: Receiver<Frame>,
}

pub struct GpuWorker {
    /// Long-held per request (`transcribe`).
    io: Mutex<WorkerIo>,
    /// Always short-held: `try_wait` / `kill`. Kept separate from `io` so
    /// the exit path can kill a worker mid-request without waiting out the
    /// transcription (the tray Restart/Quit handlers run on the main
    /// thread, which must never block on take-length work).
    child: Mutex<Child>,
    pub model: String,
}

fn read_frame(stdout: &mut impl Read) -> Result<Value, String> {
    let mut len = [0u8; 4];
    stdout
        .read_exact(&mut len)
        .map_err(|e| format!("worker stdout closed: {e}"))?;
    let mut payload = vec![0u8; u32::from_le_bytes(len) as usize];
    stdout
        .read_exact(&mut payload)
        .map_err(|e| format!("worker stdout closed mid-frame: {e}"))?;
    serde_json::from_slice(&payload).map_err(|e| format!("bad worker frame: {e}"))
}

impl GpuWorker {
    /// Spawn the worker and wait for its readiness line. `device` is "gpu"
    /// in production; "cpu" lets the protocol be exercised without waking
    /// the dGPU (the original's `--device cpu` test hook).
    pub fn spawn(
        model: &str,
        models_dir: &Path,
        compute_type: &str,
        device: &str,
        ready_timeout: Duration,
    ) -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        let mut cmd = Command::new(exe);
        cmd.args([
            "--gpu-worker",
            "--model",
            model,
            "--models-dir",
            &models_dir.to_string_lossy(),
            "--compute-type",
            compute_type,
            "--device",
            device,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        #[cfg(target_os = "windows")]
        {
            // CREATE_NO_WINDOW, like the original's pythonw spawn.
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("worker spawn failed: {e}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("worker stdout missing")?;

        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            // First the newline-terminated readiness line ...
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(n) if n > 0 => match serde_json::from_str::<Value>(line.trim()) {
                    Ok(v) => {
                        let _ = tx.send(Frame::Ready(v));
                    }
                    Err(e) => {
                        let _ = tx.send(Frame::Dead(format!("bad ready line: {e}")));
                        return;
                    }
                },
                _ => {
                    let _ = tx.send(Frame::Dead("worker exited before ready".into()));
                    return;
                }
            }
            // ... then length-prefixed response frames until EOF.
            loop {
                match read_frame(&mut reader) {
                    Ok(v) => {
                        if tx.send(Frame::Response(v)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Frame::Dead(e));
                        return;
                    }
                }
            }
        });

        let worker = Self {
            io: Mutex::new(WorkerIo { stdin, frames: rx }),
            child: Mutex::new(child),
            model: model.to_string(),
        };
        // The io guard is a temporary: it drops at the end of this statement,
        // so the failure arms below can run `stop()`'s graceful path.
        let ready = lock(&worker.io).frames.recv_timeout(ready_timeout);
        match ready {
            Ok(Frame::Ready(v)) if v["ready"].as_bool() == Some(true) => Ok(worker),
            Ok(Frame::Ready(v)) => {
                let err = v["error"].as_str().unwrap_or("unknown load failure").into();
                worker.stop();
                Err(err)
            }
            Ok(Frame::Dead(e)) => {
                worker.stop();
                Err(e)
            }
            Ok(Frame::Response(_)) => {
                worker.stop();
                Err("worker sent a frame before ready".into())
            }
            Err(RecvTimeoutError::Timeout) => {
                worker.stop();
                Err(format!(
                    "worker not ready within {}s",
                    ready_timeout.as_secs()
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                worker.stop();
                Err("worker reader died".into())
            }
        }
    }

    /// Still running? (A dead child must never show a green GPU chip.)
    /// Short-held `child` lock only — safe from any thread at any time,
    /// including while a request holds `io`.
    pub fn alive(&self) -> bool {
        matches!(lock(&self.child).try_wait(), Ok(None))
    }

    /// One framed request/response. On error or timeout the caller must
    /// treat this worker as lost (kill it and retry the SAME audio on CPU
    /// — a take can never be lost to the GPU path). Holds `io` for the
    /// whole request; concurrent callers serialize here.
    pub fn transcribe(
        &self,
        audio16: &[f32],
        beam: usize,
        vocab: Option<&str>,
    ) -> Result<String, String> {
        let header = serde_json::to_vec(&json!({
            "samples": audio16.len(),
            "beam": beam,
            "vocab": vocab,
            "language": "en",
        }))
        .map_err(|e| e.to_string())?;
        let mut io = lock(&self.io);
        let stdin = io.stdin.as_mut().ok_or("worker stdin already closed")?;
        stdin
            .write_all(&(header.len() as u32).to_le_bytes())
            .and_then(|()| stdin.write_all(&header))
            .and_then(|()| {
                let mut pcm = Vec::with_capacity(audio16.len() * 4);
                for s in audio16 {
                    pcm.extend_from_slice(&s.to_le_bytes());
                }
                stdin.write_all(&pcm)
            })
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("worker write failed: {e}"))?;

        // 2x realtime + 30 s, like the original's per-request timeout.
        let secs = audio16.len() as f64 / audio::SAMPLE_RATE as f64;
        let timeout = Duration::from_secs_f64(secs * 2.0 + 30.0);
        match io.frames.recv_timeout(timeout) {
            Ok(Frame::Response(v)) => {
                if v["ok"].as_bool() == Some(true) {
                    let joined = v["segments"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default();
                    Ok(joined.trim().to_string())
                } else {
                    Err(v["error"].as_str().unwrap_or("worker error").to_string())
                }
            }
            Ok(Frame::Dead(e)) => Err(e),
            Ok(Frame::Ready(_)) => Err("unexpected ready frame".into()),
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "worker timed out after {:.0}s",
                timeout.as_secs_f64()
            )),
            Err(RecvTimeoutError::Disconnected) => Err("worker reader died".into()),
        }
    }

    /// Stop the worker. Idle: close stdin (EOF -> clean child exit), then
    /// escalate to kill after 3 s. With a request in flight (`io` busy) the
    /// graceful path would mean waiting out a transcription — and the exit
    /// path runs on the main thread — so kill immediately instead: the
    /// dying pipes surface as an io error to the in-flight `transcribe`,
    /// whose caller retries the take on CPU (never-lose-a-take).
    pub fn stop(&self) {
        let graceful = match self.io.try_lock() {
            Ok(mut io) => {
                drop(io.stdin.take());
                true
            }
            Err(TryLockError::Poisoned(p)) => {
                drop(p.into_inner().stdin.take());
                true
            }
            Err(TryLockError::WouldBlock) => false,
        };
        let mut child = lock(&self.child);
        if graceful {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => break,
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for GpuWorker {
    fn drop(&mut self) {
        // Belt over the EOF suspenders: never leave a live child behind a
        // dropped handle (an orphaned worker keeps the dGPU awake).
        self.stop();
    }
}

/// `tiro --gpu-test <wav> [gpu|cpu]`: spawn the worker, transcribe the wav
/// through the framed protocol, stop the worker, and report — the client
/// counterpart of the worker's `--device cpu` test hook.
pub fn gpu_test(wav: &str, device: &str) {
    let (samples, rate) = match crate::transcribe::read_wav_mono(wav) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wav read failed: {e}");
            return;
        }
    };
    let audio16 = audio::resample_to_16k(&samples, rate);
    eprintln!(
        "{} samples @16k, spawning worker on {device} ...",
        audio16.len()
    );
    let t0 = Instant::now();
    let worker = match GpuWorker::spawn(
        "base.en",
        Path::new("models"),
        "int8",
        device,
        READY_TIMEOUT_DOWNLOAD,
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("spawn failed: {e}");
            return;
        }
    };
    eprintln!(
        "ready in {:.1}s; alive={}",
        t0.elapsed().as_secs_f64(),
        worker.alive()
    );
    let t1 = Instant::now();
    match worker.transcribe(&audio16, 5, None) {
        Ok(text) => eprintln!("[{:.1}s] text: {text}", t1.elapsed().as_secs_f64()),
        Err(e) => eprintln!("transcribe failed: {e}"),
    }
    worker.stop();
    eprintln!("stopped; alive={}", worker.alive());
}
