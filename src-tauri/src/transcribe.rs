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

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";
/// whisper.cpp's Silero VAD model (same file its download-vad-model.sh fetches).
const VAD_URL: &str =
    "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin";
const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";

/// One whisper.cpp model the settings' model manager can offer. Sizes are
/// the exact content-lengths on the HF repo (checked 2026-07); the curated
/// subset is what the Models card shows before "Show all models".
pub struct ModelInfo {
    pub name: &'static str,
    pub hint: &'static str,
    pub curated: bool,
    /// Whether a `-q8_0` quantized GGUF exists on the HF repo. large-v1 and
    /// large-v3 only ship f16 there, so `int8` falls back to the plain file.
    pub has_q8: bool,
    /// Content-length of the q8_0 file (0 when `has_q8` is false).
    pub q8_bytes: u64,
    /// Content-length of the plain f16 file.
    pub f16_bytes: u64,
}

impl ModelInfo {
    /// The GGUF file this entry resolves to for the given `compute_type`.
    pub fn file_name(&self, compute_type: &str) -> String {
        if compute_type == "int8" && self.has_q8 {
            format!("ggml-{}-q8_0.bin", self.name)
        } else {
            format!("ggml-{}.bin", self.name)
        }
    }

    /// The expected download size for the given `compute_type`.
    pub fn size_bytes(&self, compute_type: &str) -> u64 {
        if compute_type == "int8" && self.has_q8 {
            self.q8_bytes
        } else {
            self.f16_bytes
        }
    }
}

/// The full whisper.cpp GGUF catalog offered in settings.
pub const CATALOG: &[ModelInfo] = &[
    ModelInfo {
        name: "tiny",
        hint: "Fastest — very low accuracy, all languages",
        curated: false,
        has_q8: true,
        q8_bytes: 43_537_433,
        f16_bytes: 77_691_713,
    },
    ModelInfo {
        name: "tiny.en",
        hint: "Fastest — very low accuracy",
        curated: true,
        has_q8: true,
        q8_bytes: 43_550_795,
        f16_bytes: 77_704_715,
    },
    ModelInfo {
        name: "base",
        hint: "Fast — all languages",
        curated: false,
        has_q8: true,
        q8_bytes: 81_768_585,
        f16_bytes: 147_951_465,
    },
    ModelInfo {
        name: "base.en",
        hint: "Fast — battery default",
        curated: true,
        has_q8: true,
        q8_bytes: 81_781_811,
        f16_bytes: 147_964_211,
    },
    ModelInfo {
        name: "small",
        hint: "Balanced — all languages",
        curated: false,
        has_q8: true,
        q8_bytes: 264_464_607,
        f16_bytes: 487_601_967,
    },
    ModelInfo {
        name: "small.en",
        hint: "Balanced — plugged default",
        curated: true,
        has_q8: true,
        q8_bytes: 264_477_561,
        f16_bytes: 487_614_201,
    },
    ModelInfo {
        name: "medium",
        hint: "Accurate — slower on CPU, all languages",
        curated: false,
        has_q8: true,
        q8_bytes: 823_369_779,
        f16_bytes: 1_533_763_059,
    },
    ModelInfo {
        name: "medium.en",
        hint: "Accurate — slower on CPU",
        curated: true,
        has_q8: true,
        q8_bytes: 823_382_461,
        f16_bytes: 1_533_774_781,
    },
    ModelInfo {
        name: "large-v1",
        hint: "Original large — all languages",
        curated: false,
        has_q8: false,
        q8_bytes: 0,
        f16_bytes: 3_094_623_691,
    },
    ModelInfo {
        name: "large-v2",
        hint: "Very accurate — all languages",
        curated: false,
        has_q8: true,
        q8_bytes: 1_656_129_691,
        f16_bytes: 3_094_623_691,
    },
    ModelInfo {
        name: "large-v3",
        hint: "Very accurate — all languages",
        curated: false,
        has_q8: false,
        q8_bytes: 0,
        f16_bytes: 3_095_033_483,
    },
    ModelInfo {
        name: "large-v3-turbo",
        hint: "Most accurate — GPU recommended, all languages",
        curated: true,
        has_q8: true,
        q8_bytes: 874_188_075,
        f16_bytes: 1_624_555_275,
    },
];

