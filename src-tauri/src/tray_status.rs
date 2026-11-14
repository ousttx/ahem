//! The tray icon shows what ahem is doing: "a." at rest, a wave that follows the voice while
//! recording, running dots while the speech is recognized or processed, and "!" for a moment
//! after a problem. It follows the same `core-event`s as the indicator.
//!
//! One thread draws the icon: the dictation only records the new look and never waits for the
//! drawing, and frames cannot overtake each other, so the last look drawn is always the current one.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use tauri::image::Image;

use crate::domain::events::{CoreEvent, DictationEvent};
use crate::domain::session::{SessionEvent, SessionState};
use crate::platform;
use crate::tray_glyph::{self, Glyph, Palette};

/// How often the wave moves while recording, like the level strips of the windows.
const WAVE_FRAME: Duration = Duration::from_millis(100);
/// How long each dot stays lit while busy.
const DOT_FRAME: Duration = Duration::from_millis(300);
/// How long "!" stays after a problem, like the indicator's notice.
const PROBLEM_LINGER: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Idle,
    Recording,
    Busy,
    Problem,
}

/// Dictations in progress (recording or not), a selected-text rewrite and a recent problem.
#[derive(Debug, Default)]
pub struct LookState {
    sessions: HashMap<u64, bool>,
    rewriting: bool,
    problem: bool,
}

impl LookState {
    /// The new look, or `None` when the event does not change it.
    pub fn on_event(&mut self, event: &CoreEvent) -> Option<Look> {
        let before = self.look();
        match event {
            CoreEvent::Session(SessionEvent::StateChanged { session, state }) => {
                if state.is_finished() {
                    self.sessions.remove(&session.0);
                    if matches!(state, SessionState::Failed { .. }) {
                        self.problem = true;
                    }
                } else {
                    self.sessions.insert(session.0, *state == SessionState::Recording);
                    self.problem = false;
                }
            }
            CoreEvent::Dictation(DictationEvent::SelectionStarted { .. }) => {
                self.rewriting = true;
                self.problem = false;
            }
            CoreEvent::Dictation(DictationEvent::SelectionDone) => self.rewriting = false,
            CoreEvent::Dictation(DictationEvent::SelectionFailed { .. }) => {
                self.rewriting = false;
                self.problem = true;
            }
            // A hotkey that could not start: no model, no engine, no microphone.
            CoreEvent::Dictation(DictationEvent::Blocked { .. }) => self.problem = true,
            _ => return None,
        }
        let after = self.look();
        (after != before).then_some(after)
    }

    /// After `PROBLEM_LINGER`: back to rest unless something else is shown.
    pub fn clear_problem(&mut self) -> Option<Look> {
        let before = self.look();
        self.problem = false;
        let after = self.look();
        (after != before).then_some(after)
    }

    pub fn look(&self) -> Look {
        if self.sessions.values().any(|recording| *recording) {
            Look::Recording
        } else if !self.sessions.is_empty() || self.rewriting {
            Look::Busy
        } else if self.problem {
            Look::Problem
        } else {
            Look::Idle
        }
    }
}

/// What the drawing thread shows and when it has to look again.
#[derive(Debug)]
struct Animation {
    state: LookState,
    /// The latest input level, 0–1, and the last five of the frames, the newest last.
    level: f32,
    levels: [f32; 5],
    frame: u64,
    /// The next frame, or the end of "!".
    due: Option<Instant>,
    /// The icon has to be drawn again.
    dirty: bool,
}

impl Animation {
    fn new() -> Self {
        Self {
            state: LookState::default(),
            level: 0.0,
            levels: [0.0; 5],
            frame: 0,
            due: None,
            dirty: false,
        }
    }

    /// Whether the look changed.
    fn on_event(&mut self, event: &CoreEvent, now: Instant) -> bool {
        if let CoreEvent::Dictation(DictationEvent::Level { permille }) = event {
            self.level = f32::from(*permille) / 1000.0;
            return false;
        }
        let Some(look) = self.state.on_event(event) else { return false };
        self.show(look, now);
        true
    }

    fn show(&mut self, look: Look, now: Instant) {
        self.frame = 0;
        self.levels = [0.0; 5];
        self.due = match look {
            Look::Recording => Some(now + WAVE_FRAME),
            Look::Busy => Some(now + DOT_FRAME),
            Look::Problem => Some(now + PROBLEM_LINGER),
            Look::Idle => None,
        };
        self.dirty = true;
    }

