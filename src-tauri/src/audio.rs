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
use cpal::{BufferSize, Device, SampleFormat, StreamConfig};

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

/// PipeWire-aware device listing and selection, Linux only. The raw cpal/ALSA
/// enumeration is PCM-plumbing soup ("PipeWire Sound Server", "Default ALSA
/// Output (currently ...)", one row per plugin layer, cards the desktop has
/// disabled). `pactl` — PipeWire's Pulse frontend, standard on Fedora — knows
/// exactly what the desktop knows: the real, active sources with their human
/// descriptions, minus anything disabled in the sound settings. When it is
/// available the mic list is built from it and the configured name is mapped
/// back to a capture route; when it is missing or errors everything falls
/// back to the raw cpal behavior below.
#[cfg(target_os = "linux")]
mod pactl {
    use std::process::Command;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Label appended to the default source's row in the mic selector.
    pub const DEFAULT_SUFFIX: &str = " (system default)";

    /// One non-monitor Pulse/PipeWire source.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Source {
        /// Node name, e.g. `alsa_input.usb-ABC_MIC_TEST-00.iec958-stereo`.
        pub name: String,
        /// Human description, e.g. `MIC_TEST Digital Stereo (IEC958)`.
        pub description: String,
        /// `alsa.card_name` (falling back to `alsa.long_card_name`) — the
        /// bridge to cpal's ALSA descriptions, which are "<card>, <device>".
        pub card_name: Option<String>,
    }

    /// Parse `pactl --format=json list sources`, dropping monitor sources
    /// (name ends in `.monitor`, and belt-and-braces `device.class` =
    /// "monitor") and malformed entries.
    pub fn parse_sources(json: &str) -> Option<Vec<Source>> {
        let val: serde_json::Value = serde_json::from_str(json).ok()?;
        let mut out = Vec::new();
        for s in val.as_array()? {
            let Some(name) = s.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            if name.ends_with(".monitor") {
                continue;
            }
            let props = s.get("properties");
            let class = props
                .and_then(|p| p.get("device.class"))
                .and_then(|v| v.as_str());
            if class == Some("monitor") {
                continue;
            }
            let Some(description) = s.get("description").and_then(|v| v.as_str()) else {
                continue;
            };
            let card_name = props
                .and_then(|p| {
                    p.get("alsa.card_name")
                        .or_else(|| p.get("alsa.long_card_name"))
                })
                .and_then(|v| v.as_str())
                .map(str::to_string);
            out.push(Source {
                name: name.to_string(),
                description: description.to_string(),
                card_name,
            });
        }
        Some(out)
    }

    /// The mic selector's rows: the default source first, labeled, then the
    /// remaining source descriptions in pactl order (deduplicated).
    pub fn mic_names(sources: &[Source], default_name: Option<&str>) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(def) = default_name.and_then(|d| sources.iter().find(|s| s.name == d)) {
            names.push(format!("{}{DEFAULT_SUFFIX}", def.description));
        }
        for s in sources {
            if Some(s.name.as_str()) == default_name {
                continue;
            }
            if !names.contains(&s.description) {
                names.push(s.description.clone());
            }
        }
        names
    }

    /// How to capture for a configured `mic_name`, given the live sources.
    #[derive(Debug, Clone, PartialEq)]
    pub enum Plan {
        /// Open the host default input (ALSA `default` -> PipeWire -> the
        /// desktop's default source). Sharing-safe: no exclusive hw open.
        SystemDefault,
        /// A non-default source was chosen: open the ALSA card whose cpal
        /// description contains this `alsa.card_name`.
        Card(String),
        /// The name matches no pactl source — old configs ("MIC_TEST",
        /// "pipewire") keep the raw cpal substring behavior.
        Legacy,
    }

    /// Map the configured mic name onto a capture plan. Empty and
    /// `DEFAULT_SUFFIX`-labeled names mean "follow the system default"; a
    /// name matching the default source's description also routes through
    /// the default path (PipeWire shares the device, a raw hw open would
    /// fight it for exclusive access).
    pub fn plan(mic_name: &str, sources: &[Source], default_name: Option<&str>) -> Plan {
        let needle = mic_name.trim().to_lowercase();
        if needle.is_empty() || needle.ends_with(&DEFAULT_SUFFIX.to_lowercase()) {
            return Plan::SystemDefault;
        }
        match sources
            .iter()
            .find(|s| s.description.to_lowercase().contains(&needle))
        {
            Some(s) if Some(s.name.as_str()) == default_name => Plan::SystemDefault,
            Some(s) => match &s.card_name {
                Some(card) => Plan::Card(card.clone()),
                None => Plan::SystemDefault,
            },
            None => Plan::Legacy,
        }
    }

    struct Cache {
        at: Instant,
        data: Option<(Vec<Source>, Option<String>)>,
    }

    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

    /// How long a snapshot stays fresh; the panel polls the list, and a
    /// couple of seconds of staleness is invisible next to two `pactl`
    /// spawns per call.
    const TTL: Duration = Duration::from_secs(3);

    /// The current sources + default source name, cached briefly. `None`
    /// when pactl is missing, errors, or reports no sources — callers then
    /// fall back to the raw cpal path.
    pub fn snapshot() -> Option<(Vec<Source>, Option<String>)> {
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = cache.as_ref() {
            if c.at.elapsed() < TTL {
                return c.data.clone();
            }
        }
        let data = query();
        let out = data.clone();
        *cache = Some(Cache {
            at: Instant::now(),
            data,
        });
        out
    }

    fn query() -> Option<(Vec<Source>, Option<String>)> {
        let out = Command::new("pactl")
            .args(["--format=json", "list", "sources"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let sources = parse_sources(&String::from_utf8_lossy(&out.stdout))?;
        if sources.is_empty() {
            return None;
        }
        let default_name = Command::new("pactl")
            .arg("get-default-source")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());
        Some((sources, default_name))
    }
}

