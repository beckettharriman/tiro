//! Microphone capture, ported from the original's sounddevice-based audio
//! layer (PORTING_NOTES §4): device enumeration with `mic_name` substring
//! matching, native-rate capture with a 48k/44.1k/16k fallback chain, f32
//! mono frames accumulated while recording, linear-interpolation resampling
//! to 16 kHz, and the <0.3 s empty-take rejection.
//!
//! The original filtered Windows host APIs to shared-mode ones (MME/WASAPI/
//! DirectSound, never WDM-KS/ASIO). cpal exposes exactly one shared-mode
//! host per platform (WASAPI on Windows, ALSA on Linux), so the default
//! host already is that filter.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, StreamConfig};

/// Whisper's input rate; everything is resampled to this before transcription.
pub const SAMPLE_RATE: u32 = 16_000;

/// Takes shorter than this many seconds are discarded as accidental taps.
pub const MIN_TAKE_SECS: f64 = 0.3;

/// The original tried the device's native rate first, then this chain.
const FALLBACK_RATES: [u32; 3] = [48_000, 44_100, 16_000];

/// Hard ceiling on a single take: 10 minutes of audio at the capture rate.
/// A misbehaving device can deliver samples far faster than realtime (ALSA's
/// `null` PCM produced ~45 minutes of zeros in 2 s of wall time); without a
/// cap the buffer grows without bound and transcription of the resulting
/// take grinds for tens of minutes, which reads as a wedged UI. Real
/// dictation never approaches 10 minutes per take.
pub const MAX_TAKE_SECS: usize = 600;

/// ALSA's `null` PCM is a bit bucket, not a microphone: it opens happily and
/// generates zero samples (far faster than realtime), so it must never be
/// listed in the mic selector or picked as a capture device. Matched by its
/// raw PCM id (`driver` on the ALSA host) and by its stock description text;
/// neither occurs on other hosts, so this is a no-op on Windows/WASAPI.
fn is_null_device(name: &str, driver: Option<&str>) -> bool {
    driver == Some("null") || name.starts_with("Discard all samples")
}

fn default_rate(device: &Device) -> u32 {
    device
        .default_input_config()
        .map(|c| c.sample_rate())
        .unwrap_or(48_000)
}

