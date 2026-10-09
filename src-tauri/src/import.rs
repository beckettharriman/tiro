//! File input for the take pipeline: transcribe an audio (or video) file.
//!
//! A microphone take reaches `flow::transcribe_worker` as f32 mono samples
//! plus a rate, and is resampled to 16 kHz before whisper sees it. A file
//! take reaches the very same function the very same way — the only
//! difference is who produced the samples: cpal from a mic, or the
//! decoder below from a file. Decoding is in-process and pure Rust
//! (symphonia): mp3, m4a/aac, flac, wav, ogg/vorbis, alac, aiff, and the
//! audio track of an mp4/mkv/webm, with nothing to install. Nothing
//! downstream — engine choice, vocab prompt, cleanup, corrections,
//! clipboard, transcript log, panel entry — knows or cares which source
//! it came from.
//!
//! Also home to `tiro --transcribe-file`, the headless form of the same
//! path (any file -> engine -> text on stdout), which is what
//! `scripts/transcribe-file.sh` drives.

use std::fs::File;
use std::path::Path;
use std::time::Instant;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as DecodeFailure;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

use crate::audio::SAMPLE_RATE;

/// Streaming linear resampler to 16 kHz: the same interpolation as
/// `audio::resample_to_16k`, fed one decoded chunk at a time so a long
/// file never has to sit in memory at its native rate (an hour of 48 kHz
/// stereo would be 1.4 GB as f32; at 16 kHz mono it is 230 MB). The
/// phase and the last input sample carry across chunks, so the output is
/// identical to resampling the whole file in one go.
struct Resampler {
    step: f64,
    /// Position of the next output sample, in input samples, relative to
    /// `prev` (which sits at position -1).
    pos: f64,
    prev: Option<f32>,
}

impl Resampler {
    fn new(rate: u32) -> Self {
        Self {
            step: f64::from(rate) / f64::from(SAMPLE_RATE),
            pos: 0.0,
            prev: None,
        }
    }

    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }
        if self.step == 1.0 && self.prev.is_none() {
            out.extend_from_slice(input);
            return;
        }
        // Virtual input: prev (if any) at index 0, then `input`.
        let at = |i: usize| -> f32 {
            match self.prev {
                Some(p) => {
                    if i == 0 {
                        p
                    } else {
                        input[i - 1]
                    }
                }
                None => input[i],
            }
        };
        let len = input.len() + usize::from(self.prev.is_some());
        let mut pos = self.pos;
        while pos + 1.0 < len as f64 {
            let i = pos.floor() as usize;
            let frac = (pos - i as f64) as f32;
            out.push(at(i) * (1.0 - frac) + at(i + 1) * frac);
            pos += self.step;
        }
        // Carry the last input sample and the phase past it.
        self.prev = Some(input[input.len() - 1]);
        self.pos = pos - (len as f64 - 1.0);
    }

    /// The final output sample, clamped at the last input (matches the
    /// one-shot resampler's clamp at the end of the buffer).
    fn finish(self, out: &mut Vec<f32>) {
        if let Some(p) = self.prev {
            if self.pos < 1.0 && self.step != 1.0 {
                out.push(p);
            }
        }
    }
}

/// Decode `path` to 16 kHz mono f32 — the buffer a live take is resampled
/// to. Multi-channel audio is averaged down to mono. A video file works
/// too: only its default audio track is read. Errors say what went
/// wrong in the decoder's own words so a corrupt or unsupported file
/// explains itself.
pub fn decode_to_16k(path: &Path) -> Result<Vec<f32>, String> {
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| format!("unsupported or unreadable file: {e}"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| format!("{} has no audio track", path.display()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| format!("{} has no decodable audio", path.display()))?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| format!("no decoder for this audio: {e}"))?;

    let mut out: Vec<f32> = Vec::new();
    let mut interleaved: Vec<f32> = Vec::new();
    let mut mono: Vec<f32> = Vec::new();
    let mut resampler: Option<Resampler> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(DecodeFailure::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(buf) => buf,
            // A damaged frame is skipped, like every player does; the
            // rest of the file still transcribes.
            Err(DecodeFailure::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decoding {}: {e}", path.display())),
        };
        let spec = decoded.spec();
        let channels = spec.channels().count().max(1);
        let rs = resampler.get_or_insert_with(|| Resampler::new(spec.rate()));
        interleaved.clear();
        decoded.copy_to_vec_interleaved::<f32>(&mut interleaved);
        mono.clear();
        mono.extend(
            interleaved
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        rs.push(&mono, &mut out);
    }
    if let Some(rs) = resampler {
        rs.finish(&mut out);
    }
    if out.is_empty() {
        return Err(format!("{} has no audio", path.display()));
    }
    Ok(out)
}

/// What the history shows in the mic column for a file take.
pub fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// `tiro --transcribe-file <audio> [--model NAME] [--device gpu|cpu] [--beam N]`
///
/// The headless twin of the panel's Import: decode in-process, run the
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

    /// A generated stereo 48 kHz WAV decodes to 1 s of 16 kHz mono with
    /// the tone intact.
    #[test]
    fn decodes_a_wav_to_16k_mono() {
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
            (15_990..=16_010).contains(&samples.len()),
            "{}",
            samples.len()
        );
        assert!(samples.iter().any(|s| s.abs() > 0.1));
        assert!(decode_to_16k(&dir.path().join("missing.mp3")).is_err());
        // not audio at all
        std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();
        assert!(decode_to_16k(&dir.path().join("notes.txt")).is_err());
    }

    #[test]
    fn streaming_resampler_matches_the_one_shot_resampler() {
        let input: Vec<f32> = (0..48_000).map(|i| ((i as f32) * 0.01).sin()).collect();
        let whole = crate::audio::resample_to_16k(&input, 48_000);
        let mut streamed = Vec::new();
        let mut rs = Resampler::new(48_000);
        for chunk in input.chunks(1_000) {
            rs.push(chunk, &mut streamed);
        }
        rs.finish(&mut streamed);
        assert_eq!(whole.len(), streamed.len(), "same length");
        for (a, b) in whole.iter().zip(&streamed) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    #[test]
    fn streaming_resampler_passes_16k_through() {
        let input: Vec<f32> = (0..1_000).map(|i| i as f32).collect();
        let mut out = Vec::new();
        let mut rs = Resampler::new(16_000);
        rs.push(&input[..500], &mut out);
        rs.push(&input[500..], &mut out);
        rs.finish(&mut out);
        assert_eq!(out.len(), 1_000);
        assert_eq!(out[999], 999.0);
    }
}