    /// Moves on to `now`: the next frame, or rest once "!" has been shown long enough.
    fn tick(&mut self, now: Instant) {
        let Some(due) = self.due else { return };
        if now < due {
            return;
        }
        let look = self.state.look();
        match look {
            Look::Recording | Look::Busy => {
                let step = if look == Look::Recording { WAVE_FRAME } else { DOT_FRAME };
                self.frame += 1;
                self.levels.rotate_left(1);
                self.levels[4] = self.level;
                // A late frame does not make the next ones hurry.
                let next = due + step;
                self.due = Some(if next <= now { now + step } else { next });
                self.dirty = true;
            }
            Look::Problem => match self.state.clear_problem() {
                Some(look) => self.show(look, now),
                None => self.due = None,
            },
            Look::Idle => self.due = None,
        }
    }

    fn glyph(&self) -> Glyph {
        match self.state.look() {
            Look::Idle => Glyph::Idle,
            Look::Recording => Glyph::Recording(self.levels),
            Look::Busy => Glyph::Busy { lit: (self.frame % 5) as usize },
            Look::Problem => Glyph::Problem,
        }
    }
}

struct Shared {
    animation: Mutex<Animation>,
    wake: Condvar,
}

#[derive(Clone)]
pub struct TrayStatus {
    shared: Arc<Shared>,
}

impl TrayStatus {
    /// Starts the drawing thread; it draws once the tray icon exists (`tray::create`).
    pub fn new(app: AppHandle) -> Self {
        let shared =
            Arc::new(Shared { animation: Mutex::new(Animation::new()), wake: Condvar::new() });
        let drawing = Arc::clone(&shared);
        thread::spawn(move || draw_loop(&app, &drawing));
        Self { shared }
    }

    /// Records the event; never waits for the icon.
    pub fn on_event(&self, event: &CoreEvent) {
        let mut animation = self.shared.animation.lock().expect("tray status lock");
        if animation.on_event(event, Instant::now()) {
            self.shared.wake.notify_one();
        }
    }

    /// Draws the current look again, e.g. after the taskbar theme changed.
    pub fn redraw(&self) {
        self.shared.animation.lock().expect("tray status lock").dirty = true;
        self.shared.wake.notify_one();
    }
}

/// Draws whenever the look changes or a frame is due; sleeps otherwise. The lock is never held
/// while drawing, which waits for the main thread.
fn draw_loop(app: &AppHandle, shared: &Shared) {
    let mut animation = shared.animation.lock().expect("tray status lock");
    loop {
        let now = Instant::now();
        animation.tick(now);
        if animation.dirty {
            animation.dirty = false;
            let glyph = animation.glyph();
            drop(animation);
            if let Some(tray) = app.tray_by_id(crate::tray::ID) {
                // Plain `set_icon` drops the template flag on macOS: the glyph would turn
                // black on a dark menu bar.
                let _ = tray.set_icon_with_as_template(
                    Some(icon(app, glyph)),
                    platform::TRAY_ICON.template,
                );
            }
            animation = shared.animation.lock().expect("tray status lock");
        } else {
            animation = match animation.due {
                Some(due) => {
                    let wait = due.saturating_duration_since(now);
                    shared.wake.wait_timeout(animation, wait).expect("tray status lock").0
                }
                None => shared.wake.wait(animation).expect("tray status lock"),
            };
        }
    }
}

