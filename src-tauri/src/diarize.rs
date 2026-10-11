//! Speaker separation ("who said what") for imported files and system-audio
//! captures: sherpa-onnx's offline speaker diarization, which is a port of
//! the pyannote 3.x pipeline — pyannote segmentation-3.0 over 10 s windows,
//! one speaker embedding per (window, local speaker), agglomerative
//! clustering — run through ONNX Runtime on the CPU. CPU only is a feature
//! here, not a limitation: it means this runs inside the main process
//! without creating a GPU context (design rule 1), and the clustering is
//! never the slow part of a take anyway.
//!
//! The step comes AFTER Whisper: the take is transcribed with word
//! timestamps (`transcribe::Transcriber::transcribe_words`, or the GPU
//! worker's `words` reply), this module turns the audio into speaker
//! turns, and `speaker_text` merges the two with whisperx's rule (every
//! word goes to the speaker it overlaps most; words with no overlap go to
//! the nearest turn) into "Speaker N:" paragraphs.
//!
//! Models live under the app dir's `models/` folder like the Whisper
//! GGUFs, downloaded on first use from the k2-fsa release pages with their
//! license texts saved beside them: pyannote segmentation-3.0 (MIT, CNRS)
//! as the int8 ONNX export, and the WeSpeaker ResNet34-LM embedder
//! (CC BY 4.0) — the same embedder the pyannote 3.1 pipeline uses.
//!
//! Everything in here compiles without the `diarize` cargo feature too;
//! `diarize()` then reports that the build has no speaker separation.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::transcribe::Word;

/// One stretch of speech by one speaker, seconds from the start of the take.
/// `speaker` is the clustering's label (0-based, not stable across runs).
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub start: f32,
    pub end: f32,
    pub speaker: usize,
}

/// Whether this build can separate speakers at all (the `diarize` feature).
pub fn available() -> bool {
    cfg!(feature = "diarize")
}

/// The error `diarize` returns on a build without the feature.
pub const NOT_BUILT: &str =
    "this build has no speaker separation (built without the diarize feature)";

// ---- models ----------------------------------------------------------------

const RELEASE_BASE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/";

/// The segmentation model's folder under models/, as the tarball names it.
pub const SEG_DIR: &str = "sherpa-onnx-pyannote-segmentation-3-0";
/// The int8 export inside that folder (same quality as fp32, 4x smaller).
pub const SEG_FILE: &str = "model.int8.onnx";
const SEG_FILE_BYTES: u64 = 1_540_506;
const SEG_TAR: &str = "sherpa-onnx-pyannote-segmentation-3-0.tar.bz2";
const SEG_TAR_BYTES: u64 = 6_958_444;
/// Files kept from the tarball: the int8 model, its MIT license, the
/// README. The fp32 model and the export/demo scripts are not extracted.
#[cfg(feature = "diarize")]
const SEG_KEEP: &[&str] = &[SEG_FILE, "LICENSE", "README.md"];

/// The speaker embedding model (one file on the release page).
pub const EMB_FILE: &str = "wespeaker_en_voxceleb_resnet34_LM.onnx";
const EMB_BYTES: u64 = 26_530_550;
const EMB_LICENSE_FILE: &str = "wespeaker_en_voxceleb_resnet34_LM.LICENSE.txt";
/// CC BY 4.0 legal code, saved beside the embedding model; the attribution
/// line goes on top of it.
const CC_BY_4_0: &str = include_str!("../licenses/CC-BY-4.0.txt");
const EMB_ATTRIBUTION: &str = "wespeaker_en_voxceleb_resnet34_LM.onnx\n\
Speaker embedding model from WeSpeaker (https://github.com/wenet-e2e/wespeaker),\n\
ResNet34-LM trained on VoxCeleb, exported to ONNX by the sherpa-onnx project\n\
(https://github.com/k2-fsa/sherpa-onnx). Licensed under the Creative Commons\n\
Attribution 4.0 International license, reproduced below.\n\n";

/// The two model files a run needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelPaths {
    pub segmentation: PathBuf,
    pub embedding: PathBuf,
}

fn model_paths(models_dir: &Path) -> ModelPaths {
    ModelPaths {
        segmentation: models_dir.join(SEG_DIR).join(SEG_FILE),
        embedding: models_dir.join(EMB_FILE),
    }
}