/// Unique input-device names, for the panel's mic selector.
pub fn list_mic_names() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for device in devices {
            if let Ok(desc) = device.description() {
                if is_null_device(desc.name(), desc.driver()) {
                    continue;
                }
                let name = desc.name().to_string();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

/// Matching input devices ordered by preference: case-insensitive substring
/// match on `name_substr`. An empty substring (fresh install) — or one that
/// matches nothing — prefers the host DEFAULT input device: on ALSA that is
/// the `default` PCM, which follows the desktop's (PipeWire's) configured
/// default source; on WASAPI it is the OS default input. The remaining input
/// devices follow as fallbacks, in enumeration order. The ALSA null device
/// is filtered out entirely (see `is_null_device`) — previously it sat first
/// in enumeration order and won the "first device that opens" walk.
fn candidates(name_substr: &str) -> Vec<(Device, String, u32)> {
    let host = cpal::default_host();
    let mut all: Vec<(Device, String, u32)> = host
        .input_devices()
        .map(|devices| {
            devices
                .filter_map(|device| {
                    let desc = device.description().ok()?;
                    if is_null_device(desc.name(), desc.driver()) {
                        return None;
                    }
                    let name = desc.name().to_string();
                    let rate = default_rate(&device);
                    Some((device, name, rate))
                })
                .collect()
        })
        .unwrap_or_default();
    if !name_substr.is_empty() {
        let needle = name_substr.to_lowercase();
        let (matched, rest): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|(_, name, _)| name.to_lowercase().contains(&needle));
        if !matched.is_empty() {
            return matched;
        }
        all = rest;
    }
    let mut items: Vec<(Device, String, u32)> = Vec::new();
    if let Some(device) = host.default_input_device() {
        let desc = device.description().ok();
        let null = desc
            .as_ref()
            .is_some_and(|d| is_null_device(d.name(), d.driver()));
        if !null {
            let name = desc
                .map(|d| d.name().to_string())
                .unwrap_or_else(|| "system default".to_string());
            let rate = default_rate(&device);
            items.push((device, name, rate));
        }
    }
    let default_id = items.first().and_then(|(d, _, _)| d.id().ok());
    for item in all {
        if default_id.is_some() && item.0.id().ok() == default_id {
            continue; // already first as the host default
        }
        items.push(item);
    }
    items
}

/// Downmix `data` (interleaved, `channels`-wide frames) to mono and append
/// it to `buf`, never growing `buf` past `max_samples` (the `MAX_TAKE_SECS`
/// cap at the capture rate). Returns true when the cap dropped any input.
/// Called from the realtime callback: no allocation beyond the amortized
/// `Vec` growth the uncapped path already did.
fn append_capped(buf: &mut Vec<f32>, data: &[f32], channels: u16, max_samples: usize) -> bool {
    let remaining = max_samples.saturating_sub(buf.len());
    if channels <= 1 {
        let n = data.len().min(remaining);
        buf.extend_from_slice(&data[..n]);
        n < data.len()
    } else {
        let ch = usize::from(channels);
        let frames = data.len() / ch;
        let n = frames.min(remaining);
        buf.extend(
            data.chunks_exact(ch)
                .take(n)
                .map(|frame| frame.iter().sum::<f32>() / ch as f32),
        );
        n < frames
    }
}

/// `resample_to_16k`: the original's np.interp linear resampler. Sample j of
/// the output sits at position j*len/n on the input grid; interpolate
/// linearly between neighbours, clamping at the final sample.
pub fn resample_to_16k(audio: &[f32], sr: u32) -> Vec<f32> {
    if sr == SAMPLE_RATE {
        return audio.to_vec();
    }
    let len = audio.len();
    let n = ((len as f64) * f64::from(SAMPLE_RATE) / f64::from(sr)).round() as usize;
    if n == 0 || len == 0 {
        return Vec::new();
    }
    (0..n)
        .map(|j| {
            let pos = (j as f64) * (len as f64) / (n as f64);
            let i = pos.floor() as usize;
            if i + 1 >= len {
                audio[len - 1]
            } else {
                let frac = (pos - i as f64) as f32;
                audio[i] * (1.0 - frac) + audio[i + 1] * frac
            }
        })
        .collect()
}

/// The <0.3 s guard: too little audio at `rate` to be a real take. f64
/// arithmetic so the boundary behaves exactly like the original's Python
/// comparison (`audio.size < rate * 0.3`).
pub fn too_short(samples: usize, rate: u32) -> bool {
    (samples as f64) < f64::from(rate) * MIN_TAKE_SECS
}

/// Live input level shared between the stream callback and the pill's
/// level pusher. The callback folds each chunk's peak |sample| in with an
/// atomic max on the f32 bit pattern — valid because peaks are
/// non-negative, and for non-negative IEEE-754 floats the bit ordering
/// matches the numeric ordering. Allocation-free and lock-free, so the
/// audio callback stays cheap.
pub struct LevelMeter(AtomicU32);

impl LevelMeter {
    pub fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    /// Fold a chunk's peak absolute sample into the meter.
    pub fn fold(&self, samples: &[f32]) {
        let mut peak = 0.0f32;
        for &s in samples {
            let a = s.abs();
            if a > peak {
                peak = a;
            }
        }
        if peak > 0.0 {
            self.0.fetch_max(peak.to_bits(), Ordering::Relaxed);
        }
    }

    /// Read the peak accumulated since the last call, resetting it — each
    /// poll reports the loudest moment of its own window, so a brief word
    /// between polls still registers.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Ordering::Relaxed))
    }
}

impl Default for LevelMeter {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a raw peak (linear, 0..1) to a perceptual 0..1 meter value: dBFS
/// with a -50 dB floor, so quiet speech (peaks around 0.03..0.1) still
/// visibly moves the meter instead of hugging zero.
pub fn perceptual_level(peak: f32) -> f32 {
    if peak <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * peak.log10();
    ((db + 50.0) / 50.0).clamp(0.0, 1.0)
}

#[derive(Debug)]
pub struct AudioError(pub String);

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for AudioError {}

/// A live input stream accumulating f32 mono samples.
pub struct Recording {
    stream: cpal::Stream,
    frames: Arc<Mutex<Vec<f32>>>,
    recording: Arc<AtomicBool>,
    level: Arc<LevelMeter>,
    rate: u32,
    mic_name: String,
}

/// The collected take, still at the capture rate.
pub struct Take {
    pub samples: Vec<f32>,
    pub rate: u32,
    pub mic_name: String,
}

impl Recording {
    /// Open an input stream for the configured mic, walking the candidate
    /// devices and the native/48k/44.1k/16k rate chain like the original
    /// `start_recording`.
    pub fn start(mic_name_substr: &str) -> Result<Self, AudioError> {
        let mut last_err = String::from("no input devices");
        for (device, name, native) in candidates(mic_name_substr) {
            let mut rates = vec![native];
            for rate in FALLBACK_RATES {
                if !rates.contains(&rate) {
                    rates.push(rate);
                }
            }
            for rate in rates {
                match Self::open(&device, &name, rate) {
                    Ok(recording) => {
                        eprintln!("Recording on '{name}' @ {rate} Hz");
                        return Ok(recording);
                    }
                    Err(err) => last_err = err.0,
                }
            }
        }
        Err(AudioError(last_err))
    }