/// Look a model up in the catalog by name.
pub fn catalog_find(name: &str) -> Option<&'static ModelInfo> {
    CATALOG.iter().find(|m| m.name == name)
}

/// Map a model name + the config's `compute_type` to a GGUF file name. The
/// original's CPU `int8` quantization maps to the q8_0 GGUF variants; any
/// other value gets the plain (f16) files. Catalog entries know whether a
/// q8_0 file actually exists upstream (large-v1/large-v3 don't have one);
/// unknown model names keep the plain historical mapping.
pub fn model_file_name(model: &str, compute_type: &str) -> String {
    if let Some(info) = catalog_find(model) {
        return info.file_name(compute_type);
    }
    if compute_type == "int8" {
        format!("ggml-{model}-q8_0.bin")
    } else {
        format!("ggml-{model}.bin")
    }
}

/// Install state of one model on disk.
pub struct ModelStatus {
    pub installed: bool,
    pub bytes: u64,
}

/// Whether `model`'s resolved GGUF is present under `models_dir`, and how
/// large the file on disk is.
pub fn model_status(models_dir: &Path, model: &str, compute_type: &str) -> ModelStatus {
    match fs::metadata(models_dir.join(model_file_name(model, compute_type))) {
        Ok(md) if md.is_file() => ModelStatus {
            installed: true,
            bytes: md.len(),
        },
        _ => ModelStatus {
            installed: false,
            bytes: 0,
        },
    }
}

fn download(url: &str, dest: &Path, label: &str) -> Result<(), String> {
    download_with(url, dest, label, &mut |_, _| true)
}

/// The error string a cancelled download surfaces as — callers treat it as
/// a quiet outcome, not a failure.
pub const DOWNLOAD_CANCELLED: &str = "cancelled";

