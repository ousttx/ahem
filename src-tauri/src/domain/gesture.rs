//! Hold, toggle or auto: turns hotkey presses and releases into starting and stopping a
//! recording.
//!
//! A press while the key is still held (key repeat) is ignored in every mode. A mode chosen
//! during a recording applies from the next one.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Auto mode: a press held at least this long is a hold, a shorter one a tap.
pub const HOLD_THRESHOLD: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingMode {
    /// Record while the hotkey is held.
    Hold,
    /// The first press starts, the next press stops.
    Toggle,
    /// A tap records until the next press, a hold until the release (the default).
    #[default]
    Auto,
}

impl RecordingMode {
    /// Settings saved before this choice existed recorded with start and stop.
    pub fn saved_before_the_choice() -> Self {
        Self::Toggle
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    Start,
    Stop,
    /// Auto mode, a tap: the recording goes on with the hotkey released.
    Latch,
}

#[derive(Debug)]
pub struct HotkeyGesture {
    /// The mode of the next recording.
    next: RecordingMode,
    /// The mode of the current or the last recording.
    mode: RecordingMode,
    held: bool,
    recording: bool,
    /// When the press that started the recording went down, while it is held.
    started_at: Option<Instant>,
}

impl HotkeyGesture {
    pub fn new(mode: RecordingMode) -> Self {
        Self { next: mode, mode, held: false, recording: false, started_at: None }
    }

    pub fn set_mode(&mut self, mode: RecordingMode) {
        self.next = mode;
        if !self.recording {
            self.mode = mode;
        }
    }

    /// The mode of the current recording.
    pub fn mode(&self) -> RecordingMode {
        self.mode
    }

    /// The recording goes on with the hotkey up: toggle mode, or a tap in auto mode.
    pub fn hands_free(&self) -> bool {
        match self.mode {
            RecordingMode::Hold => false,
            RecordingMode::Toggle => true,
            RecordingMode::Auto => self.recording && !self.held,
        }
    }

    pub fn pressed(&mut self, now: Instant) -> Option<Gesture> {
        if self.held {
            return None;
        }
        self.held = true;
        if self.recording {
            (self.mode != RecordingMode::Hold).then(|| self.stop())
        } else {
            self.mode = self.next;
            self.recording = true;
            self.started_at = Some(now);
            Some(Gesture::Start)
        }
    }

    pub fn released(&mut self, now: Instant) -> Option<Gesture> {
        self.held = false;
        let started_at = self.started_at.take();
        if !self.recording {
            return None;
        }
        match self.mode {
            RecordingMode::Hold => Some(self.stop()),
            RecordingMode::Toggle => None,
            RecordingMode::Auto => started_at.map(|at| {
                if now.saturating_duration_since(at) >= HOLD_THRESHOLD {
                    self.stop()
                } else {
                    Gesture::Latch
                }
            }),
        }
    }

    /// The recording ended without the hotkey, e.g. the microphone failed.
    pub fn reset(&mut self) {
        self.recording = false;
        self.started_at = None;
        self.mode = self.next;
    }

    /// The recording ended and its hotkey went away (the settings no longer name it): its
    /// release will never come, so the next press of another hotkey starts anew.
    pub fn forget(&mut self) {
        self.reset();
        self.held = false;
    }