    fn open(device: &Device, name: &str, rate: u32) -> Result<Self, AudioError> {
        let frames = Arc::new(Mutex::new(Vec::<f32>::new()));
        let recording = Arc::new(AtomicBool::new(true));
        let level = Arc::new(LevelMeter::new());
        // Mono first, like the original; a device that refuses 1 channel is
        // captured at its native channel count and downmixed in the callback.
        let channel_counts = {
            let native_channels = device
                .default_input_config()
                .map(|c| c.channels())
                .unwrap_or(1);
            if native_channels == 1 {
                vec![1]
            } else {
                vec![1, native_channels]
            }
        };
        let max_samples = rate as usize * MAX_TAKE_SECS;
        let mut last_err = String::new();
        for channels in channel_counts {
            let config = StreamConfig {
                channels,
                sample_rate: rate,
                buffer_size: BufferSize::Default,
            };
            let frames_cb = Arc::clone(&frames);
            let recording_cb = Arc::clone(&recording);
            let mut capped = false; // log the cap once per stream
            let level_cb = Arc::clone(&level);
            let built = device.build_input_stream(
                config,
                move |data: &[f32], _| {
                    if !recording_cb.load(Ordering::Relaxed) {
                        return;
                    }
                    // Peak over the raw interleaved chunk (any channel).
                    level_cb.fold(data);
                    let mut buf = frames_cb.lock().unwrap_or_else(|e| e.into_inner());
                    if append_capped(&mut buf, data, channels, max_samples) && !capped {
                        capped = true;
                        eprintln!(
                            "take capped at {MAX_TAKE_SECS} s of audio; dropping further samples"
                        );
                    }
                },
                |err| eprintln!("audio stream error: {err}"),
                None,
            );
            match built {
                Ok(stream) => match stream.play() {
                    Ok(()) => {
                        return Ok(Self {
                            stream,
                            frames,
                            recording,
                            level,
                            rate,
                            mic_name: name.to_string(),
                        })
                    }
                    Err(err) => last_err = err.to_string(),
                },
                Err(err) => last_err = err.to_string(),
            }
        }
        Err(AudioError(last_err))
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn mic_name(&self) -> &str {
        &self.mic_name
    }

    /// Shared handle to the live level meter; stays valid (and quiet) after
    /// the stream stops.
    pub fn level_meter(&self) -> Arc<LevelMeter> {
        Arc::clone(&self.level)
    }

    /// Stop capturing and hand back the take. The recording flag is cleared
    /// before the stream is torn down so a trailing callback can't append.
    pub fn stop(self) -> Take {
        self.recording.store(false, Ordering::Relaxed);
        drop(self.stream);
        let samples = std::mem::take(&mut *self.frames.lock().unwrap_or_else(|e| e.into_inner()));
        Take {
            samples,
            rate: self.rate,
            mic_name: self.mic_name,
        }
    }
}

/// `tiro --record-test`: temporary 1.2 verification command — records ~2 s
/// from the configured default and logs sample counts. Superseded by the real
/// recording state machine in phase 2.
pub fn record_test() {
    println!("devices: {:?}", list_mic_names());
    let recording = match Recording::start("") {
        Ok(r) => r,
        Err(err) => {
            println!("ERROR opening mic: {err}");
            return;
        }
    };
    println!(
        "recording 2 s on '{}' @ {} Hz…",
        recording.mic_name(),
        recording.rate()
    );
    let meter = recording.level_meter();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let take = recording.stop();
    let peak = meter.take_peak();
    let secs = take.samples.len() as f32 / take.rate as f32;
    let resampled = resample_to_16k(&take.samples, take.rate);
    println!(
        "captured {} samples @ {} Hz ({secs:.2} s) -> {} samples @ 16 kHz; too_short={}",
        take.samples.len(),
        take.rate,
        resampled.len(),
        too_short(take.samples.len(), take.rate),
    );
    println!(
        "peak level: {peak:.4} ({:.2} on the 0..1 meter)",
        perceptual_level(peak)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_identity_at_16k() {
        let audio = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_to_16k(&audio, 16_000), audio);
    }

    #[test]
    fn resample_halves_from_32k() {
        let audio: Vec<f32> = (0..3200).map(|i| i as f32).collect();
        let out = resample_to_16k(&audio, 32_000);
        assert_eq!(out.len(), 1600);
        // positions land exactly on every other input sample
        assert_eq!(out[0], 0.0);
        assert_eq!(out[1], 2.0);
        assert_eq!(out[799], 1598.0);
    }

    #[test]
    fn resample_interpolates_between_samples() {
        // 8 kHz -> 16 kHz doubles length; odd outputs sit halfway between inputs
        let audio = vec![0.0, 1.0, 2.0, 3.0];
        let out = resample_to_16k(&audio, 8_000);
        assert_eq!(out.len(), 8);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[1], 0.5);
        assert_eq!(out[2], 1.0);
        assert_eq!(out[7], 3.0, "clamped at the final sample");
    }