/// Stream `url` to `dest` (via a `.part` file), reporting every chunk to
/// `progress` as `(bytes_done, content_length)`; a `false` return aborts
/// the pull and removes the partial file.
fn download_with(
    url: &str,
    dest: &Path,
    label: &str,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<(), String> {
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
        if !progress(done, total) {
            drop(out);
            let _ = fs::remove_file(&part);
            eprintln!("  {label}: download cancelled");
            return Err(DOWNLOAD_CANCELLED.into());
        }
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
    download_model_with(models_dir, model, compute_type, &mut |_, _| true)
}

/// `ensure_model` with a progress callback — the model manager's download
/// path. Reports `(bytes_done, content_length)` per received chunk; a model
/// already on disk returns immediately without calling `progress`.
pub fn download_model_with(
    models_dir: &Path,
    model: &str,
    compute_type: &str,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<PathBuf, String> {
    fs::create_dir_all(models_dir).map_err(|e| e.to_string())?;
    let file = model_file_name(model, compute_type);
    let path = models_dir.join(&file);
    if !path.exists() {
        download_with(&format!("{HF_BASE}{file}"), &path, &file, progress)?;
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

/// Annotations whisper.cpp emits for audio that contains no speech, matched
/// case-insensitively against the inside of a `[...]`/`(...)`/`*...*` group
/// after normalizing `_`/`-` to spaces: `[BLANK_AUDIO]`, `[ Silence ]`,
/// `(noise)`, `*music*`, …
const NON_SPEECH_PHRASES: &[&str] = &[
    "blank audio",
    "silence",
    "silent",
    "inaudible",
    "music",
    "music playing",
    "background music",
    "background noise",
    "noise",
    "static",
    "applause",
    "laughter",
    "laughing",
    "laughs",
    "no speech",
    "no audio",
    "breathing",
    "coughing",
    "cough",
    "sigh",
    "sighs",
    "sighing",
    "chatter",
    "indistinct chatter",
    "wind",
    "typing",
    "clicking",
    "humming",
    "beep",
    "beeping",
    "pause",
];

fn is_non_speech_phrase(inner: &str) -> bool {
    let normalized = inner
        .to_ascii_lowercase()
        .replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    NON_SPEECH_PHRASES.contains(&normalized.as_str())
}

/// Collapse a transcription that consists ONLY of whisper.cpp non-speech
/// markers (and whitespace) to the empty string. The original's
/// faster-whisper returned `""` for silent takes, which routed them through
/// the empty-result path (cancel cue, nothing copied, nothing logged — see
/// `flow::Outcome::Empty`); whisper.cpp instead emits literal markers like
/// `[BLANK_AUDIO]`, `(silence)` or `♪`. This restores parity at the point
/// where both engines' results converge (the in-process CPU path and the
/// `--gpu-worker` child both run [`Transcriber::transcribe`]).
///
/// Conservative by design (never lose a take): any character outside a
/// recognized marker — including bare words like "silence" without brackets,
/// unclosed brackets, or stray punctuation — keeps the take verbatim.
/// Markers mixed with real speech are NOT stripped.
pub fn collapse_non_speech(text: &str) -> &str {
    let mut rest = text.trim_start();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('♪') {
            rest = r.trim_start();
            continue;
        }
        let close = match rest.as_bytes()[0] {
            b'[' => ']',
            b'(' => ')',
            b'*' => '*',
            _ => return text,
        };
        let Some(end) = rest[1..].find(close) else {
            return text;
        };
        if !is_non_speech_phrase(&rest[1..1 + end]) {
            return text;
        }
        rest = rest[1 + end + 1..].trim_start();
    }
    ""
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
        let joined = parts.join(" ");
        // A take that is nothing but non-speech markers ([BLANK_AUDIO], …)
        // becomes "" so silence follows the original's empty-result path.
        Ok(collapse_non_speech(joined.trim()).to_string())
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
    // Same resolution as the app proper (exe-dir rule, TIRO_APP_DIR
    // override) so this headless path proves the REAL config/vocab chain.
    let app_dir = crate::flow::app_dir();
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
    // The REAL vocab path: the same prompt the live take pipeline feeds
    // whisper (vocab.txt via use_vocab_bias), logged so tests can assert
    // the bias is actually wired without depending on model output.
    let vocab = get_vocab_prompt(&app_dir, &cfg);
    match &vocab {
        Some(p) => eprintln!("vocab prompt: {p}"),
        None => eprintln!("vocab prompt: (none — bias off or vocab.txt empty)"),
    }
    let start = std::time::Instant::now();
    match transcriber.transcribe(&audio, 1, vocab.as_deref()) {
        Ok(text) => {
            eprintln!("transcribed in {:.2} s", start.elapsed().as_secs_f32());
            println!("TEXT: {text}");
            // Mirror the live take's clipboard layer (flow::transcribe_worker):
            // cleanup per clipboard_cleanup, then corrections.txt — so the
            // whole chain is provable headless. TEXT above stays verbatim,
            // exactly like the transcript log.
            let cleanup_mode = cfg.get("clipboard_cleanup");
            let corrections = crate::vocab::read_corrections(&app_dir);
            eprintln!(
                "cleanup mode: {cleanup_mode}; corrections: {}",
                corrections.len()
            );
            let clean = crate::vocab::apply_corrections(
                &crate::clipboard::clipboard_text(&text, &cleanup_mode),
                &corrections,
            );
            println!("CLEAN: {clean}");
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
        // not in the catalog -> historical mapping still applies
        assert_eq!(
            model_file_name("distil-x", "int8"),
            "ggml-distil-x-q8_0.bin"
        );
    }

    #[test]
    fn models_without_q8_fall_back_to_f16() {
        // The HF repo has no q8_0 file for large-v1/large-v3 (checked live);
        // int8 must resolve to the plain f16 file for those, not a 404 name.
        assert_eq!(model_file_name("large-v1", "int8"), "ggml-large-v1.bin");
        assert_eq!(model_file_name("large-v3", "int8"), "ggml-large-v3.bin");
        assert_eq!(
            model_file_name("large-v2", "int8"),
            "ggml-large-v2-q8_0.bin"
        );
        assert_eq!(
            model_file_name("large-v3-turbo", "int8"),
            "ggml-large-v3-turbo-q8_0.bin"
        );
    }

    #[test]
    fn catalog_is_complete_and_curated_in_order() {
        assert_eq!(CATALOG.len(), 12);
        let curated: Vec<&str> = CATALOG
            .iter()
            .filter(|m| m.curated)
            .map(|m| m.name)
            .collect();
        assert_eq!(
            curated,
            [
                "tiny.en",
                "base.en",
                "small.en",
                "medium.en",
                "large-v3-turbo"
            ]
        );
        for m in CATALOG {
            assert!(m.f16_bytes > 0, "{} has no f16 size", m.name);
            assert_eq!(m.has_q8, m.q8_bytes > 0, "{} q8 size mismatch", m.name);
            assert!(m.size_bytes("int8") > 0, "{}", m.name);
        }
        assert!(catalog_find("tiny.en").is_some());
        assert!(catalog_find("nope").is_none());
    }

    #[test]
    fn model_status_reports_install_state_and_size() {
        let dir = TempDir::new().unwrap();
        let st = model_status(dir.path(), "tiny.en", "int8");
        assert!(!st.installed);
        assert_eq!(st.bytes, 0);
        std::fs::write(dir.path().join("ggml-tiny.en-q8_0.bin"), b"stub").unwrap();
        let st = model_status(dir.path(), "tiny.en", "int8");
        assert!(st.installed);
        assert_eq!(st.bytes, 4);
        // a different compute_type resolves to a different (absent) file
        assert!(!model_status(dir.path(), "tiny.en", "float16").installed);
    }

    /// Real-network check of the model manager's download path; run manually
    /// with `cargo test -- --ignored download_tiny_en`.
    #[test]
    #[ignore = "network: downloads ~43 MB from huggingface.co"]
    fn download_tiny_en_q8_with_progress() {
        let dir = TempDir::new().unwrap();
        let mut events = 0u64;
        let mut last = (0u64, None::<u64>);
        let path = download_model_with(dir.path(), "tiny.en", "int8", &mut |done, total| {
            events += 1;
            last = (done, total);
            true
        })
        .unwrap();
        assert!(path.ends_with("ggml-tiny.en-q8_0.bin"));
        let expected = catalog_find("tiny.en").unwrap().q8_bytes;
        assert_eq!(std::fs::metadata(&path).unwrap().len(), expected);
        assert_eq!(last.0, expected, "final progress event == file size");
        assert_eq!(last.1, Some(expected), "content-length reported");
        assert!(events > 10, "progress fired {events} times");
        // second call: already installed -> no progress events
        events = 0;
        let again = download_model_with(dir.path(), "tiny.en", "int8", &mut |_, _| {
            events += 1;
            true
        })
        .unwrap();
        assert_eq!(again, path);
        assert_eq!(events, 0);
    }

    #[test]
    fn non_speech_only_takes_collapse_to_empty() {
        for t in [
            "[BLANK_AUDIO]",
            " [BLANK_AUDIO] ",
            "[BLANK_AUDIO] [BLANK_AUDIO]",
            "[blank_audio]",
            "[Blank_Audio]",
            "[BLANK-AUDIO]",
            "(silence)",
            "(Silence)",
            "[SILENCE]",
            "[ Silence ]",
            "[ Inaudible ]",
            "[MUSIC]",
            "*music*",
            "(noise)",
            "♪",
            "♪ ♪ ♪",
            "[MUSIC] (silence) ♪",
            "",
            "   ",
        ] {
            assert_eq!(collapse_non_speech(t), "", "{t:?}");
        }
    }

    #[test]
    fn real_speech_passes_through_untouched() {
        for t in [
            "hello world",
            "he said [BLANK_AUDIO] appears on screen",
            "[BLANK_AUDIO] then he spoke",
            "turn the music down",
            "silence",        // bare word, no marker delimiters
            "(well, maybe)",  // parenthesized real words
            "[BLANK_AUDIO",   // unclosed bracket -> doubt -> keep
            "[BLANK_AUDIO].", // stray punctuation -> doubt -> keep
            "(no)",           // ambiguous single word -> keep
            "♪ happy birthday ♪",
            "...",
        ] {
            assert_eq!(collapse_non_speech(t), t, "{t:?}");
        }
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