    fn stop(&mut self) -> Gesture {
        self.reset();
        Gesture::Stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Gesture::{Latch, Start, Stop};

    fn after(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn hold_records_while_pressed() {
        let t = Instant::now();
        let mut gesture = HotkeyGesture::new(RecordingMode::Hold);
        assert_eq!(gesture.pressed(t), Some(Start));
        assert_eq!(gesture.pressed(after(t, 50)), None, "key repeat");
        assert_eq!(gesture.released(after(t, 100)), Some(Stop));
        assert_eq!(gesture.released(after(t, 150)), None);
    }

    #[test]
    fn toggle_starts_and_stops_on_presses() {
        let t = Instant::now();
        let mut gesture = HotkeyGesture::new(RecordingMode::Toggle);
        assert_eq!(gesture.pressed(t), Some(Start));
        assert_eq!(gesture.pressed(after(t, 50)), None, "key repeat");
        assert_eq!(gesture.released(after(t, 900)), None);
        assert_eq!(gesture.pressed(after(t, 2000)), Some(Stop));
        assert_eq!(gesture.released(after(t, 2100)), None);
        assert_eq!(gesture.pressed(after(t, 3000)), Some(Start));
    }

    #[test]
    fn auto_tap_records_until_the_next_press() {
        let t = Instant::now();
        let mut gesture = HotkeyGesture::new(RecordingMode::Auto);
        assert_eq!(gesture.pressed(t), Some(Start));
        assert_eq!(gesture.released(after(t, 299)), Some(Latch));
        assert_eq!(gesture.pressed(after(t, 5000)), Some(Stop));
        // Held long or not, the release of the stopping press does nothing.
        assert_eq!(gesture.released(after(t, 6000)), None);
        assert_eq!(gesture.pressed(after(t, 7000)), Some(Start));
    }

    #[test]
    fn auto_hold_records_until_the_release() {
        let t = Instant::now();
        let mut gesture = HotkeyGesture::new(RecordingMode::Auto);
        assert_eq!(gesture.pressed(t), Some(Start));
        assert_eq!(gesture.released(after(t, 300)), Some(Stop));
        assert_eq!(gesture.pressed(after(t, 1000)), Some(Start));
        assert_eq!(gesture.released(after(t, 4000)), Some(Stop));
    }

    #[test]
    fn hands_are_free_in_toggle_mode_and_after_a_tap() {
        let t = Instant::now();
        let mut gesture = HotkeyGesture::new(RecordingMode::Auto);
        gesture.pressed(t);
        assert!(!gesture.hands_free(), "still held: maybe a hold");
        gesture.released(after(t, 100));
        assert!(gesture.hands_free());
        assert!(HotkeyGesture::new(RecordingMode::Toggle).hands_free());
        let mut hold = HotkeyGesture::new(RecordingMode::Hold);
        hold.pressed(t);
        assert!(!hold.hands_free());
    }

    #[test]
    fn auto_key_repeat_while_held_does_not_stop() {
        let t = Instant::now();
        let mut gesture = HotkeyGesture::new(RecordingMode::Auto);
        assert_eq!(gesture.pressed(t), Some(Start));
        for millis in [30, 300, 1000, 2000] {
            assert_eq!(gesture.pressed(after(t, millis)), None, "key repeat");
        }
        assert_eq!(gesture.released(after(t, 2500)), Some(Stop));
    }

    /// A held hotkey that went away sends no release: forgotten, the next press starts.
    #[test]
    fn a_hotkey_that_went_away_is_forgotten() {
        let now = Instant::now();
        let mut gesture = HotkeyGesture::new(RecordingMode::Hold);
        assert_eq!(gesture.pressed(now), Some(Gesture::Start));
        gesture.forget();
        assert_eq!(gesture.pressed(now), Some(Gesture::Start));
    }

    #[test]
    fn reset_allows_a_new_start() {
        let t = Instant::now();
        let mut gesture = HotkeyGesture::new(RecordingMode::Toggle);
        gesture.pressed(t);
        gesture.released(after(t, 100));
        gesture.reset();
        assert_eq!(gesture.pressed(after(t, 200)), Some(Start));
    }

    #[test]
    fn auto_reset_while_held_ignores_the_release() {
        let t = Instant::now();
        let mut gesture = HotkeyGesture::new(RecordingMode::Auto);
        gesture.pressed(t);
        gesture.reset();
        assert_eq!(gesture.released(after(t, 100)), None);
        assert_eq!(gesture.pressed(after(t, 200)), Some(Start));
    }

    #[test]
    fn mode_change_applies_to_the_next_recording() {
        let t = Instant::now();
        let mut gesture = HotkeyGesture::new(RecordingMode::Toggle);
        gesture.set_mode(RecordingMode::Hold);
        assert_eq!(gesture.pressed(t), Some(Start));
        assert_eq!(gesture.released(after(t, 100)), Some(Stop));

        // Changed while recording: this recording keeps its mode.
        let mut gesture = HotkeyGesture::new(RecordingMode::Toggle);
        assert_eq!(gesture.pressed(t), Some(Start));
        gesture.set_mode(RecordingMode::Hold);
        assert_eq!(gesture.mode(), RecordingMode::Toggle);
        assert_eq!(gesture.released(after(t, 100)), None);
        assert_eq!(gesture.pressed(after(t, 1000)), Some(Stop));
        assert_eq!(gesture.mode(), RecordingMode::Hold);
        assert_eq!(gesture.released(after(t, 1100)), None);
        assert_eq!(gesture.pressed(after(t, 2000)), Some(Start));
        assert_eq!(gesture.released(after(t, 2100)), Some(Stop));
    }
}