/// Unique input-device names, for the panel's mic selector. On Linux with
/// PipeWire's `pactl` available this is the desktop's own list — real,
/// enabled sources with the default one first (see `mod pactl`); otherwise
/// the raw cpal enumeration.
pub fn list_mic_names() -> Vec<String> {
    #[cfg(target_os = "linux")]
    if let Some((sources, default_name)) = pactl::snapshot() {
        return pactl::mic_names(&sources, default_name.as_deref());
    }
    cpal_mic_names()
}

/// The raw cpal enumeration (always the source of truth on Windows).
fn cpal_mic_names() -> Vec<String> {
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

/// Every input device (minus the ALSA null PCM) in enumeration order.
fn all_inputs() -> Vec<(Device, String, u32)> {
    let host = cpal::default_host();
    host.input_devices()
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
        .unwrap_or_default()
}

/// `all` reordered so the host DEFAULT input device leads: on ALSA that is
/// the `default` PCM, which follows the desktop's (PipeWire's) configured
/// default source; on WASAPI it is the OS default input.
fn default_first(all: Vec<(Device, String, u32)>) -> Vec<(Device, String, u32)> {
    let host = cpal::default_host();
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

/// Matching input devices ordered by preference. On Linux with pactl
/// available the configured name is first mapped against the live sources
/// (see `pactl::plan`): the default source routes through the host default
/// device, a non-default source routes to its ALSA card (all of that card's
/// PCM entries, so the rate/format fallbacks in `Recording::open` get their
/// shot at each plugin layer), and an unmatched name falls through to the
/// raw behavior. The raw behavior — and the only one on Windows — is a
/// case-insensitive substring match on the cpal device description; an empty
/// substring (fresh install), or one that matches nothing, prefers the host
/// default input device with the remaining devices as fallbacks. The ALSA
/// null device is filtered out entirely (see `is_null_device`) — previously
/// it sat first in enumeration order and won the "first device that opens"
/// walk.
fn candidates(name_substr: &str) -> Vec<(Device, String, u32)> {
    #[cfg(target_os = "linux")]
    if let Some((sources, default_name)) = pactl::snapshot() {
        match pactl::plan(name_substr, &sources, default_name.as_deref()) {
            pactl::Plan::SystemDefault => return default_first(all_inputs()),
            pactl::Plan::Card(card) => {
                let needle = card.to_lowercase();
                let (matched, rest): (Vec<_>, Vec<_>) = all_inputs()
                    .into_iter()
                    .partition(|(_, name, _)| name.to_lowercase().contains(&needle));
                // All of one card's PCM entries share a description; several
                // distinct descriptions would mean the card name was too
                // generic to trust — fall back to the default device.
                let mut descs: Vec<&str> = matched.iter().map(|(_, n, _)| n.as_str()).collect();
                descs.sort_unstable();
                descs.dedup();
                if descs.len() == 1 {
                    let mut items = matched;
                    items.extend(default_first(rest));
                    return items;
                }
                let mut all = matched;
                all.extend(rest);
                return default_first(all);
            }
            pactl::Plan::Legacy => {}
        }
    }
    let mut all = all_inputs();
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
    default_first(all)
}

/// Input sample types the capture path can open, each converted to f32 in
/// the stream callback. The original only ever requested f32 — fine on
/// WASAPI (f32 is native) and on ALSA's converting PCMs (`default`,
/// `sysdefault`, `plughw`), but raw hw PCMs (`front:`/`hw:`) typically only
/// speak S16_LE/S32_LE and rejected every rate/channel combination with
/// "Sample format f32 is not supported", which read as "No microphone".
trait InputSample: cpal::SizedSample {
    fn to_f32(self) -> f32;
}

impl InputSample for f32 {
    fn to_f32(self) -> f32 {
        self
    }
}

impl InputSample for i16 {
    fn to_f32(self) -> f32 {
        f32::from(self) / 32_768.0
    }
}

impl InputSample for i32 {
    fn to_f32(self) -> f32 {
        self as f32 / 2_147_483_648.0
    }
}

impl InputSample for u16 {
    fn to_f32(self) -> f32 {
        (f32::from(self) - 32_768.0) / 32_768.0
    }
}

fn format_label(format: SampleFormat) -> &'static str {
    match format {
        SampleFormat::F32 => "f32",
        SampleFormat::I16 => "i16",
        SampleFormat::I32 => "i32",
        SampleFormat::U16 => "u16",
        _ => "?",
    }
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
    format: &'static str,
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
                        eprintln!("Recording on '{name}' @ {rate} Hz ({})", recording.format);
                        return Ok(recording);
                    }
                    Err(err) => last_err = err.0,
                }
            }
        }
        Err(AudioError(last_err))
    }

    fn open(device: &Device, name: &str, rate: u32) -> Result<Self, AudioError> {
        let default_cfg = device.default_input_config().ok();
        // Mono first, like the original; a device that refuses 1 channel is
        // captured at its native channel count and downmixed in the callback.
        // An unqueryable device (e.g. busy raw hw PCM) still gets a stereo
        // attempt — most hw capture PCMs are exactly 2-channel.
        let channel_counts = match default_cfg.as_ref().map(|c| c.channels()) {
            Some(1) => vec![1],
            Some(n) => vec![1, n],
            None => vec![1, 2],
        };
        // The device's native format first, then the rest of the supported
        // set. WASAPI keeps its old behavior (f32 native, first try wins);
        // raw ALSA hw PCMs land on their real S16/S32 formats.
        let mut formats: Vec<SampleFormat> = Vec::new();
        if let Some(f) = default_cfg.map(|c| c.sample_format()) {
            if format_label(f) != "?" {
                formats.push(f);
            }
        }
        for f in [
            SampleFormat::F32,
            SampleFormat::I16,
            SampleFormat::I32,
            SampleFormat::U16,
        ] {
            if !formats.contains(&f) {
                formats.push(f);
            }
        }
        let frames = Arc::new(Mutex::new(Vec::<f32>::new()));
        let recording = Arc::new(AtomicBool::new(true));
        let level = Arc::new(LevelMeter::new());
        let max_samples = rate as usize * MAX_TAKE_SECS;
        let mut last_err = String::new();
        for channels in channel_counts {
            for &format in &formats {
                let config = StreamConfig {
                    channels,
                    sample_rate: rate,
                    buffer_size: BufferSize::Default,
                };
                let built = match format {
                    SampleFormat::F32 => Self::build_stream::<f32>(
                        device,
                        config,
                        channels,
                        max_samples,
                        &frames,
                        &recording,
                        &level,
                    ),
                    SampleFormat::I16 => Self::build_stream::<i16>(
                        device,
                        config,
                        channels,
                        max_samples,
                        &frames,
                        &recording,
                        &level,
                    ),
                    SampleFormat::I32 => Self::build_stream::<i32>(
                        device,
                        config,
                        channels,
                        max_samples,
                        &frames,
                        &recording,
                        &level,
                    ),
                    SampleFormat::U16 => Self::build_stream::<u16>(
                        device,
                        config,
                        channels,
                        max_samples,
                        &frames,
                        &recording,
                        &level,
                    ),
                    _ => continue,
                };
                match built.and_then(|s| s.play().map(|()| s).map_err(|e| e.to_string())) {
                    Ok(stream) => {
                        return Ok(Self {
                            stream,
                            frames,
                            recording,
                            level,
                            rate,
                            mic_name: name.to_string(),
                            format: format_label(format),
                        })
                    }
                    Err(err) => last_err = err,
                }
            }
        }
        Err(AudioError(last_err))
    }

    /// Build one typed input stream converting samples to f32 in the
    /// callback: peak-metering and cap-guarded accumulation both run on the
    /// converted interleaved chunk, exactly like the old f32-only path. The
    /// scratch buffer reaches steady-state capacity after the first chunks,
    /// so the callback stays allocation-free thereafter.
    fn build_stream<T: InputSample>(
        device: &Device,
        config: StreamConfig,
        channels: u16,
        max_samples: usize,
        frames: &Arc<Mutex<Vec<f32>>>,
        recording: &Arc<AtomicBool>,
        level: &Arc<LevelMeter>,
    ) -> Result<cpal::Stream, String> {
        let frames_cb = Arc::clone(frames);
        let recording_cb = Arc::clone(recording);
        let level_cb = Arc::clone(level);
        let mut capped = false; // log the cap once per stream
        let mut scratch: Vec<f32> = Vec::new();
        device
            .build_input_stream(
                config,
                move |data: &[T], _| {
                    if !recording_cb.load(Ordering::Relaxed) {
                        return;
                    }
                    scratch.clear();
                    scratch.extend(data.iter().map(|s| s.to_f32()));
                    // Peak over the raw interleaved chunk (any channel).
                    level_cb.fold(&scratch);
                    let mut buf = frames_cb.lock().unwrap_or_else(|e| e.into_inner());
                    if append_capped(&mut buf, &scratch, channels, max_samples) && !capped {
                        capped = true;
                        eprintln!(
                            "take capped at {MAX_TAKE_SECS} s of audio; dropping further samples"
                        );
                    }
                },
                |err| eprintln!("audio stream error: {err}"),
                None,
            )
            .map_err(|e| e.to_string())
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The sample format the stream actually opened with ("f32", "i16", …).
    pub fn sample_format(&self) -> &'static str {
        self.format
    }

    pub fn mic_name(&self) -> &str {
        &self.mic_name
    }

    /// Shared handle to the live level meter; stays valid (and quiet) after
    /// the stream stops.
    pub fn level_meter(&self) -> Arc<LevelMeter> {
        Arc::clone(&self.level)
    }

    /// Drop everything captured so far, keeping the stream and level meter
    /// alive. The settings-meter monitor reuses the take pipeline's stream
    /// as a pure level tap: draining on every level poll (~15 Hz) keeps it
    /// from ever accumulating a take's worth of audio.
    pub fn discard_frames(&self) {
        self.frames
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
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

/// `tiro --record-test [mic-substring]`: headless verification command —
/// records ~2 s (from the system default, or from the mic matching the
/// optional substring exactly like the `mic_name` setting) and logs sample
/// counts, the opened format, and the peak level.
pub fn record_test() {
    let args: Vec<String> = std::env::args().collect();
    let mic = args
        .iter()
        .position(|a| a == "--record-test")
        .and_then(|i| args.get(i + 1))
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_default();
    println!("devices: {:?}", list_mic_names());
    let recording = match Recording::start(&mic) {
        Ok(r) => r,
        Err(err) => {
            println!("ERROR opening mic: {err}");
            return;
        }
    };
    println!(
        "recording 2 s on '{}' @ {} Hz ({})…",
        recording.mic_name(),
        recording.rate(),
        recording.sample_format()
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

    #[test]
    fn input_sample_conversions() {
        assert_eq!(InputSample::to_f32(0.25f32), 0.25);
        assert_eq!(InputSample::to_f32(0i16), 0.0);
        assert_eq!(InputSample::to_f32(i16::MIN), -1.0);
        assert!((InputSample::to_f32(i16::MAX) - 1.0).abs() < 1e-4);
        assert_eq!(InputSample::to_f32(i32::MIN), -1.0);
        assert!((InputSample::to_f32(i32::MAX) - 1.0).abs() < 1e-6);
        assert_eq!(InputSample::to_f32(32_768u16), 0.0, "u16 midpoint is zero");
        assert_eq!(InputSample::to_f32(0u16), -1.0);
        assert!((InputSample::to_f32(u16::MAX) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn format_labels() {
        assert_eq!(format_label(SampleFormat::F32), "f32");
        assert_eq!(format_label(SampleFormat::I16), "i16");
        assert_eq!(format_label(SampleFormat::I32), "i32");
        assert_eq!(format_label(SampleFormat::U16), "u16");
        assert_eq!(format_label(SampleFormat::U8), "?", "unhandled formats");
    }
}

#[cfg(all(test, target_os = "linux"))]
mod live_tests {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait};

    /// Manual hardware check (`cargo test -- --ignored`): open a raw ALSA
    /// `front:`/`hw:` capture PCM — which rejects f32 — and prove the format
    /// fallback records real samples through the integer path. Skips (passes
    /// vacuously) when no such free device exists on the machine.
    #[test]
    #[ignore = "needs live audio hardware; run manually"]
    fn raw_hw_pcm_records_via_integer_format() {
        let host = cpal::default_host();
        let Ok(devices) = host.input_devices() else {
            return;
        };
        for device in devices {
            let Ok(desc) = device.description() else {
                continue;
            };
            let raw = desc
                .driver()
                .is_some_and(|d| d.starts_with("front:") || d.starts_with("hw:"));
            if !raw {
                continue;
            }
            let name = desc.name().to_string();
            let Ok(rec) = Recording::open(&device, &name, 48_000) else {
                continue; // busy or refuses 48 kHz entirely
            };
            assert_ne!(
                rec.sample_format(),
                "f32",
                "raw hw PCMs don't speak f32; the fallback must pick an integer format"
            );
            std::thread::sleep(std::time::Duration::from_secs(1));
            let format = rec.sample_format();
            let take = rec.stop();
            println!(
                "captured {} samples ({format}) from '{name}'",
                take.samples.len()
            );
            assert!(
                !too_short(take.samples.len(), take.rate),
                "a second of capture must yield real samples"
            );
            assert!(
                take.samples.iter().all(|s| s.is_finite() && s.abs() <= 1.0),
                "converted samples stay in [-1, 1]"
            );
            return;
        }
        println!("no free raw hw capture PCM on this machine; skipping");
    }
}

#[cfg(all(test, target_os = "linux"))]
mod pactl_tests {
    use super::pactl::{mic_names, parse_sources, plan, Plan, Source, DEFAULT_SUFFIX};

    /// Trimmed-down real `pactl --format=json list sources` output: a sink
    /// monitor, the built-in mic, a USB mic, one malformed entry, and one
    /// source without ALSA properties.
    const PACTL_JSON: &str = r#"[
      {"index":51,"name":"alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
       "description":"Monitor of Built-in Audio Analog Stereo",
       "properties":{"device.class":"monitor","alsa.card_name":"HDA Intel PCH"}},
      {"index":52,"name":"alsa_input.pci-0000_00_1f.3.analog-stereo",
       "description":"Built-in Audio Analog Stereo",
       "properties":{"device.class":"sound","alsa.card_name":"HDA Intel PCH",
                     "alsa.long_card_name":"HDA Intel PCH at 0x6044118000 irq 188"}},
      {"index":1698,"name":"alsa_input.usb-ABC_MIC_TEST-00.iec958-stereo",
       "description":"MIC_TEST Digital Stereo (IEC958)",
       "properties":{"device.class":"sound","alsa.card_name":"MIC_TEST"}},
      {"index":9999},
      {"index":77,"name":"echo-cancel-source",
       "description":"Echo-Cancel Source","properties":{}}
    ]"#;

    const DEFAULT: &str = "alsa_input.usb-ABC_MIC_TEST-00.iec958-stereo";

    fn sources() -> Vec<Source> {
        parse_sources(PACTL_JSON).expect("canned JSON parses")
    }

    #[test]
    fn parse_drops_monitors_and_malformed_entries() {
        let s = sources();
        assert_eq!(s.len(), 3, "monitor and nameless entries dropped");
        assert_eq!(s[0].description, "Built-in Audio Analog Stereo");
        assert_eq!(s[0].card_name.as_deref(), Some("HDA Intel PCH"));
        assert_eq!(s[1].description, "MIC_TEST Digital Stereo (IEC958)");
        assert_eq!(s[1].card_name.as_deref(), Some("MIC_TEST"));
        assert_eq!(s[2].description, "Echo-Cancel Source");
        assert_eq!(s[2].card_name, None, "no ALSA properties");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_sources("not json").is_none());
        assert!(parse_sources("{\"a\":1}").is_none(), "not an array");
        assert_eq!(parse_sources("[]"), Some(Vec::new()));
    }

    #[test]
    fn monitor_suffix_alone_excludes() {
        let json = r#"[{"name":"x.monitor","description":"Monitor of X"}]"#;
        assert_eq!(parse_sources(json), Some(Vec::new()));
    }

    #[test]
    fn mic_names_default_first_and_labeled() {
        let names = mic_names(&sources(), Some(DEFAULT));
        assert_eq!(
            names,
            vec![
                format!("MIC_TEST Digital Stereo (IEC958){DEFAULT_SUFFIX}"),
                "Built-in Audio Analog Stereo".to_string(),
                "Echo-Cancel Source".to_string(),
            ]
        );
    }

    #[test]
    fn mic_names_without_known_default() {
        let names = mic_names(&sources(), None);
        assert_eq!(names.len(), 3);
        assert_eq!(names[0], "Built-in Audio Analog Stereo", "pactl order");
        let names = mic_names(&sources(), Some("gone-source"));
        assert_eq!(names.len(), 3, "stale default name just goes unlabeled");
    }

    #[test]
    fn plan_empty_and_labeled_names_follow_system_default() {
        let s = sources();
        assert_eq!(plan("", &s, Some(DEFAULT)), Plan::SystemDefault);
        assert_eq!(plan("   ", &s, Some(DEFAULT)), Plan::SystemDefault);
        assert_eq!(
            plan(
                &format!("Built-in Audio Analog Stereo{DEFAULT_SUFFIX}"),
                &s,
                Some(DEFAULT)
            ),
            Plan::SystemDefault,
            "a stale labeled entry still means 'the default'"
        );
    }

    #[test]
    fn plan_default_source_routes_through_default_device() {
        // The chosen source IS the default: share it via PipeWire's default
        // path instead of fighting for the raw hw device.
        assert_eq!(
            plan("MIC_TEST", &sources(), Some(DEFAULT)),
            Plan::SystemDefault
        );
    }

    #[test]
    fn plan_non_default_source_maps_to_its_alsa_card() {
        assert_eq!(
            plan("built-in audio", &sources(), Some(DEFAULT)),
            Plan::Card("HDA Intel PCH".to_string()),
            "case-insensitive substring on the description"
        );
        assert_eq!(
            plan("MIC_TEST", &sources(), Some("other-source")),
            Plan::Card("MIC_TEST".to_string())
        );
    }

    #[test]
    fn plan_source_without_card_falls_back_to_default() {
        assert_eq!(
            plan("Echo-Cancel", &sources(), Some(DEFAULT)),
            Plan::SystemDefault
        );
    }

    #[test]
    fn plan_unmatched_name_is_legacy() {
        assert_eq!(plan("pipewire", &sources(), Some(DEFAULT)), Plan::Legacy);
        assert_eq!(plan("Webcam C920", &sources(), None), Plan::Legacy);
    }
}
