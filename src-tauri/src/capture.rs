//! System audio capture: record what the machine is playing (a call, a
//! video) as a take, optionally mixed with the microphone.
//!
//! Backends:
//! - Windows: WASAPI loopback. cpal opens a *playback* endpoint for input
//!   and WASAPI transparently serves the mix that endpoint renders.
//! - Linux: PipeWire. A sink's monitor is captured with `pw-record
//!   --target <sink>` (falling back to `parec --device=<sink>.monitor` on
//!   a PulseAudio-compatible server), already as 16 kHz mono f32 on a
//!   pipe. The list of sinks comes from `pactl`, like the microphone list.
//! - macOS: there is no public loopback API without a virtual device;
//!   `list_outputs` is empty, so the panel hides the feature there.
//!
//! The result is a 16 kHz mono f32 buffer, the shape every take has, so
//! the transcription pipeline does not know it came from the speakers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::audio::{self, LevelMeter, SAMPLE_RATE};

/// Playback devices (or sinks) whose output can be captured, the system
/// default first and labeled. Empty where the feature is unavailable.
pub fn list_outputs() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        linux::list_sinks()
    }
    #[cfg(windows)]
    {
        windows::list_outputs()
    }
    #[cfg(target_os = "macos")]
    {
        Vec::new()
    }
}

/// Label appended to the default playback device's row.
pub const DEFAULT_SUFFIX: &str = " (system default)";

/// A running system-audio capture.
pub struct SystemCapture {
    inner: Backend,
    frames: Arc<Mutex<Vec<f32>>>,
    level: Arc<LevelMeter>,
    /// Native rate of the captured frames (16 kHz on Linux, the device
    /// rate on Windows); `stop` resamples.
    rate: u32,
    pub source: String,
}

enum Backend {
    #[cfg(windows)]
    Cpal(cpal::Stream, Arc<AtomicBool>),
    #[cfg(target_os = "linux")]
    Child(std::process::Child, Arc<AtomicBool>),
    #[allow(dead_code)]
    None,
}

impl SystemCapture {
    /// Start capturing the named output (a substring of a `list_outputs`
    /// row; empty = the system default).
    pub fn start(output: &str) -> Result<Self, String> {
        let frames = Arc::new(Mutex::new(Vec::<f32>::new()));
        let level = Arc::new(LevelMeter::new());
        #[cfg(target_os = "linux")]
        {
            let (child, running, name) = linux::start(output, &frames, &level)?;
            return Ok(Self {
                inner: Backend::Child(child, running),
                frames,
                level,
                rate: SAMPLE_RATE,
                source: name,
            });
        }
        #[cfg(windows)]
        {
            let (stream, running, name, rate) = windows::start(output, &frames, &level)?;
            return Ok(Self {
                inner: Backend::Cpal(stream, running),
                frames,
                level,
                rate,
                source: name,
            });
        }
        #[allow(unreachable_code)]
        {
            let _ = (output, frames, level);
            Err("system audio capture is not available on this platform".into())
        }
    }

    pub fn level_meter(&self) -> Arc<LevelMeter> {
        Arc::clone(&self.level)
    }

    /// Stop and hand back the capture as 16 kHz mono.
    pub fn stop(self) -> Vec<f32> {
        match self.inner {
            #[cfg(windows)]
            Backend::Cpal(stream, running) => {
                running.store(false, Ordering::Relaxed);
                drop(stream);
            }
            #[cfg(target_os = "linux")]
            Backend::Child(mut child, running) => {
                running.store(false, Ordering::Relaxed);
                let _ = child.kill();
                let _ = child.wait();
            }
            Backend::None => {}
        }
        let samples = std::mem::take(&mut *self.frames.lock().unwrap_or_else(|e| e.into_inner()));
        audio::resample_to_16k(&samples, self.rate)
    }
}