    #[test]
    fn resample_of_empty_is_empty() {
        assert!(resample_to_16k(&[], 48_000).is_empty());
    }

    #[test]
    fn null_device_is_recognized() {
        assert!(is_null_device(
            "Discard all samples (playback) or generate zero samples (capture)",
            Some("null"),
        ));
        assert!(
            is_null_device("Discard all samples (playback)", None),
            "description alone is enough"
        );
        assert!(
            is_null_device("anything", Some("null")),
            "raw PCM id alone is enough"
        );
        assert!(!is_null_device("PipeWire Sound Server", Some("pipewire")));
        assert!(!is_null_device("MIC_TEST, USB Audio", None));
        assert!(!is_null_device("Microphone (USB Audio)", Some("{0.0.1}")));
    }

    #[test]
    fn append_capped_mono_under_cap() {
        let mut buf = vec![0.5f32; 3];
        assert!(!append_capped(&mut buf, &[1.0, 2.0], 1, 10));
        assert_eq!(buf, vec![0.5, 0.5, 0.5, 1.0, 2.0]);
    }

    #[test]
    fn append_capped_mono_stops_at_cap() {
        let mut buf = vec![0.0f32; 8];
        assert!(append_capped(&mut buf, &[1.0, 2.0, 3.0], 1, 10));
        assert_eq!(buf.len(), 10);
        assert_eq!(&buf[8..], &[1.0, 2.0]);
        // once full, further data is dropped entirely and still reported
        assert!(append_capped(&mut buf, &[4.0], 1, 10));
        assert_eq!(buf.len(), 10);
    }

    #[test]
    fn append_capped_downmixes_and_caps() {
        let mut buf = Vec::new();
        // stereo frames [1,3] [5,7] [9,11] -> mono 2, 6, 10; cap at 2 frames
        assert!(append_capped(
            &mut buf,
            &[1.0, 3.0, 5.0, 7.0, 9.0, 11.0],
            2,
            2
        ));
        assert_eq!(buf, vec![2.0, 6.0]);
    }

    #[test]
    fn max_take_is_ten_minutes() {
        assert_eq!(MAX_TAKE_SECS, 600);
        // ~45 min of null-device output at 48 kHz would be capped to 10 min
        let cap = 48_000 * MAX_TAKE_SECS;
        assert!(130_887_360 > cap);
    }

    #[test]
    fn level_meter_folds_peak_and_resets_on_take() {
        let meter = LevelMeter::new();
        assert_eq!(meter.take_peak(), 0.0, "starts silent");
        meter.fold(&[0.1, -0.5, 0.2]);
        meter.fold(&[0.3, -0.05]);
        assert_eq!(meter.take_peak(), 0.5, "peak |sample| across chunks");
        assert_eq!(meter.take_peak(), 0.0, "take resets the meter");
        meter.fold(&[]);
        assert_eq!(meter.take_peak(), 0.0, "empty chunk leaves it silent");
    }

    #[test]
    fn perceptual_level_mapping() {
        assert_eq!(perceptual_level(0.0), 0.0);
        assert_eq!(perceptual_level(-1.0), 0.0, "negative peaks clamp to 0");
        assert_eq!(perceptual_level(1.0), 1.0, "full scale");
        assert_eq!(perceptual_level(2.0), 1.0, "clipped input clamps to 1");
        // -50 dB floor: 10^(-50/20) ~ 0.00316 maps to ~0
        assert!(perceptual_level(0.003).abs() < 0.01);
        // quiet speech (-20 dBFS) sits visibly mid-meter
        let quiet = perceptual_level(0.1);
        assert!((quiet - 0.6).abs() < 0.01, "0.1 -> ~0.6, got {quiet}");
        // monotonic
        assert!(perceptual_level(0.02) < perceptual_level(0.2));
    }

    #[test]
    fn too_short_boundary() {
        assert!(too_short(0, 48_000));
        assert!(too_short(14_399, 48_000), "just under 0.3 s at 48 kHz");
        assert!(!too_short(14_400, 48_000), "exactly 0.3 s at 48 kHz");
        assert!(!too_short(16_000, 16_000), "1 s at 16 kHz");
    }
}
