//! CPU transcription via whisper-rs (whisper.cpp), mirroring the original's
//! faster-whisper usage (PORTING_NOTES §5): 16 kHz f32 mono in, language=en,
//! beam 1 on CPU, VAD enabled (whisper.cpp's integrated Silero VAD stands in
//! for faster-whisper's vad_filter), vocab bias as the initial prompt,
//! segments joined with spaces and stripped. Models are GGUF files under
//! `./models`, downloaded from huggingface.co/ggerganov/whisper.cpp on first
//! use with progress logged.
//!
//! HARD RULE (PORTING_NOTES §6): this module must never initialize a GPU
//! context in the main process — `use_gpu` is explicitly false here; GPU
//! inference belongs exclusively to the `--gpu-worker` child (task 3.4).

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadParams,
};

use crate::audio::SAMPLE_RATE;
use crate::config::ConfigStore;

/// The models offered in the panel's selectors.
pub const MODELS: [&str; 3] = ["base.en", "small.en", "medium.en"];

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";
/// whisper.cpp's Silero VAD model (same file its download-vad-model.sh fetches).
const VAD_URL: &str =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";
const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";

/// Map a model name + the config's `compute_type` to a GGUF file name. The
/// original's CPU `int8` quantization maps to the q8_0 GGUF variants; any
/// other value gets the plain (f16) files.
pub fn model_file_name(model: &str, compute_type: &str) -> String {
    if compute_type == "int8" {
        format!("ggml-{model}-q8_0.bin")
    } else {
        format!("ggml-{model}.bin")
    }
}

fn download(url: &str, dest: &Path, label: &str) -> Result<(), String> {
    eprintln!("downloading {label} from {url}");
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("download of {label} failed: {e}"))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let mut reader = response.into_body().into_reader();
    let part = dest.with_extension("part");
    let mut out = fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done: u64 = 0;
    let mut last_pct = 0;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if let Some(total) = total {
            let pct = (done * 100 / total) as u32;
            if pct >= last_pct + 10 {
                last_pct = pct - pct % 10;
                eprintln!("  {label}: {pct}% ({done}/{total} bytes)");
            }
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    drop(out);
    fs::rename(&part, dest).map_err(|e| e.to_string())?;
    eprintln!("  {label}: download complete ({done} bytes)");
    Ok(())
}

/// Path to the GGUF for `model`, downloading it on first use.
pub fn ensure_model(models_dir: &Path, model: &str, compute_type: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(models_dir).map_err(|e| e.to_string())?;
    let file = model_file_name(model, compute_type);
    let path = models_dir.join(&file);
    if !path.exists() {
        download(&format!("{HF_BASE}{file}"), &path, &file)?;
    }
    Ok(path)
}

/// Path to the Silero VAD model, downloading it on first use.
pub fn ensure_vad_model(models_dir: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(models_dir).map_err(|e| e.to_string())?;
    let path = models_dir.join(VAD_FILE);
    if !path.exists() {
        download(VAD_URL, &path, VAD_FILE)?;
    }
    Ok(path)
}

/// `get_vocab_prompt`: vocab.txt words joined into one whitespace-collapsed
/// line, skipping blanks and `#` comments; None when disabled/missing/empty.
pub fn get_vocab_prompt(app_dir: &Path, cfg: &ConfigStore) -> Option<String> {
    if !cfg.get_bool("use_vocab_bias") {
        return None;
    }
    let raw = fs::read_to_string(app_dir.join("vocab.txt")).ok()?;
    let words: Vec<&str> = raw
        .lines()
        .filter(|ln| !ln.trim().is_empty() && !ln.trim_start().starts_with('#'))
        .collect();
    let txt = words
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if txt.is_empty() {
        None
    } else {
        Some(txt)
    }
}

/// A loaded CPU whisper model.
pub struct Transcriber {
    ctx: WhisperContext,
    vad_model: Option<PathBuf>,
}

impl Transcriber {
    /// Load the GGUF at `model_path` on CPU. `vad_model` enables Silero VAD
    /// for every transcription (mirroring the original's vad_filter=True).
    pub fn load(model_path: &Path, vad_model: Option<PathBuf>) -> Result<Self, String> {
        // The main process must NEVER touch a GPU (PORTING_NOTES §6).
        Self::load_on(model_path, vad_model, false)
    }

    /// Load with an explicit GPU choice. `use_gpu` is only ever true inside
    /// the `--gpu-worker` child process (PORTING_NOTES §6).
    pub fn load_on(
        model_path: &Path,
        vad_model: Option<PathBuf>,
        use_gpu: bool,
    ) -> Result<Self, String> {
        let mut params = WhisperContextParameters::default();
        params.use_gpu(use_gpu);
        let ctx = WhisperContext::new_with_params(
            model_path
                .to_str()
                .ok_or_else(|| "model path is not valid UTF-8".to_string())?,
            params,
        )
        .map_err(|e| format!("model load failed: {e}"))?;
        Ok(Self { ctx, vad_model })
    }