/// Mix two 16 kHz mono tracks (system audio and the microphone) into one:
/// summed, then limited to [-1, 1]. The shorter track is padded with
/// silence; both start at the moment the capture started, so they line
/// up to within a buffer of latency, which is nothing to Whisper.
pub fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
    let n = a.len().max(b.len());
    (0..n)
        .map(|i| {
            let x = a.get(i).copied().unwrap_or(0.0) + b.get(i).copied().unwrap_or(0.0);
            x.clamp(-1.0, 1.0)
        })
        .collect()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::io::Read;
    use std::process::{Command, Stdio};

    /// One sink from `pactl --format=json list sinks`.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Sink {
        /// PipeWire object id: what `pw-record --target` needs (a node
        /// NAME is accepted by the flag but silently falls back to the
        /// default source, which is the microphone).
        pub index: u64,
        pub name: String,
        pub description: String,
    }

    pub fn parse_sinks(json: &str) -> Vec<Sink> {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(json) else {
            return Vec::new();
        };
        let Some(arr) = val.as_array() else {
            return Vec::new();
        };
        arr.iter()
            .filter_map(|s| {
                Some(Sink {
                    index: s.get("index")?.as_u64()?,
                    name: s.get("name")?.as_str()?.to_string(),
                    description: s.get("description")?.as_str()?.to_string(),
                })
            })
            .collect()
    }

    fn pactl(args: &[&str]) -> Option<String> {
        let out = Command::new("pactl")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn sinks() -> (Vec<Sink>, Option<String>) {
        let sinks = pactl(&["--format=json", "list", "sinks"])
            .map(|j| parse_sinks(&j))
            .unwrap_or_default();
        let default = pactl(&["get-default-sink"]).filter(|s| !s.is_empty());
        (sinks, default)
    }

    pub fn rows(sinks: &[Sink], default: Option<&str>) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(d) = default.and_then(|d| sinks.iter().find(|s| s.name == d)) {
            out.push(format!("{}{DEFAULT_SUFFIX}", d.description));
        }
        for s in sinks {
            if !out.iter().any(|r| r.starts_with(&s.description)) {
                out.push(s.description.clone());
            }
        }
        out
    }

    pub fn list_sinks() -> Vec<String> {
        let (sinks, default) = sinks();
        rows(&sinks, default.as_deref())
    }

    /// Resolve a row (or empty) to a sink node name.
    pub fn resolve(output: &str, sinks: &[Sink], default: Option<&str>) -> Option<Sink> {
        let needle = output.trim().to_lowercase();
        if needle.is_empty() || needle.ends_with(&DEFAULT_SUFFIX.to_lowercase()) {
            if let Some(d) = default.and_then(|d| sinks.iter().find(|s| s.name == d)) {
                return Some(d.clone());
            }
        }
        sinks
            .iter()
            .find(|s| s.description.to_lowercase().contains(&needle))
            .cloned()
            .or_else(|| sinks.first().cloned())
    }

    pub fn start(
        output: &str,
        frames: &Arc<Mutex<Vec<f32>>>,
        level: &Arc<LevelMeter>,
    ) -> Result<(std::process::Child, Arc<AtomicBool>, String), String> {
        let (all, default) = sinks();
        let sink = resolve(output, &all, default.as_deref())
            .ok_or("no playback device to capture (pactl lists no sinks)")?;
        let rate = SAMPLE_RATE.to_string();
        // pw-record first (PipeWire native, targets the sink node and
        // captures its monitor); parec for a PulseAudio server.
        let attempts: [(&str, Vec<String>); 2] = [
            (
                "pw-record",
                vec![
                    "--raw".into(),
                    "--target".into(),
                    sink.index.to_string(),
                    "--rate".into(),
                    rate.clone(),
                    "--channels".into(),
                    "1".into(),
                    "--format".into(),
                    "f32".into(),
                    "-".into(),
                ],
            ),
            (
                "parec",
                vec![
                    format!("--device={}.monitor", sink.name),
                    "--format=float32le".into(),
                    format!("--rate={rate}"),
                    "--channels=1".into(),
                    "--raw".into(),
                ],
            ),
        ];
        let mut last =
            String::from("no capture tool found (install pipewire-utils or pulseaudio-utils)");
        for (exe, args) in attempts {
            let spawned = Command::new(exe)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            let mut child = match spawned {
                Ok(c) => c,
                Err(e) => {
                    last = format!("{exe}: {e}");
                    continue;
                }
            };
            let Some(mut stdout) = child.stdout.take() else {
                let _ = child.kill();
                continue;
            };
            let running = Arc::new(AtomicBool::new(true));
            let frames_t = Arc::clone(frames);
            let level_t = Arc::clone(level);
            let running_t = Arc::clone(&running);
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 4096 * 4];
                let mut carry: Vec<u8> = Vec::new();
                loop {
                    let n = match stdout.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if !running_t.load(Ordering::Relaxed) {
                        break;
                    }
                    carry.extend_from_slice(&buf[..n]);
                    let (words, _) = carry.as_chunks::<4>();
                    let whole = words.len() * 4;
                    let chunk: Vec<f32> = words.iter().map(|b| f32::from_le_bytes(*b)).collect();
                    carry.drain(..whole);
                    level_t.fold(&chunk);
                    frames_t
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend_from_slice(&chunk);
                }
            });
            eprintln!("capture: {exe} on '{}'", sink.description);
            return Ok((child, running, sink.description));
        }
        Err(last)
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{BufferSize, SampleFormat, StreamConfig};

    fn outputs() -> Vec<(cpal::Device, String)> {
        let host = cpal::default_host();
        let mut out = Vec::new();
        if let Some(d) = host.default_output_device() {
            let name = d.name().unwrap_or_else(|_| "Default Output".into());
            out.push((d, format!("{name}{DEFAULT_SUFFIX}")));
        }
        if let Ok(devs) = host.output_devices() {
            for d in devs {
                let name = d.name().unwrap_or_default();
                if name.is_empty() || out.iter().any(|(_, n)| n.starts_with(&name)) {
                    continue;
                }
                out.push((d, name));
            }
        }
        out
    }

    pub fn list_outputs() -> Vec<String> {
        outputs().into_iter().map(|(_, n)| n).collect()
    }

    pub fn start(
        output: &str,
        frames: &Arc<Mutex<Vec<f32>>>,
        level: &Arc<LevelMeter>,
    ) -> Result<(cpal::Stream, Arc<AtomicBool>, String, u32), String> {
        let needle = output.trim().to_lowercase();
        let all = outputs();
        let (device, name) = all
            .into_iter()
            .find(|(_, n)| {
                needle.is_empty()
                    || needle.ends_with(&DEFAULT_SUFFIX.to_lowercase())
                    || n.to_lowercase().contains(&needle)
            })
            .ok_or("no playback device to capture")?;
        let cfg = device
            .default_output_config()
            .map_err(|e| format!("{name}: {e}"))?;
        let channels = cfg.channels();
        let rate = cfg.sample_rate();
        let config = StreamConfig {
            channels,
            sample_rate: rate,
            buffer_size: BufferSize::Default,
        };
        let running = Arc::new(AtomicBool::new(true));
        let frames_cb = Arc::clone(frames);
        let level_cb = Arc::clone(level);
        let running_cb = Arc::clone(&running);
        let mut scratch: Vec<f32> = Vec::new();
        let on_err = |err| eprintln!("capture stream error: {err}");
        // Loopback: an output endpoint opened for input. WASAPI renders f32
        // for the shared-mode mix; the i16 arm covers an endpoint that
        // reports otherwise.
        let stream = match cfg.sample_format() {
            SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    if !running_cb.load(Ordering::Relaxed) {
                        return;
                    }
                    scratch.clear();
                    scratch.extend(data.iter().map(|&s| f32::from(s) / 32768.0));
                    level_cb.fold(&scratch);
                    let mut buf = frames_cb.lock().unwrap_or_else(|e| e.into_inner());
                    audio::append_mono_frames(&mut buf, &scratch, channels);
                },
                on_err,
                None,
            ),
            _ => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    if !running_cb.load(Ordering::Relaxed) {
                        return;
                    }
                    scratch.clear();
                    scratch.extend_from_slice(data);
                    level_cb.fold(&scratch);
                    let mut buf = frames_cb.lock().unwrap_or_else(|e| e.into_inner());
                    audio::append_mono_frames(&mut buf, &scratch, channels);
                },
                on_err,
                None,
            ),
        }
        .map_err(|e| format!("{name}: {e}"))?;
        stream.play().map_err(|e| format!("{name}: {e}"))?;
        eprintln!("capture: loopback on '{name}' @ {rate} Hz, {channels} ch");
        Ok((stream, running, name, rate))
    }
}

