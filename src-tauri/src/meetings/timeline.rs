//! Keeping a track in step with the wall clock.
//!
//! The two tracks of a meeting come from different devices and must stay aligned: a device
//! switch leaves a gap, and Windows loopback delivers nothing while nothing plays. Whenever the
//! samples written fall behind the time since the start by more than [`TOLERANCE`], silence
//! fills the difference. A device that runs a little fast is not trimmed.

use std::time::Duration;

use crate::audio::resample::TARGET_RATE;

/// Normal delivery lags by a few tens of milliseconds; only a real gap is filled.
pub const TOLERANCE: Duration = Duration::from_millis(200);

#[derive(Debug, Default)]
pub struct Timeline {
    written: u64,
}

impl Timeline {
    /// Samples of silence to write before `incoming` new samples, `elapsed` after the start of
    /// the recording. Counts both as written.
    pub fn gap(&mut self, elapsed: Duration, incoming: usize) -> usize {
        let expected = (elapsed.as_secs_f64() * f64::from(TARGET_RATE)) as u64;
        let after = self.written + incoming as u64;
        let tolerance = (TOLERANCE.as_secs_f64() * f64::from(TARGET_RATE)) as u64;
        let gap = if expected > after + tolerance { expected - after } else { 0 };
        self.written = after + gap;
        gap as usize
    }

    #[cfg(test)]
    pub fn written(&self) -> u64 {
        self.written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: usize = TARGET_RATE as usize;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn steady_audio_gets_no_silence() {
        let mut timeline = Timeline::default();
        // 100 ms blocks that arrive 30 ms late.
        for block in 1..=50u64 {
            assert_eq!(timeline.gap(ms(block * 100 + 30), RATE / 10), 0, "block {block}");
        }
        assert_eq!(timeline.written(), 5 * RATE as u64);
    }

    #[test]
    fn a_device_switch_is_filled_with_silence() {
        let mut timeline = Timeline::default();
        assert_eq!(timeline.gap(ms(1_000), RATE), 0);
        // The new device starts 700 ms later: its first 100 ms block ends at 1.8 s.
        let gap = timeline.gap(ms(1_800), RATE / 10);
        assert_eq!(gap, RATE * 7 / 10);
        assert_eq!(timeline.written(), (RATE * 18 / 10) as u64);
    }

    #[test]
    fn silence_from_loopback_is_filled_while_waiting() {
        let mut timeline = Timeline::default();
        // Nothing arrives; a check every 100 ms fills once the lag passes the tolerance, so the
        // track never lags by more than that.
        let mut filled = 0;
        for tick in 1..=10u64 {
            filled += timeline.gap(ms(tick * 100), 0);
            let lag = (tick as usize * RATE / 10).saturating_sub(filled);
            assert!(lag <= RATE / 5, "tick {tick}: {lag} samples behind");
        }
        assert!(filled >= RATE * 8 / 10, "{filled}");
        // Then sound plays again and nothing more is added.
        assert_eq!(timeline.gap(ms(1_100), RATE / 10), 0);
    }

    #[test]
    fn a_fast_device_is_not_trimmed() {
        let mut timeline = Timeline::default();
        assert_eq!(timeline.gap(ms(1_000), RATE + RATE / 2), 0);
        assert_eq!(timeline.written(), (RATE + RATE / 2) as u64);
    }
}
