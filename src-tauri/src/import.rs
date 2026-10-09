//! File input for the take pipeline (the "transcribe an audio file" idea).
//!
//! A microphone take reaches `flow::transcribe_worker` as f32 mono samples
//! plus a rate, and is resampled to 16 kHz before whisper sees it. A file
//! take reaches the very same function the very same way — the only
//! difference is who produced the samples: cpal from a mic, or ffmpeg
//! from a file. ffmpeg decodes any container/codec it knows (mp3, m4a,
//! ogg, flac, wav, opus, the audio track of a video, ...) straight to raw
//! 16 kHz mono f32 on its stdout, so nothing downstream — engine choice,
//! vocab prompt, cleanup, corrections, clipboard, transcript log, panel
//! entry — knows or cares which source it came from.
//!
//! ffmpeg is an external tool on purpose: bundling a decoder stack for an
//! idea branch is out of scope, and it is a one-line install everywhere
//! (`dnf`/`apt install ffmpeg`, `brew install ffmpeg`, `winget install
//! ffmpeg`). `FFMPEG` in the environment names a specific binary.
//!
//! Also home to `tiro --transcribe-file`, the headless form of the same
//! path (any file -> engine -> text on stdout), which is what
//! `scripts/transcribe-file.sh` drives.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::audio::SAMPLE_RATE;

/// The ffmpeg binary: `$FFMPEG` when set, otherwise `ffmpeg` on PATH.
pub fn ffmpeg_exe() -> PathBuf {
    std::env::var_os("FFMPEG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ffmpeg"))
}