/// `tiro --loopback-test [output-substring]`: capture 3 s of whatever the
/// machine is playing and report the peak and RMS, the way
/// `--record-test` does for the microphone.
pub fn loopback_test(output: &str) {
    let outputs = list_outputs();
    eprintln!("outputs: {outputs:?}");
    let cap = match SystemCapture::start(output) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("capture failed: {e}");
            return;
        }
    };
    eprintln!("capturing 3 s from '{}'…", cap.source);
    std::thread::sleep(std::time::Duration::from_secs(3));
    let peak = cap.level_meter().take_peak();
    let samples = cap.stop();
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
    eprintln!(
        "captured {} samples @ 16 kHz ({:.2} s); peak {peak:.3}, rms {rms:.4}",
        samples.len(),
        samples.len() as f32 / SAMPLE_RATE as f32
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_pads_and_limits() {
        assert_eq!(mix(&[0.5, 0.5], &[0.25]), vec![0.75, 0.5]);
        assert_eq!(mix(&[0.9], &[0.9]), vec![1.0]);
        assert!(mix(&[], &[]).is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sinks_parse_and_resolve() {
        let json = r#"[{"index":58,"name":"alsa_output.pci.analog-stereo","description":"Built-in Audio Analog Stereo"},
                       {"index":97,"name":"bluez_output.AA","description":"WH-1000XM4"}]"#;
        let sinks = linux::parse_sinks(json);
        assert_eq!(sinks.len(), 2);
        let rows = linux::rows(&sinks, Some("bluez_output.AA"));
        assert_eq!(rows[0], format!("WH-1000XM4{DEFAULT_SUFFIX}"));
        assert_eq!(rows.len(), 2, "{rows:?}");
        let d = linux::resolve("", &sinks, Some("bluez_output.AA")).unwrap();
        assert_eq!((d.index, d.name.as_str()), (97, "bluez_output.AA"));
        let b = linux::resolve("built-in", &sinks, None).unwrap();
        assert_eq!(b.name, "alsa_output.pci.analog-stereo");
        assert!(linux::parse_sinks("not json").is_empty());
    }
}
