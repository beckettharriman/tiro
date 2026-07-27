//! Sound cues, ported from the original's synthesized sine tones: soft
//! attack/release-shaped notes pre-rendered at 44.1 kHz, played non-blocking
//! on the default output device. Frequencies, durations, per-note volumes,
//! the 6 ms linear attack, the cosine² release, and the 12 ms inter-note gap
//! are copied exactly from the original `_tone`/`_seq`/`_CUES`.

use std::collections::HashMap;
use std::num::NonZero;
use std::sync::OnceLock;
use std::time::Duration;

use rodio::buffer::SamplesBuffer;

use crate::config::ConfigStore;

const SR_OUT: u32 = 44_100;

/// A soft sine (or chord) with fast attack + smooth cosine release.
fn tone(freqs: &[f64], dur: f64, vol: f64) -> Vec<f32> {
    let n = (SR_OUT as f64 * dur) as usize;
    // np.linspace(0, dur, n, endpoint=False): t_i = i * dur / n (the step is
    // dur/n, not exactly 1/SR because n is truncated).
    let mut wave: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 * dur / n as f64;
            freqs
                .iter()
                .map(|f| (2.0 * std::f64::consts::PI * f * t).sin())
                .sum::<f64>()
                / freqs.len() as f64
        })
        .collect();
    let a = ((SR_OUT as f64 * 0.006) as usize).max(1); // 6 ms attack
    let r = ((SR_OUT as f64 * (dur * 0.7).min(0.13)) as usize).max(1); // smooth release
    for (i, sample) in wave.iter_mut().enumerate().take(a.min(n)) {
        // np.linspace(0, 1, a) is endpoint-inclusive
        let g = if a > 1 {
            i as f64 / (a - 1) as f64
        } else {
            0.0
        };
        *sample *= g;
    }
    let start = n.saturating_sub(r);
    for (k, sample) in wave[start..].iter_mut().enumerate() {
        // np.cos(np.linspace(0, pi/2, r)) ** 2, endpoint-inclusive
        let x = if r > 1 {
            k as f64 * (std::f64::consts::PI / 2.0) / (r - 1) as f64
        } else {
            0.0
        };
        *sample *= x.cos().powi(2);
    }
    wave.iter().map(|s| (s * vol) as f32).collect()
}

/// Notes concatenated with `gap` seconds of silence after each one.
fn seq(notes: &[(&[f64], f64, f64)], gap: f64) -> Vec<f32> {
    let mut out = Vec::new();
    for (freqs, dur, vol) in notes {
        out.extend(tone(freqs, *dur, *vol));
        if gap > 0.0 {
            out.extend(std::iter::repeat_n(0.0f32, (SR_OUT as f64 * gap) as usize));
        }
    }
    out
}

const DEFAULT_GAP: f64 = 0.012;

/// The six cues, pre-rendered once — calm, rounded, musical.
fn cues() -> &'static HashMap<&'static str, Vec<f32>> {
    static CUES: OnceLock<HashMap<&'static str, Vec<f32>>> = OnceLock::new();
    CUES.get_or_init(|| {
        HashMap::from([
            // C5 -> G5 (rising)
            (
                "start",
                seq(
                    &[(&[523.25][..], 0.055, 0.16), (&[783.99][..], 0.075, 0.18)],
                    DEFAULT_GAP,
                ),
            ),
            // G5 -> D5 (settling)
            (
                "stop",
                seq(
                    &[(&[783.99][..], 0.055, 0.15), (&[587.33][..], 0.085, 0.15)],
                    DEFAULT_GAP,
                ),
            ),
            // E5+B5 soft bell
            ("done", seq(&[(&[659.25, 987.77][..], 0.18, 0.17)], 0.0)),
            // quiet low fall (G4 -> Eb4)
            (
                "cancel",
                seq(
                    &[(&[392.00][..], 0.07, 0.11), (&[311.13][..], 0.11, 0.09)],
                    DEFAULT_GAP,
                ),
            ),
            // tiny high tick (C6)
            ("copy", seq(&[(&[1046.50][..], 0.045, 0.13)], 0.0)),
            // A4 -> E4 (calm but attention-getting)
            (
                "error",
                seq(
                    &[(&[440.00][..], 0.07, 0.15), (&[329.63][..], 0.12, 0.15)],
                    DEFAULT_GAP,
                ),
            ),
        ])
    })
}