fn ffmpeg_command() -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(ffmpeg_exe());
    #[cfg(target_os = "windows")]
    {
        // CREATE_NO_WINDOW: no console flashing behind the panel.
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Is ffmpeg runnable? `Ok(version line)` or why not — the panel shows
/// the reason instead of a dead Import button.
pub fn ffmpeg_available() -> Result<String, String> {
    let out = ffmpeg_command()
        .args(["-hide_banner", "-version"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg not found ({}): {e}", ffmpeg_exe().display()))?;
    if !out.status.success() {
        return Err(format!("ffmpeg -version failed: {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("ffmpeg")
        .to_string())
}

/// `ffmpeg_available`, probed once per process (the panel asks on every
/// `get_state`; spawning ffmpeg each time would be silly). Installing
/// ffmpeg while Tiro runs needs a restart to be noticed — acceptable.
pub fn ffmpeg_status() -> Result<String, String> {
    static STATUS: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
    STATUS.get_or_init(ffmpeg_available).clone()
}

/// Decode `path` to 16 kHz mono f32 — the buffer a live take is resampled
/// to. Video files work too (`-vn` drops the picture). Errors carry
/// ffmpeg's own stderr so a corrupt or unsupported file says why.
pub fn decode_to_16k(path: &Path) -> Result<Vec<f32>, String> {
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let mut child = ffmpeg_command()
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &SAMPLE_RATE.to_string(),
            "-f",
            "f32le",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run ffmpeg ({}): {e}", ffmpeg_exe().display()))?;
    // stderr drains on its own thread so a chatty failure can never fill
    // its pipe while this thread is blocked on stdout.
    let mut stderr = child.stderr.take().ok_or("ffmpeg stderr missing")?;
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut stdout = child.stdout.take().ok_or("ffmpeg stdout missing")?;
    let mut bytes = Vec::new();
    stdout
        .read_to_end(&mut bytes)
        .map_err(|e| format!("reading ffmpeg output: {e}"))?;
    let status = child
        .wait()
        .map_err(|e| format!("waiting for ffmpeg: {e}"))?;
    let err_text = err_reader.join().unwrap_or_default();
    if !status.success() {
        let why = err_text.trim();
        return Err(if why.is_empty() {
            format!("ffmpeg failed ({status})")
        } else {
            format!("ffmpeg: {why}")
        });
    }
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    if samples.is_empty() {
        return Err(format!("{} has no audio stream", path.display()));
    }
    Ok(samples)
}

/// What the history shows in the mic column for a file take.
pub fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// `tiro --transcribe-file <audio> [--model NAME] [--device gpu|cpu] [--beam N]`
///
/// The headless twin of the panel's Import: decode with ffmpeg, run the
/// configured (or named) model on the chosen device through the SAME
/// engine code the app serves takes with — `Transcriber` in-process on
/// CPU, the `tiro-gpu-worker` child on GPU — and print the verbatim
/// transcript on stdout (everything else goes to stderr, so
/// `> out.txt` captures exactly the text). Beam follows the live policy
/// (GPU 5, CPU 1) unless `--beam` says otherwise. Models resolve under
/// the app dir like everything else (`TIRO_APP_DIR` to relocate), and a
/// missing model downloads first, exactly as the app would.
pub fn transcribe_file_cli(args: &[String], at: usize) {
    let Some(path) = args.get(at + 1).filter(|a| !a.starts_with("--")) else {
        eprintln!(
            "usage: tiro --transcribe-file <audio> [--model NAME] [--device gpu|cpu] [--beam N]"
        );
        return;
    };
    let opt = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let device = opt("--device").unwrap_or_else(|| "cpu".into());
    if device != "cpu" && device != "gpu" {
        eprintln!("--device must be gpu or cpu");
        return;
    }
    let app_dir = crate::flow::app_dir();
    let cfg = crate::config::ConfigStore::load(app_dir.join("config.ini"), &app_dir);
    let model = opt("--model").unwrap_or_else(|| {
        let m = cfg.get("model");
        if m.is_empty() {
            cfg.get("model_battery")
        } else {
            m
        }
    });
    let compute_type = cfg.get("compute_type");
    let beam: usize = match opt("--beam") {
        Some(b) => match b.parse() {
            Ok(n) if n >= 1 => n,
            _ => {
                eprintln!("--beam must be a positive integer");
                return;
            }
        },
        None if device == "gpu" => 5,
        None => 1,
    };
    let models_dir = app_dir.join("models");
    let vocab = crate::transcribe::get_vocab_prompt(&app_dir, &cfg);

    let t0 = Instant::now();
    let audio = match decode_to_16k(Path::new(path)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };
    let secs = audio.len() as f64 / f64::from(SAMPLE_RATE);
    eprintln!(
        "decoded {} in {:.1}s: {} samples @ 16 kHz ({:.1} s of audio)",
        path,
        t0.elapsed().as_secs_f64(),
        audio.len(),
        secs
    );
    eprintln!(
        "model {model} ({compute_type}) on {device}, beam {beam}, vocab prompt: {}",
        vocab.as_deref().unwrap_or("(none)")
    );

    let t1 = Instant::now();
    let result = if device == "gpu" {
        let gpu_device = crate::hw::default_gpu_index(&crate::hw::snapshot().gpus);
        match crate::gpu::GpuWorker::spawn(
            &model,
            &models_dir,
            &compute_type,
            "gpu",
            gpu_device,
            crate::gpu::READY_TIMEOUT_DOWNLOAD,
        ) {
            Ok(w) => {
                eprintln!(
                    "GPU worker ready (device {gpu_device}) in {:.1}s",
                    t1.elapsed().as_secs_f64()
                );
                let r = w.transcribe(&audio, beam, vocab.as_deref());
                w.stop();
                r
            }
            Err(e) => Err(format!("GPU worker unavailable: {e}")),
        }
    } else {
        crate::transcribe::ensure_model(&models_dir, &model, &compute_type)
            .and_then(|model_path| {
                let vad = crate::transcribe::ensure_vad_model(&models_dir)
                    .map_err(|e| eprintln!("VAD model unavailable ({e}); continuing without VAD"))
                    .ok();
                crate::transcribe::Transcriber::load(&model_path, vad)
            })
            .and_then(|t| {
                eprintln!("CPU model loaded in {:.1}s", t1.elapsed().as_secs_f64());
                t.transcribe(&audio, beam, vocab.as_deref())
            })
    };
    match result {
        Ok(text) => {
            let took = t1.elapsed().as_secs_f64();
            eprintln!(
                "transcribed {:.1} s of audio in {took:.1} s ({:.2}x realtime), {} chars",
                secs,
                secs / took.max(0.001),
                text.chars().count()
            );
            println!("{text}");
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_label_is_the_file_name() {
        assert_eq!(file_label(Path::new("/a/b/talk.mp3")), "talk.mp3");
    }

    /// Round-trip a generated WAV through ffmpeg (skipped when ffmpeg is
    /// not installed on the test machine).
    #[test]
    fn decodes_a_wav_to_16k_mono() {
        if ffmpeg_available().is_err() {
            eprintln!("ffmpeg missing; skipping");
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let wav = dir.path().join("tone.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..48_000 {
            let s = ((i as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16;
            w.write_sample(s).unwrap();
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
        let samples = decode_to_16k(&wav).unwrap();
        // 1 s at 16 kHz, give or take the resampler's edge.
        assert!(
            (15_900..=16_100).contains(&samples.len()),
            "{}",
            samples.len()
        );
        assert!(samples.iter().any(|s| s.abs() > 0.1));
        assert!(decode_to_16k(&dir.path().join("missing.mp3")).is_err());
    }
}