fn file_has_size(path: &Path, bytes: u64) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == bytes)
}

/// Both models present and complete under `models_dir` (exact sizes, like
/// `transcribe::model_status`: a truncated file reads as not installed and
/// is re-downloaded).
pub fn models_installed(models_dir: &Path) -> bool {
    let p = model_paths(models_dir);
    file_has_size(&p.segmentation, SEG_FILE_BYTES) && file_has_size(&p.embedding, EMB_BYTES)
}

/// Total bytes `ensure_models` would download when nothing is installed.
pub const DOWNLOAD_BYTES: u64 = SEG_TAR_BYTES + EMB_BYTES;

/// Resolve the model files, downloading what is missing. `progress` sees
/// `(bytes_done, total)` across both downloads together (the segmentation
/// tarball first, then the embedding model); returning `false` cancels,
/// which surfaces as `transcribe::DOWNLOAD_CANCELLED`. Models already on
/// disk return at once without a progress call.
pub fn ensure_models(
    models_dir: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<ModelPaths, String> {
    let paths = model_paths(models_dir);
    if models_installed(models_dir) {
        return Ok(paths);
    }
    fs::create_dir_all(models_dir).map_err(|e| e.to_string())?;
    let need_seg = !file_has_size(&paths.segmentation, SEG_FILE_BYTES);
    let need_emb = !file_has_size(&paths.embedding, EMB_BYTES);
    let total = if need_seg { SEG_TAR_BYTES } else { 0 } + if need_emb { EMB_BYTES } else { 0 };
    let mut base = 0u64;
    if need_seg {
        let tar_path = models_dir.join(SEG_TAR);
        crate::transcribe::download_with(
            &format!("{RELEASE_BASE}speaker-segmentation-models/{SEG_TAR}"),
            &tar_path,
            SEG_TAR,
            &mut |done, _| progress(base + done, Some(total)),
        )?;
        let extracted = extract_segmentation(&tar_path, models_dir);
        let _ = fs::remove_file(&tar_path);
        extracted?;
        if !file_has_size(&paths.segmentation, SEG_FILE_BYTES) {
            return Err(format!(
                "{SEG_TAR} did not contain {SEG_DIR}/{SEG_FILE} at the expected size"
            ));
        }
        base += SEG_TAR_BYTES;
    }
    if need_emb {
        crate::transcribe::download_with(
            &format!("{RELEASE_BASE}speaker-recongition-models/{EMB_FILE}"),
            &paths.embedding,
            EMB_FILE,
            &mut |done, _| progress(base + done, Some(total)),
        )?;
        let license = models_dir.join(EMB_LICENSE_FILE);
        if let Err(e) = fs::write(&license, format!("{EMB_ATTRIBUTION}{CC_BY_4_0}")) {
            eprintln!("could not write {}: {e}", license.display());
        }
    }
    Ok(paths)
}

/// Pull `SEG_KEEP` out of the segmentation tarball into
/// `models_dir/SEG_DIR/`. Only the listed file names under the tarball's
/// own top-level folder are written; anything else in the archive is
/// skipped, so a surprising archive cannot write outside that folder.
#[cfg(feature = "diarize")]
fn extract_segmentation(tar_path: &Path, models_dir: &Path) -> Result<(), String> {
    let dir = models_dir.join(SEG_DIR);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = fs::File::open(tar_path).map_err(|e| format!("open {SEG_TAR}: {e}"))?;
    let mut archive = tar::Archive::new(bzip2::read::MultiBzDecoder::new(file));
    let entries = archive
        .entries()
        .map_err(|e| format!("read {SEG_TAR}: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("read {SEG_TAR}: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("read {SEG_TAR}: {e}"))?
            .into_owned();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !entry.header().entry_type().is_file() || !SEG_KEEP.contains(&name) {
            continue;
        }
        let dest = dir.join(name);
        let mut out =
            fs::File::create(&dest).map_err(|e| format!("write {}: {e}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| format!("write {}: {e}", dest.display()))?;
    }
    Ok(())
}

#[cfg(not(feature = "diarize"))]
fn extract_segmentation(_tar_path: &Path, _models_dir: &Path) -> Result<(), String> {
    Err(NOT_BUILT.into())
}

// ---- the pipeline ----------------------------------------------------------

/// Threads for the two ONNX sessions. The embedding passes dominate and
/// scale well; half the logical cores, capped, leaves the machine usable
/// while a long call separates in the background. `TIRO_DIARIZE_THREADS`
/// overrides for measurements.
fn threads() -> i32 {
    if let Some(n) = std::env::var("TIRO_DIARIZE_THREADS")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .filter(|n| *n >= 1)
    {
        return n;
    }
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    (cores / 2).clamp(1, 8) as i32
}

/// Clustering distance threshold when the speaker count is not given
/// (cosine distance, complete linkage). sherpa-onnx's default of 0.5 split
/// a two-person phone call into 10 speakers; 0.7 is what the pyannote 3.1
/// pipeline uses with this same embedder, and on a 5-minute excerpt of
/// that call 0.7 and 0.8 both found exactly the two (0.9 merged them).
/// `TIRO_DIARIZE_THRESHOLD` overrides it for tuning runs.
#[cfg(feature = "diarize")]
const CLUSTER_THRESHOLD: f32 = 0.7;

#[cfg(feature = "diarize")]
fn cluster_threshold() -> f32 {
    std::env::var("TIRO_DIARIZE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|t| *t > 0.0)
        .unwrap_or(CLUSTER_THRESHOLD)
}

/// Separate the speakers in `samples_16k` (16 kHz mono f32). `num_speakers`
/// fixes the cluster count when the caller knows it (a two-person call);
/// `None` lets the threshold decide. `progress` receives 0.0..=1.0 during
/// the embedding phase, the long one. Models are resolved under
/// `models_dir` and downloaded first when missing (without progress; use
/// `ensure_models` beforehand to show that phase).
pub fn diarize_in(
    models_dir: &Path,
    samples_16k: &[f32],
    num_speakers: Option<usize>,
    progress: impl FnMut(f32),
) -> Result<Vec<Turn>, String> {
    let paths = ensure_models(models_dir, &mut |_, _| true)?;
    let mut progress = progress;
    engine::run(&paths, samples_16k, num_speakers, &mut progress)
}

/// `diarize_in` against the app dir's models folder.
pub fn diarize(
    samples_16k: &[f32],
    num_speakers: Option<usize>,
    progress: impl FnMut(f32),
) -> Result<Vec<Turn>, String> {
    diarize_in(
        &crate::flow::app_dir().join("models"),
        samples_16k,
        num_speakers,
        progress,
    )
}

#[cfg(feature = "diarize")]
mod engine {
    use std::ffi::{c_void, CString};
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use sherpa_onnx_sys as sys;

    use super::{ModelPaths, Turn};

    // The 1.13.8 safe wrapper exposes `process` without a progress
    // callback; the C API has one. Same C library, same ABI.
    extern "C" {
        fn SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
            sd: *const sys::OfflineSpeakerDiarization,
            samples: *const f32,
            n: i32,
            callback: Option<unsafe extern "C" fn(i32, i32, *mut c_void) -> i32>,
            arg: *mut c_void,
        ) -> *const sys::OfflineSpeakerDiarizationResult;
    }

    /// `arg` is a `*mut &mut dyn FnMut(f32)` for the duration of one
    /// `process` call. A panic must not unwind into C (that aborts), so
    /// the closure runs under catch_unwind and the return value — which
    /// the library ignores anyway — says "continue".
    unsafe extern "C" fn on_progress(done: i32, total: i32, arg: *mut c_void) -> i32 {
        if arg.is_null() || total <= 0 {
            return 1;
        }
        let frac = (done as f32 / total as f32).clamp(0.0, 1.0);
        // SAFETY: `arg` points at the `&mut dyn FnMut(f32)` that `run`
        // keeps alive across the process call; the library calls this
        // from the calling thread only.
        let progress: &mut &mut dyn FnMut(f32) = unsafe { &mut *arg.cast() };
        let _ = catch_unwind(AssertUnwindSafe(|| progress(frac)));
        1
    }

    struct Diarizer(*const sys::OfflineSpeakerDiarization);

    impl Drop for Diarizer {
        fn drop(&mut self) {
            // SAFETY: created by SherpaOnnxCreateOfflineSpeakerDiarization
            // and destroyed exactly once, here.
            unsafe { sys::SherpaOnnxDestroyOfflineSpeakerDiarization(self.0) }
        }
    }

    struct DiarizationResult(*const sys::OfflineSpeakerDiarizationResult);

    impl Drop for DiarizationResult {
        fn drop(&mut self) {
            // SAFETY: a non-null result pointer from Process*, destroyed once.
            unsafe { sys::SherpaOnnxOfflineSpeakerDiarizationDestroyResult(self.0) }
        }
    }

    fn c_path(path: &std::path::Path) -> Result<CString, String> {
        let s = path
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))?;
        CString::new(s).map_err(|_| format!("{} contains a NUL byte", path.display()))
    }

    pub fn run(
        paths: &ModelPaths,
        samples: &[f32],
        num_speakers: Option<usize>,
        progress: &mut dyn FnMut(f32),
    ) -> Result<Vec<Turn>, String> {
        let n = i32::try_from(samples.len())
            .map_err(|_| "audio too long for speaker separation".to_string())?;
        let seg_model = c_path(&paths.segmentation)?;
        let emb_model = c_path(&paths.embedding)?;
        let provider = CString::new("cpu").map_err(|e| e.to_string())?;
        let threads = super::threads();
        let num_clusters = num_speakers
            .and_then(|n| i32::try_from(n).ok())
            .filter(|n| *n >= 1)
            .unwrap_or(-1);
        let config = sys::OfflineSpeakerDiarizationConfig {
            segmentation: sys::OfflineSpeakerSegmentationModelConfig {
                pyannote: sys::OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: seg_model.as_ptr(),
                    window_shift_ratio: 0.1,
                },
                num_threads: threads,
                debug: 0,
                provider: provider.as_ptr(),
            },
            embedding: sys::SpeakerEmbeddingExtractorConfig {
                model: emb_model.as_ptr(),
                num_threads: threads,
                debug: 0,
                provider: provider.as_ptr(),
            },
            clustering: sys::FastClusteringConfig {
                num_clusters,
                threshold: super::cluster_threshold(),
                compute_confidence: 0,
            },
            min_duration_on: 0.3,
            min_duration_off: 0.5,
        };
        // SAFETY: every pointer in `config` stays alive (the CStrings
        // above) until the create call returns; the library copies what
        // it keeps.
        let sd = unsafe { sys::SherpaOnnxCreateOfflineSpeakerDiarization(&config) };
        if sd.is_null() {
            return Err(format!(
                "speaker separation models failed to load ({} / {})",
                paths.segmentation.display(),
                paths.embedding.display()
            ));
        }
        let sd = Diarizer(sd);
        // SAFETY: the model's rate is a plain getter on a live handle.
        let rate = unsafe { sys::SherpaOnnxOfflineSpeakerDiarizationGetSampleRate(sd.0) };
        if rate != crate::audio::SAMPLE_RATE as i32 {
            return Err(format!(
                "segmentation model expects {rate} Hz audio, the take is 16000 Hz"
            ));
        }
        let mut cb: &mut dyn FnMut(f32) = progress;
        let arg: *mut &mut dyn FnMut(f32) = &mut cb;
        // SAFETY: `samples` is a live slice of `n` floats; `arg` points at
        // a local that outlives the call, which runs synchronously on this
        // thread and calls `on_progress` with exactly this pointer.
        let result = unsafe {
            SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
                sd.0,
                samples.as_ptr(),
                n,
                Some(on_progress),
                arg.cast(),
            )
        };
        if result.is_null() {
            return Err("speaker separation produced no result".into());
        }
        let result = DiarizationResult(result);
        // SAFETY: plain getters on a live result; the sorted segment array
        // has `count` entries and is destroyed with DestroySegment once
        // copied out.
        let turns = unsafe {
            let count = sys::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments(result.0);
            let p = sys::SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime(result.0);
            if p.is_null() || count <= 0 {
                Vec::new()
            } else {
                let turns = std::slice::from_raw_parts(p, count as usize)
                    .iter()
                    .map(|s| Turn {
                        start: s.start,
                        end: s.end,
                        speaker: s.speaker.max(0) as usize,
                    })
                    .collect();
                sys::SherpaOnnxOfflineSpeakerDiarizationDestroySegment(p);
                turns
            }
        };
        Ok(turns)
    }
}