/// Play `name` scaled by `volume` on the default output device, without
/// blocking the caller. Each cue opens its own short-lived output stream on a
/// background thread (the original's sd.play equivalent); playback failures
/// are silently ignored, cues are best-effort.
fn play_samples(samples: Vec<f32>) {
    std::thread::spawn(move || {
        let Ok(sink) = rodio::DeviceSinkBuilder::open_default_sink() else {
            return;
        };
        let secs = samples.len() as f64 / SR_OUT as f64;
        let (Some(channels), Some(rate)) = (NonZero::new(1u16), NonZero::new(SR_OUT)) else {
            return;
        };
        sink.mixer()
            .add(SamplesBuffer::new(channels, rate, samples));
        // keep the device open until the cue has fully sounded
        std::thread::sleep(Duration::from_secs_f64(secs + 0.05));
    });
}

/// `play_cue`: honors the `beeps` toggle and `sound_volume` (clamped to the
/// documented 0.0–1.5 so a stray edit can't silence or blow out the cues; an
/// unparseable value skips the cue, like the original's catch-all).
pub fn play_cue(cfg: &ConfigStore, name: &str) {
    if !cfg.get_bool("beeps") {
        return;
    }
    let Some(samples) = cues().get(name) else {
        return;
    };
    let Ok(vol) = cfg.get("sound_volume").parse::<f64>() else {
        return;
    };
    let vol = vol.clamp(0.0, 1.5) as f32;
    play_samples(samples.iter().map(|s| s * vol).collect());
}

/// `tiro --cue-test`: temporary check that plays each cue in order.
pub fn cue_test() {
    for name in ["start", "stop", "done", "cancel", "copy", "error"] {
        println!("cue: {name}");
        let Some(samples) = cues().get(name) else {
            continue;
        };
        play_samples(samples.clone());
        std::thread::sleep(Duration::from_millis(600));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected_tone_len(dur: f64) -> usize {
        (SR_OUT as f64 * dur) as usize
    }

    const GAP_LEN: usize = 529; // int(44100 * 0.012)

    #[test]
    fn cue_lengths_match_original() {
        let c = cues();
        assert_eq!(
            c["start"].len(),
            expected_tone_len(0.055) + GAP_LEN + expected_tone_len(0.075) + GAP_LEN
        );
        assert_eq!(
            c["stop"].len(),
            expected_tone_len(0.055) + GAP_LEN + expected_tone_len(0.085) + GAP_LEN
        );
        assert_eq!(c["done"].len(), expected_tone_len(0.18), "done has no gap");
        assert_eq!(c["copy"].len(), expected_tone_len(0.045), "copy has no gap");
        assert_eq!(
            c["cancel"].len(),
            expected_tone_len(0.07) + GAP_LEN + expected_tone_len(0.11) + GAP_LEN
        );
        assert_eq!(
            c["error"].len(),
            expected_tone_len(0.07) + GAP_LEN + expected_tone_len(0.12) + GAP_LEN
        );
    }

    #[test]
    fn tone_attack_starts_at_zero_and_release_lands_near_zero() {
        let t = tone(&[440.0], 0.07, 0.15);
        assert_eq!(t[0], 0.0, "attack begins silent");
        assert!(t.last().unwrap().abs() < 1e-6, "cosine release ends ~0");
        let peak = t.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak <= 0.15 + 1e-6, "volume bound respected, got {peak}");
        assert!(peak > 0.10, "tone actually sounds, got {peak}");
    }

    #[test]
    fn tone_frequency_is_right() {
        // count zero crossings of the un-enveloped middle of a 1 s 440 Hz tone
        let t = tone(&[440.0], 1.0, 1.0);
        let mid = &t[SR_OUT as usize / 4..3 * (SR_OUT as usize) / 4];
        let crossings = mid
            .windows(2)
            .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
            .count();
        // 0.5 s of 440 Hz has ~440 zero crossings (2 per cycle)
        assert!((430..=450).contains(&crossings), "got {crossings}");
    }

    #[test]
    fn chord_averages_voices() {
        let t = tone(&[659.25, 987.77], 0.18, 0.17);
        let peak = t.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak <= 0.17 + 1e-6, "chord divided by voice count");
    }

    #[test]
    fn unknown_cue_is_absent() {
        assert!(!cues().contains_key("nope"));
    }
}