    /// Transcribe 16 kHz mono f32 samples; join segment texts with spaces
    /// and strip, exactly like the original `_do_transcribe`.
    pub fn transcribe(
        &self,
        samples: &[f32],
        beam: usize,
        vocab: Option<&str>,
    ) -> Result<String, String> {
        let mut params = FullParams::new(SamplingStrategy::BeamSearch {
            beam_size: beam as std::ffi::c_int,
            patience: -1.0,
        });
        params.set_language(Some("en"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if let Some(vocab) = vocab {
            params.set_initial_prompt(vocab);
        }
        let vad_path;
        if let Some(vad_model) = &self.vad_model {
            vad_path = vad_model.to_str().map(|s| s.to_string());
            if let Some(path) = &vad_path {
                params.set_vad_model_path(Some(path));
                params.set_vad_params(WhisperVadParams::default());
                params.enable_vad(true);
            }
        }
        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| format!("state init failed: {e}"))?;
        state
            .full(params, samples)
            .map_err(|e| format!("transcribe failed: {e}"))?;
        let n = state.full_n_segments();
        let mut parts = Vec::new();
        for i in 0..n {
            if let Some(segment) = state.get_segment(i) {
                if let Ok(text) = segment.to_str_lossy() {
                    let trimmed = text.trim().to_string();
                    if !trimmed.is_empty() {
                        parts.push(trimmed);
                    }
                }
            }
        }
        Ok(parts.join(" ").trim().to_string())
    }

    /// The original's warm-up: transcribe 1 s of silence to page the model in
    /// and verify the device actually works.
    pub fn warm_up(&self) -> Result<(), String> {
        let silence = vec![0.0f32; SAMPLE_RATE as usize];
        self.transcribe(&silence, 1, None).map(|_| ())
    }
}

/// `tiro --transcribe-test <wav>`: temporary 1.5 verification — loads the
/// battery model per config, downloads on first use, warms up, then
/// transcribes the given WAV (any rate/channels; converted like a live take).
/// Read a WAV as mono f32 at its native rate (int formats normalized,
/// channels averaged).
pub fn read_wav_mono(wav_path: &str) -> Result<(Vec<f32>, u32), String> {
    let reader =
        hound::WavReader::open(wav_path).map_err(|e| format!("cannot read {wav_path}: {e}"))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .filter_map(Result::ok)
            .collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .filter_map(Result::ok)
                .map(|s| s as f32 / max)
                .collect()
        }
    };
    let mono: Vec<f32> = if spec.channels > 1 {
        samples
            .chunks_exact(spec.channels as usize)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect()
    } else {
        samples
    };
    Ok((mono, spec.sample_rate))
}

pub fn transcribe_test(wav_path: &str) {
    let app_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cfg = ConfigStore::load(app_dir.join("config.ini"), &app_dir);
    let model = {
        let m = cfg.get("model_battery");
        if m.is_empty() {
            cfg.get("model")
        } else {
            m
        }
    };
    let models_dir = app_dir.join("models");
    let model_path = match ensure_model(&models_dir, &model, &cfg.get("compute_type")) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };
    let vad_path = match ensure_vad_model(&models_dir) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("WARNING: VAD model unavailable ({e}); continuing without VAD");
            None
        }
    };
    let (mono, rate) = match read_wav_mono(wav_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };
    let audio = crate::audio::resample_to_16k(&mono, rate);
    eprintln!(
        "wav: {} Hz -> {} samples @ 16 kHz ({:.2} s)",
        rate,
        audio.len(),
        audio.len() as f32 / SAMPLE_RATE as f32
    );
    let transcriber = match Transcriber::load(&model_path, vad_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };
    eprintln!("warming up on 1 s silence…");
    if let Err(e) = transcriber.warm_up() {
        eprintln!("ERROR: warm-up failed: {e}");
        return;
    }
    let vocab = get_vocab_prompt(&app_dir, &cfg);
    let start = std::time::Instant::now();
    match transcriber.transcribe(&audio, 1, vocab.as_deref()) {
        Ok(text) => {
            eprintln!("transcribed in {:.2} s", start.elapsed().as_secs_f32());
            println!("TEXT: {text}");
        }
        Err(e) => eprintln!("ERROR: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn model_files_map_like_the_original_compute_type() {
        assert_eq!(model_file_name("base.en", "int8"), "ggml-base.en-q8_0.bin");
        assert_eq!(
            model_file_name("small.en", "int8"),
            "ggml-small.en-q8_0.bin"
        );
        assert_eq!(model_file_name("base.en", "float16"), "ggml-base.en.bin");
        assert_eq!(model_file_name("medium.en", ""), "ggml-medium.en.bin");
    }

    #[test]
    fn vocab_prompt_skips_comments_and_blanks() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("vocab.txt"),
            "# names\nTiro\n\n  # tools\nWhisper   cpal\n",
        )
        .unwrap();
        let cfg = ConfigStore::load(dir.path().join("config.ini"), dir.path());
        assert_eq!(
            get_vocab_prompt(dir.path(), &cfg),
            Some("Tiro Whisper cpal".to_string())
        );
    }

    #[test]
    fn vocab_prompt_none_when_disabled_or_missing() {
        let dir = TempDir::new().unwrap();
        let mut cfg = ConfigStore::load(dir.path().join("config.ini"), dir.path());
        assert_eq!(get_vocab_prompt(dir.path(), &cfg), None, "no vocab.txt");
        std::fs::write(dir.path().join("vocab.txt"), "word\n").unwrap();
        cfg.set("use_vocab_bias", "false");
        assert_eq!(get_vocab_prompt(dir.path(), &cfg), None, "bias disabled");
        std::fs::write(dir.path().join("vocab.txt"), "# only comments\n\n").unwrap();
        cfg.set("use_vocab_bias", "true");
        assert_eq!(
            get_vocab_prompt(dir.path(), &cfg),
            None,
            "empty after filter"
        );
    }
}
