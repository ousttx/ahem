//! Sample-rate conversion to Whisper's 16 kHz: a windowed-sinc low-pass filter at the input
//! rate (so higher frequencies do not fold into speech), evaluated only at the points that
//! the linear interpolation needs.

use std::f64::consts::PI;

pub const TARGET_RATE: u32 = 16_000;
const TAPS: usize = 63;
/// Below the 8 kHz Nyquist limit of the output, leaving room for the filter's transition.
const CUTOFF_HZ: f64 = 7_600.0;

pub fn to_16k(input: &[f32], rate: u32) -> Vec<f32> {
    if rate == TARGET_RATE || input.is_empty() || rate == 0 {
        return input.to_vec();
    }
    let filter = (rate > TARGET_RATE).then(|| lowpass(rate));
    let sample = |index: usize| match &filter {
        Some(taps) => convolve(input, taps, index),
        None => input[index],
    };
    let ratio = f64::from(rate) / f64::from(TARGET_RATE);
    let count = (input.len() as f64 / ratio).floor() as usize;
    (0..count)
        .map(|i| {
            let position = i as f64 * ratio;
            let left = position.floor() as usize;
            let right = (left + 1).min(input.len() - 1);
            let fraction = (position - left as f64) as f32;
            let a = sample(left);
            // On an input sample (every output at 48 kHz) the right one adds nothing.
            if right == left || fraction == 0.0 {
                return a;
            }
            a + (sample(right) - a) * fraction
        })
        .collect()
}

/// [`to_16k`] over audio that arrives in blocks, for a recording written while it goes on. The
/// output equals `to_16k` of the whole input, block boundaries do not show.
pub struct Resampler {
    rate: u32,
    filter: Option<Vec<f32>>,
    ratio: f64,
    /// Input not yet behind every output that needs it; `pending[0]` is input sample `base`.
    pending: Vec<f32>,
    base: usize,
    /// Output samples given out so far.
    produced: usize,
}

impl Resampler {
    pub fn new(rate: u32) -> Self {
        let rate = rate.max(1);
        Self {
            rate,
            filter: (rate > TARGET_RATE).then(|| lowpass(rate)),
            ratio: f64::from(rate) / f64::from(TARGET_RATE),
            pending: Vec::new(),
            base: 0,
            produced: 0,
        }
    }

    /// The 16 kHz samples that this block completes.
    pub fn push(&mut self, block: &[f32]) -> Vec<f32> {
        if self.rate == TARGET_RATE {
            return block.to_vec();
        }
        self.pending.extend_from_slice(block);
        let half = TAPS / 2;
        // An output needs input up to one past its left neighbour plus half the filter.
        let available = self.base + self.pending.len();
        let mut out = Vec::new();
        loop {
            let position = self.produced as f64 * self.ratio;
            if position.floor() as usize + 1 + half >= available {
                break;
            }
            out.push(self.sample_at(position, available));
            self.produced += 1;
        }
        self.trim();
        out
    }

    /// The rest of the output once the input has ended: the total is `to_16k`'s length.
    pub fn finish(&mut self) -> Vec<f32> {
        if self.rate == TARGET_RATE {
            return Vec::new();
        }
        let available = self.base + self.pending.len();
        let count = (available as f64 / self.ratio).floor() as usize;
        let mut out = Vec::new();
        while self.produced < count {
            let position = self.produced as f64 * self.ratio;
            out.push(self.sample_at(position, available));
            self.produced += 1;
        }
        self.pending.clear();
        self.base = available;
        out
    }

    fn sample_at(&self, position: f64, available: usize) -> f32 {
        let left = position.floor() as usize;
        let right = (left + 1).min(available - 1);
        let fraction = (position - left as f64) as f32;
        let a = self.input_at(left);
        if right == left || fraction == 0.0 {
            return a;
        }
        a + (self.input_at(right) - a) * fraction
    }

    /// Filtered input at an absolute index; samples past the end count as silence. Input
    /// before `base` is no longer reached (`trim`).
    fn input_at(&self, index: usize) -> f32 {
        let local = index - self.base;
        match &self.filter {
            Some(taps) => convolve(&self.pending, taps, local),
            None => self.pending[local],
        }
    }

    /// Drops input that no later output reaches.
    fn trim(&mut self) {
        let next = (self.produced as f64 * self.ratio).floor() as usize;
        let keep_from = next.saturating_sub(TAPS / 2 + 1);
        if keep_from > self.base {
            self.pending.drain(..keep_from - self.base);
            self.base = keep_from;
        }
    }
}