#[cfg(not(feature = "diarize"))]
mod engine {
    use super::{ModelPaths, Turn, NOT_BUILT};

    pub fn run(
        _paths: &ModelPaths,
        _samples: &[f32],
        _num_speakers: Option<usize>,
        _progress: &mut dyn FnMut(f32),
    ) -> Result<Vec<Turn>, String> {
        Err(NOT_BUILT.into())
    }
}

// ---- merging with the words ------------------------------------------------

/// Which speaker each word belongs to, by whisperx's rule: the speaker
/// whose turns overlap the word the most (ties: the lower label); a word
/// no turn touches goes to the nearest turn in time (`fill_nearest`).
/// `None` only when there are no turns at all.
pub fn assign_speakers(words: &[Word], turns: &[Turn]) -> Vec<Option<usize>> {
    if turns.is_empty() {
        return vec![None; words.len()];
    }
    let n_speakers = turns.iter().map(|t| t.speaker + 1).max().unwrap_or(1);
    let mut overlap = vec![0.0f32; n_speakers];
    words
        .iter()
        .map(|w| {
            let (ws, we) = (w.t0 as f32 / 100.0, w.t1 as f32 / 100.0);
            overlap.iter_mut().for_each(|o| *o = 0.0);
            let mut nearest: Option<(f32, usize)> = None;
            for t in turns {
                let shared = we.min(t.end) - ws.max(t.start);
                if shared > 0.0 {
                    overlap[t.speaker] += shared;
                }
                let gap = (t.start - we).max(ws - t.end).max(0.0);
                if nearest.is_none_or(|(g, _)| gap < g) {
                    nearest = Some((gap, t.speaker));
                }
            }
            let best = overlap
                .iter()
                .enumerate()
                .filter(|(_, o)| **o > 0.0)
                .max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(&a.0)))
                .map(|(s, _)| s);
            best.or(nearest.map(|(_, s)| s))
        })
        .collect()
}