/// The glyph as a tray image: as high as the platform's tray icon at the monitor's scale,
/// a template on macOS and colored for the taskbar elsewhere.
pub fn icon(app: &AppHandle, glyph: Glyph) -> Image<'static> {
    let spec = platform::TRAY_ICON;
    let monitor_scale =
        app.primary_monitor().ok().flatten().map_or(1.0, |monitor| monitor.scale_factor());
    let height = (spec.height * monitor_scale.max(spec.min_scale)).round() as u32;
    let width = if spec.square { height } else { (f64::from(height) * 26.0 / 24.0).round() as u32 };
    let palette = if spec.template {
        Palette::TEMPLATE
    } else {
        Palette::colored(platform::taskbar_is_light())
    };
    Image::new_owned(tray_glyph::render(glyph, palette, width, height), width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::events::{BlockReason, SelectionProblem};
    use crate::domain::session::{DiscardReason, Failure, FailureKind, SessionId};

    fn state(session: u64, state: SessionState) -> CoreEvent {
        CoreEvent::Session(SessionEvent::StateChanged { session: SessionId(session), state })
    }

    fn failed() -> SessionState {
        SessionState::Failed {
            failure: Failure { kind: FailureKind::InsertionBlocked, detail: String::new() },
        }
    }

    #[test]
    fn follows_a_dictation_from_the_wave_to_the_letter() {
        let mut tray = LookState::default();
        assert_eq!(tray.on_event(&state(1, SessionState::Recording)), Some(Look::Recording));
        assert_eq!(tray.on_event(&state(1, SessionState::Transcribing)), Some(Look::Busy));
        assert_eq!(tray.on_event(&state(1, SessionState::PostProcessing)), None);
        assert_eq!(tray.on_event(&state(1, SessionState::Inserting)), None);
        assert_eq!(tray.on_event(&state(1, SessionState::Inserted)), Some(Look::Idle));
    }

    #[test]
    fn recording_wins_over_another_dictation_in_progress() {
        let mut tray = LookState::default();
        tray.on_event(&state(1, SessionState::Recording));
        tray.on_event(&state(1, SessionState::Transcribing));
        assert_eq!(tray.on_event(&state(2, SessionState::Recording)), Some(Look::Recording));
        assert_eq!(tray.on_event(&state(2, SessionState::Transcribing)), Some(Look::Busy));
        assert_eq!(tray.on_event(&state(1, SessionState::Inserted)), None);
        assert_eq!(tray.on_event(&state(2, SessionState::Inserted)), Some(Look::Idle));
    }

    #[test]
    fn a_failure_shows_a_problem_until_it_is_cleared_or_a_new_dictation_starts() {
        let mut tray = LookState::default();
        tray.on_event(&state(1, SessionState::Recording));
        assert_eq!(tray.on_event(&state(1, failed())), Some(Look::Problem));
        assert_eq!(tray.clear_problem(), Some(Look::Idle));
        assert_eq!(tray.clear_problem(), None);

        tray.on_event(&state(2, SessionState::Recording));
        tray.on_event(&state(2, failed()));
        assert_eq!(tray.on_event(&state(3, SessionState::Recording)), Some(Look::Recording));
        assert_eq!(tray.on_event(&state(3, SessionState::Inserted)), Some(Look::Idle));
    }

    #[test]
    fn a_discarded_dictation_is_not_a_problem() {
        let mut tray = LookState::default();
        tray.on_event(&state(1, SessionState::Recording));
        let discarded = SessionState::Discarded { reason: DiscardReason::NoSpeech };
        assert_eq!(tray.on_event(&state(1, discarded)), Some(Look::Idle));
    }

    #[test]
    fn follows_rewrites_blocked_hotkeys_and_ignores_levels() {
        let mut tray = LookState::default();
        let started =
            CoreEvent::Dictation(DictationEvent::SelectionStarted { mode_id: "m".into() });
        assert_eq!(tray.on_event(&started), Some(Look::Busy));
        let done = CoreEvent::Dictation(DictationEvent::SelectionDone);
        assert_eq!(tray.on_event(&done), Some(Look::Idle));
        tray.on_event(&started);
        let failed = CoreEvent::Dictation(DictationEvent::SelectionFailed {
            problem: SelectionProblem::ReadOnly,
            detail: String::new(),
        });
        assert_eq!(tray.on_event(&failed), Some(Look::Problem));
        tray.clear_problem();
        let blocked = CoreEvent::Dictation(DictationEvent::Blocked {
            reason: BlockReason::NoModel,
            detail: String::new(),
        });
        assert_eq!(tray.on_event(&blocked), Some(Look::Problem));
        let level = CoreEvent::Dictation(DictationEvent::Level { permille: 10 });
        assert_eq!(tray.on_event(&level), None);
    }

    #[test]
    fn the_wave_moves_on_with_the_latest_level_once_a_frame_is_due() {
        let start = Instant::now();
        let mut animation = Animation::new();
        assert!(animation.on_event(&state(1, SessionState::Recording), start));
        assert!(animation.dirty);
        animation.dirty = false;
        let level = CoreEvent::Dictation(DictationEvent::Level { permille: 500 });
        assert!(!animation.on_event(&level, start));
        animation.tick(start + WAVE_FRAME / 2);
        assert!(!animation.dirty);
        animation.tick(start + WAVE_FRAME);
        assert!(animation.dirty);
        assert_eq!(animation.glyph(), Glyph::Recording([0.0, 0.0, 0.0, 0.0, 0.5]));
    }

    #[test]
    fn a_late_frame_does_not_make_the_next_ones_hurry() {
        let start = Instant::now();
        let mut animation = Animation::new();
        animation.on_event(&state(1, SessionState::Transcribing), start);
        animation.tick(start + Duration::from_secs(1));
        assert_eq!(animation.glyph(), Glyph::Busy { lit: 1 });
        assert_eq!(animation.due, Some(start + Duration::from_secs(1) + DOT_FRAME));
    }

    #[test]
    fn the_exclamation_mark_goes_away_after_its_time() {
        let start = Instant::now();
        let mut animation = Animation::new();
        animation.on_event(&state(1, SessionState::Recording), start);
        animation.on_event(&state(1, failed()), start);
        assert_eq!(animation.glyph(), Glyph::Problem);
        animation.dirty = false;
        animation.tick(start + PROBLEM_LINGER - Duration::from_millis(1));
        assert_eq!(animation.glyph(), Glyph::Problem);
        animation.tick(start + PROBLEM_LINGER);
        assert_eq!(animation.glyph(), Glyph::Idle);
        assert!(animation.dirty);
        assert_eq!(animation.due, None);
    }
}