/// Blackman-windowed sinc, normalized to unit gain at 0 Hz.
fn lowpass(rate: u32) -> Vec<f32> {
    let cutoff = CUTOFF_HZ / f64::from(rate);
    let last = (TAPS - 1) as f64;
    let taps: Vec<f64> = (0..TAPS)
        .map(|n| {
            let x = n as f64 - last / 2.0;
            let sinc =
                if x == 0.0 { 2.0 * cutoff } else { (2.0 * PI * cutoff * x).sin() / (PI * x) };
            let phase = 2.0 * PI * n as f64 / last;
            let window = 0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos();
            sinc * window
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    taps.iter().map(|tap| (tap / sum) as f32).collect()
}

/// The filter centred on `input[center]`; samples past either end count as silence.
fn convolve(input: &[f32], taps: &[f32], center: usize) -> f32 {
    let half = taps.len() / 2;
    // Away from the ends every tap has its sample: the same sum in the same order, unchecked.
    let start = center.checked_sub(half);
    if let Some(window) = start.and_then(|start| input.get(start..start + taps.len())) {
        return window.iter().zip(taps).map(|(value, tap)| value * tap).sum();
    }
    taps.iter()
        .enumerate()
        .filter_map(|(k, tap)| {
            let index = (center + k).checked_sub(half)?;
            input.get(index).map(|value| value * tap)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frequency: f64, rate: u32, seconds: f64) -> Vec<f32> {
        let count = (f64::from(rate) * seconds) as usize;
        (0..count)
            .map(|i| (0.5 * (2.0 * PI * frequency * i as f64 / f64::from(rate)).sin()) as f32)
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        // Skip the filter's edges.
        let inner = &samples[100..samples.len() - 100];
        (inner.iter().map(|s| s * s).sum::<f32>() / inner.len() as f32).sqrt()
    }

    fn crossings(samples: &[f32]) -> usize {
        samples.windows(2).filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0)).count()
    }

    /// Inside the input the filter skips the checks; it must give the very same sums.
    #[test]
    fn the_filter_sums_alike_inside_and_at_the_ends() {
        let checked = |input: &[f32], taps: &[f32], center: usize| -> f32 {
            let half = taps.len() / 2;
            taps.iter()
                .enumerate()
                .filter_map(|(k, tap)| input.get((center + k).checked_sub(half)?).map(|v| v * tap))
                .sum()
        };
        let input = tone(440.0, 48_000, 0.01);
        let taps = lowpass(48_000);
        for center in 0..input.len() {
            let fast = convolve(&input, &taps, center);
            assert_eq!(fast.to_bits(), checked(&input, &taps, center).to_bits(), "at {center}");
        }
    }

    #[test]
    fn keeps_16k_audio_unchanged() {
        let input = tone(440.0, 16_000, 0.1);
        assert_eq!(to_16k(&input, 16_000), input);
    }

    #[test]
    fn output_length_follows_the_rate() {
        assert_eq!(to_16k(&tone(440.0, 48_000, 1.0), 48_000).len(), 16_000);
        assert_eq!(to_16k(&tone(440.0, 44_100, 1.0), 44_100).len(), 16_000);
        assert_eq!(to_16k(&tone(440.0, 8_000, 1.0), 8_000).len(), 16_000);
    }

    #[test]
    fn speech_band_tone_survives() {
        for rate in [48_000, 44_100] {
            let output = to_16k(&tone(1_000.0, rate, 1.0), rate);
            let found = crossings(&output);
            assert!((1_995..=2_005).contains(&found), "{rate}: {found} zero crossings");
            let level = rms(&output);
            assert!((level - 0.3536).abs() < 0.02, "{rate}: rms {level}");
        }
    }

    #[test]
    fn blocks_give_the_same_output_as_the_whole() {
        // The rates of recordings and of files, an odd one, and a block size that varies.
        for rate in [8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 44_056, 48_000, 96_000] {
            let input = tone(700.0, rate, 0.37);
            let whole = to_16k(&input, rate);
            let mut resampler = Resampler::new(rate);
            let mut streamed = Vec::new();
            // Uneven blocks, as audio callbacks deliver them.
            for block in input.chunks(1_013) {
                streamed.extend(resampler.push(block));
            }
            streamed.extend(resampler.finish());
            assert_eq!(streamed.len(), whole.len(), "{rate}");
            let worst = streamed.iter().zip(&whole).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(worst < 1e-6, "{rate}: max difference {worst}");
        }
    }

    #[test]
    fn the_resampler_keeps_little_input() {
        let mut resampler = Resampler::new(48_000);
        for block in tone(440.0, 48_000, 2.0).chunks(480) {
            resampler.push(block);
        }
        assert!(resampler.pending.len() < 480 + TAPS * 4, "{}", resampler.pending.len());
    }

    #[test]
    fn frequencies_above_8k_do_not_fold_back() {
        // Without the filter a 12 kHz tone would appear at 4 kHz with full strength.
        let output = to_16k(&tone(12_000.0, 48_000, 1.0), 48_000);
        assert!(rms(&output) < 0.01, "rms {}", rms(&output));
    }
}