/// The speaker-labelled transcript: consecutive words by the same speaker
/// form one paragraph, paragraphs are separated by a blank line and start
/// with "Speaker N: ", N counting from 1 in order of first appearance (so
/// whoever speaks first is Speaker 1 whatever the clustering called them).
#[derive(Debug, Clone, PartialEq)]
pub struct Labelled {
    pub text: String,
    /// Distinct speakers in the text; 0 when no turn was found and the
    /// text is the plain, unlabelled transcript.
    pub speakers: usize,
}

pub fn speaker_text(words: &[Word], turns: &[Turn]) -> Labelled {
    let assigned = assign_speakers(words, turns);
    let mut order: Vec<usize> = Vec::new(); // cluster label -> position = N-1
    let mut paragraphs: Vec<(Option<usize>, Vec<&str>)> = Vec::new();
    for (word, speaker) in words.iter().zip(&assigned) {
        let label = speaker.map(|s| match order.iter().position(|o| *o == s) {
            Some(i) => i + 1,
            None => {
                order.push(s);
                order.len()
            }
        });
        match paragraphs.last_mut() {
            Some((last, text)) if *last == label => text.push(word.text.as_str()),
            _ => paragraphs.push((label, vec![word.text.as_str()])),
        }
    }
    let text = paragraphs
        .iter()
        .map(|(label, text)| match label {
            Some(n) => format!("Speaker {n}: {}", text.join(" ")),
            None => text.join(" "),
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    Labelled {
        text,
        speakers: order.len(),
    }
}

/// Apply `f` to the body of every paragraph of a `speaker_text` result,
/// keeping the "Speaker N: " labels and the blank lines between
/// paragraphs. The clipboard cleanup collapses whitespace, so it has to
/// run per paragraph rather than over the whole labelled text.
pub fn map_paragraphs(text: &str, mut f: impl FnMut(&str) -> String) -> String {
    text.split("\n\n")
        .map(|p| match split_label(p) {
            Some((label, body)) => format!("{label}{}", f(body)),
            None => f(p),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// "Speaker 3: hello" -> ("Speaker 3: ", "hello").
fn split_label(paragraph: &str) -> Option<(&str, &str)> {
    let rest = paragraph.strip_prefix("Speaker ")?;
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let body_at = "Speaker ".len() + digits + ": ".len();
    rest[digits..]
        .starts_with(": ")
        .then(|| paragraph.split_at(body_at))
}

// ---- headless check --------------------------------------------------------

/// Peak resident set size of this process in MiB, where the OS tells us.
pub fn peak_rss_mib() -> Option<f64> {
    #[cfg(unix)]
    {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: getrusage fills the struct we point it at.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
            return None;
        }
        // Linux reports KiB, macOS bytes.
        let scale = if cfg!(target_os = "macos") {
            1024.0 * 1024.0
        } else {
            1024.0
        };
        Some(usage.ru_maxrss as f64 / scale)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// `tiro --diarize-test <wav> [--speakers N]`: run the pipeline alone on a
/// WAV (any rate/channels) and print the turns, the speaker count, timing
/// and memory. Models resolve under the app dir like everything else.
pub fn diarize_test(args: &[String], at: usize) {
    let Some(wav) = args.get(at + 1).filter(|a| !a.starts_with("--")) else {
        eprintln!("usage: tiro --diarize-test <wav> [--speakers N]");
        return;
    };
    let speakers = match speakers_arg(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };
    let (mono, rate) = match crate::transcribe::read_wav_mono(wav) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };
    let audio = crate::audio::resample_to_16k(&mono, rate);
    let secs = audio.len() as f64 / f64::from(crate::audio::SAMPLE_RATE);
    eprintln!(
        "wav: {rate} Hz -> {} samples @ 16 kHz ({secs:.1} s)",
        audio.len()
    );
    let models_dir = crate::flow::app_dir().join("models");
    let t0 = Instant::now();
    if !models_installed(&models_dir) {
        eprintln!(
            "downloading speaker separation models ({} MB)",
            DOWNLOAD_BYTES / 1_000_000
        );
    }
    if let Err(e) = ensure_models(&models_dir, &mut |_, _| true) {
        eprintln!("ERROR: {e}");
        return;
    }
    eprintln!(
        "models ready in {:.1}s, {} threads, speakers: {}",
        t0.elapsed().as_secs_f64(),
        threads(),
        speakers.map_or("auto".to_string(), |n| n.to_string())
    );
    let t1 = Instant::now();
    let mut last = -1i32;
    let turns = match diarize_in(&models_dir, &audio, speakers, |p| {
        let pct = (p * 100.0) as i32;
        if pct / 10 > last / 10 {
            last = pct;
            eprintln!("  embeddings {pct}% ({:.1}s)", t1.elapsed().as_secs_f64());
        }
    }) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    };
    let took = t1.elapsed().as_secs_f64();
    let found = turns.iter().map(|t| t.speaker + 1).max().unwrap_or(0);
    for t in &turns {
        println!("{:8.2} {:8.2} speaker {}", t.start, t.end, t.speaker);
    }
    eprintln!(
        "{} turns, {found} speakers, {secs:.1} s of audio in {took:.1} s ({:.3}x realtime, {:.1}x faster), peak RSS {}",
        turns.len(),
        took / secs.max(0.001),
        secs / took.max(0.001),
        peak_rss_mib().map_or("n/a".to_string(), |m| format!("{m:.0} MiB"))
    );
}

/// `--speakers N` from a CLI arg list: None when absent.
pub fn speakers_arg(args: &[String]) -> Result<Option<usize>, String> {
    let Some(i) = args.iter().position(|a| a == "--speakers") else {
        return Ok(None);
    };
    match args.get(i + 1).map(|v| v.parse::<usize>()) {
        Some(Ok(n)) if n >= 1 => Ok(Some(n)),
        _ => Err("--speakers must be a positive integer".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, t0: i64, t1: i64) -> Word {
        Word {
            text: text.to_string(),
            t0,
            t1,
        }
    }

    fn turn(start: f32, end: f32, speaker: usize) -> Turn {
        Turn {
            start,
            end,
            speaker,
        }
    }

    #[test]
    fn words_go_to_the_speaker_they_overlap_most() {
        let turns = [turn(0.0, 2.0, 1), turn(2.0, 4.0, 0)];
        // straddles the boundary: 0.3 s with speaker 1, 0.7 s with speaker 0
        let words = [word("a", 0, 100), word("b", 170, 270), word("c", 300, 390)];
        assert_eq!(
            assign_speakers(&words, &turns),
            vec![Some(1), Some(0), Some(0)]
        );
    }

    #[test]
    fn overlap_sums_across_turns_of_one_speaker() {
        // speaker 0 has two short turns inside the word, speaker 1 one
        // longer one; the sum decides (0.6 vs 0.5).
        let turns = [turn(0.0, 0.3, 0), turn(0.3, 0.8, 1), turn(0.8, 1.1, 0)];
        let words = [word("long", 0, 110)];
        assert_eq!(assign_speakers(&words, &turns), vec![Some(0)]);
    }

    #[test]
    fn ties_go_to_the_lower_label_and_no_turns_means_none() {
        let turns = [turn(0.0, 1.0, 1), turn(1.0, 2.0, 0)];
        assert_eq!(
            assign_speakers(&[word("x", 50, 150)], &turns),
            vec![Some(0)]
        );
        assert_eq!(assign_speakers(&[word("x", 0, 10)], &[]), vec![None]);
    }

    #[test]
    fn words_outside_every_turn_fill_from_the_nearest() {
        let turns = [turn(1.0, 2.0, 0), turn(5.0, 6.0, 1)];
        // before everything -> first turn; in the gap, closer to the second
        let words = [
            word("early", 0, 50),
            word("gap", 420, 460),
            word("late", 700, 750),
        ];
        assert_eq!(
            assign_speakers(&words, &turns),
            vec![Some(0), Some(1), Some(1)]
        );
    }

    #[test]
    fn paragraphs_regroup_and_number_by_first_appearance() {
        // cluster 1 speaks first, so it becomes Speaker 1
        let turns = [turn(0.0, 2.0, 1), turn(2.0, 4.0, 0), turn(4.0, 6.0, 1)];
        let words = [
            word("Hi", 0, 50),
            word("there.", 60, 120),
            word("Hello!", 210, 300),
            word("Right,", 410, 450),
            word("so.", 460, 520),
        ];
        let out = speaker_text(&words, &turns);
        assert_eq!(out.speakers, 2);
        assert_eq!(
            out.text,
            "Speaker 1: Hi there.\n\nSpeaker 2: Hello!\n\nSpeaker 1: Right, so."
        );
    }

    #[test]
    fn no_turns_gives_the_plain_text() {
        let words = [word("just", 0, 10), word("me", 20, 30)];
        let out = speaker_text(&words, &[]);
        assert_eq!(out.speakers, 0);
        assert_eq!(out.text, "just me");
        assert_eq!(speaker_text(&[], &[turn(0.0, 1.0, 0)]).text, "");
    }

    #[test]
    fn map_paragraphs_keeps_labels() {
        let text = "Speaker 1: um hello\n\nSpeaker 2: yes  yes\n\nno label here";
        let out = map_paragraphs(text, |body| body.to_uppercase());
        assert_eq!(
            out,
            "Speaker 1: UM HELLO\n\nSpeaker 2: YES  YES\n\nNO LABEL HERE"
        );
        assert_eq!(split_label("Speaker 12: x"), Some(("Speaker 12: ", "x")));
        assert_eq!(split_label("Speaker : x"), None);
        assert_eq!(split_label("Speakers 1: x"), None);
    }

    #[test]
    fn speakers_flag_parses() {
        let a = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(speakers_arg(&a(&["--diarize"])), Ok(None));
        assert_eq!(speakers_arg(&a(&["--speakers", "3"])), Ok(Some(3)));
        assert!(speakers_arg(&a(&["--speakers", "0"])).is_err());
        assert!(speakers_arg(&a(&["--speakers"])).is_err());
    }

    #[test]
    fn install_state_needs_both_files_at_their_sizes() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!models_installed(dir.path()));
        let seg = dir.path().join(SEG_DIR);
        fs::create_dir_all(&seg).unwrap();
        let f = fs::File::create(seg.join(SEG_FILE)).unwrap();
        f.set_len(SEG_FILE_BYTES).unwrap();
        assert!(!models_installed(dir.path()), "embedding missing");
        let f = fs::File::create(dir.path().join(EMB_FILE)).unwrap();
        f.set_len(EMB_BYTES - 1).unwrap();
        assert!(!models_installed(dir.path()), "embedding truncated");
        f.set_len(EMB_BYTES).unwrap();
        assert!(models_installed(dir.path()));
        let paths = model_paths(dir.path());
        assert!(paths
            .segmentation
            .ends_with("sherpa-onnx-pyannote-segmentation-3-0/model.int8.onnx"));
    }
}
