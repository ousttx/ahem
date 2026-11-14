//! How much of a recording is speech, judged by loudness alone (no VAD model yet).
//!
//! Frames of 20 ms; the noise level is the 10th percentile of frame loudness; a frame counts
//! as speech above `max(ABSOLUTE_FLOOR, min(noise × ABOVE_NOISE, LOUD))`. The cap keeps
//! continuous speech (whose quietest frames are still loud) from raising the bar too high.

use std::time::Duration;

pub const FRAMES_PER_SECOND: u32 = 50;
/// About −48 dBFS: below this nothing is speech.
const ABSOLUTE_FLOOR: f32 = 0.004;
const ABOVE_NOISE: f32 = 2.5;
const LOUD: f32 = 0.02;

/// About −80 dBFS: below this at every sample there is no sound at all, only the zeros of a gap
/// or of a silenced echo, through Opus.
const DIGITAL_SILENCE: f32 = 0.0001;

/// Nothing at all is heard: a gap filled in a meeting track, an echo silenced.
pub fn digital_silence(samples: &[f32]) -> bool {
    samples.iter().all(|sample| sample.abs() < DIGITAL_SILENCE)
}

pub fn speech_duration(samples: &[f32], rate: u32) -> Duration {
    let frame = (rate / FRAMES_PER_SECOND).max(1) as usize;
    let speech = speech_frames(samples, rate).into_iter().filter(|speech| *speech).count();
    Duration::from_secs_f64((speech * frame) as f64 / f64::from(rate.max(1)))
}

/// Whether each 20 ms frame is speech.
pub fn speech_frames(samples: &[f32], rate: u32) -> Vec<bool> {
    let frame = (rate / FRAMES_PER_SECOND).max(1) as usize;
    let levels: Vec<f32> = samples.chunks(frame).map(rms).collect();
    if levels.is_empty() {
        return Vec::new();
    }
    let mut sorted = levels.clone();
    sorted.sort_by(f32::total_cmp);
    let noise = sorted[sorted.len() / 10];
    let threshold = (noise * ABOVE_NOISE).clamp(ABSOLUTE_FLOOR, LOUD);
    levels.into_iter().map(|level| level > threshold).collect()
}

fn rms(frame: &[f32]) -> f32 {
    (frame.iter().map(|sample| sample * sample).sum::<f32>() / frame.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;
    use std::path::Path;

    use super::*;

    /// The zeros of a gap are silence; the quietest room is not.
    #[test]
    fn only_zeros_are_digital_silence() {
        assert!(digital_silence(&[0.0; 16_000]));
        assert!(digital_silence(&[0.00002, -0.00003, 0.0]));
        let room: Vec<f32> = (0..16_000).map(|i| 0.001 * (i as f32 * 0.3).sin()).collect();
        assert!(!digital_silence(&room));
    }

    const RATE: u32 = 16_000;

    /// Deterministic white noise with the given RMS.
    fn noise(seconds: f32, level: f32) -> Vec<f32> {
        let mut state: u32 = 12_345;
        (0..(RATE as f32 * seconds) as usize)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                // Uniform in [-1, 1) has RMS 1/√3.
                ((state >> 8) as f32 / (1 << 23) as f32 - 1.0) * level * 3f32.sqrt()
            })
            .collect()
    }

    fn tone(seconds: f32, amplitude: f32) -> Vec<f32> {
        (0..(RATE as f32 * seconds) as usize)
            .map(|i| amplitude * (2.0 * PI * 220.0 * i as f32 / RATE as f32).sin())
            .collect()
    }

    fn seconds(samples: &[f32]) -> f32 {
        speech_duration(samples, RATE).as_secs_f32()
    }

    #[test]
    fn silence_and_quiet_rooms_have_no_speech() {
        assert_eq!(seconds(&vec![0.0; 32_000]), 0.0);
        assert!(seconds(&noise(2.0, 0.002)) < 0.05);
        assert!(seconds(&noise(2.0, 0.01)) < 0.05, "steady fan noise");
        assert_eq!(seconds(&[]), 0.0);
    }

    #[test]
    fn speech_after_a_pause_is_counted() {
        let mut audio = noise(1.0, 0.002);
        audio.extend(tone(1.0, 0.1));
        assert!((seconds(&audio) - 1.0).abs() < 0.05, "{}", seconds(&audio));
    }

    #[test]
    fn continuous_speech_is_counted() {
        assert!(seconds(&tone(2.0, 0.1)) > 1.9);
    }

    #[test]
    fn quiet_speech_in_a_quiet_room_is_counted() {
        let mut audio = noise(1.0, 0.0015);
        audio.extend(tone(0.5, 0.02));
        assert!(seconds(&audio) > 0.45);
    }

    /// Synthesized speech from `runtimes/test-audio`.
    #[test]
    #[ignore = "needs runtimes/test-audio: cargo test -- --ignored synthesized_speech"]
    fn synthesized_speech_is_counted() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtimes/test-audio/en-short.wav");
        let bytes = std::fs::read(path).unwrap();
        let samples: Vec<f32> = bytes[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32_768.0)
            .collect();
        let speech = seconds(&samples);
        println!("speech {speech:.2} s of {:.2} s", samples.len() as f32 / RATE as f32);
        assert!(speech > 1.5);
    }
}
