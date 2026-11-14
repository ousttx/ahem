//! The dictation indicator: a small always-on-top window that never takes focus and lets the
//! mouse through. It is created visible once, off screen, and is only moved afterwards:
//! `tao` shows a window without activating it only at creation, and a later `show()` would
//! steal the focus that insertion into the hotkey's window depends on. Its page draws the state
//! from the same `core-event`s as the main window.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tauri::{AppHandle, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::domain::events::{CoreEvent, DictationEvent};
use crate::domain::session::{SessionEvent, SessionState};

pub const LABEL: &str = "indicator";
/// The page's size, in CSS pixels: room for the widest capsule (340 px) and the tallest (three
/// lines of a problem, 66.7 px), each with its 1 px edge (`indicator.css`).
const WIDTH: f64 = 380.0;
const HEIGHT: f64 = 70.0;
/// Where the capsule's middle sits above the work area's end, past `INDICATOR_MARGIN`, in
/// logical pixels.
const MIDDLE: f64 = 32.0;
/// Far outside any monitor while hidden (not -32000, which Windows uses for minimized windows).
const OFF_SCREEN: PhysicalPosition<i32> = PhysicalPosition { x: -20_000, y: -20_000 };

const SUCCESS_LINGER: Duration = Duration::from_millis(1200);
const NOTICE_LINGER: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Show,
    /// Show, then hide after the delay unless something new starts.
    ShowFor(Duration),
    HideAfter(Duration),
}

/// What is in progress: dictations and a selected-text rewrite.
#[derive(Debug, Default)]
pub struct IndicatorState {
    /// Turned off in the settings: what is in progress is still followed, nothing is shown.
    off: bool,
    sessions: HashSet<u64>,
    /// Dictations with a notice to read beside their text: processing failed (inserted as
    /// recognized), or the microphone stopped midway.
    unprocessed: HashSet<u64>,
    rewriting: bool,
}

impl IndicatorState {
    pub fn on_event(&mut self, event: &CoreEvent) -> Option<Placement> {
        let placement = self.follow(event);
        if self.off { None } else { placement }
    }

    fn follow(&mut self, event: &CoreEvent) -> Option<Placement> {
        match event {
            CoreEvent::Session(SessionEvent::StateChanged { session, state }) => {
                if state.is_finished() {
                    self.sessions.remove(&session.0);
                    let notice = self.unprocessed.remove(&session.0);
                    let done = matches!(state, SessionState::Inserted | SessionState::Kept);
                    let linger = if done && !notice { SUCCESS_LINGER } else { NOTICE_LINGER };
                    self.idle().then_some(Placement::HideAfter(linger))
                } else {
                    self.sessions.insert(session.0).then_some(Placement::Show)
                }
            }
            CoreEvent::Dictation(DictationEvent::SelectionStarted { .. }) => {
                self.rewriting = true;
                Some(Placement::Show)
            }
            CoreEvent::Dictation(DictationEvent::SelectionDone) => {
                self.rewriting = false;
                self.idle().then_some(Placement::HideAfter(SUCCESS_LINGER))
            }
            // Inserted all the same, with a notice to read: shown longer.
            CoreEvent::Session(SessionEvent::ProcessingFailed { session, .. })
            | CoreEvent::Dictation(DictationEvent::MicrophoneLost { session, .. }) => {
                self.unprocessed.insert(session.0);
                None
            }
            CoreEvent::Dictation(DictationEvent::SelectionFailed { .. }) => {
                let started = std::mem::take(&mut self.rewriting);
                // A rewrite that could not start (no model, no focus) was not shown yet.
                self.idle().then_some(if started {
                    Placement::HideAfter(NOTICE_LINGER)
                } else {
                    Placement::ShowFor(NOTICE_LINGER)
                })
            }
            // A hotkey that could not start (no model, no engine, no microphone), or a dictation
            // that went in but not into the history, a refused "Recognize again": shown for the
            // notice.
            CoreEvent::Dictation(
                DictationEvent::Blocked { .. } | DictationEvent::HistoryFailed { .. },
            ) if self.idle() => Some(Placement::ShowFor(NOTICE_LINGER)),
            _ => None,
        }
    }

    fn idle(&self) -> bool {
        self.sessions.is_empty() && !self.rewriting
    }
}

pub struct Indicator {
    window: WebviewWindow,
    state: Mutex<IndicatorState>,
    /// Which pending shows and hides still hold.
    placements: Arc<Placements>,
}

/// Which placements of the capsule still hold. Every placement, and turning it off, voids a
/// pending hide, so it does not hide a newer dictation. Only a newer show, and turning it off,
/// voids a pending show: a hide that follows a show at once (a problem right after the start)
/// must not keep it from showing.
#[derive(Debug, Default)]
struct Placements {
    any: AtomicU64,
    shows: AtomicU64,
}

impl Placements {
    /// A new placement: its number among all, and the number of the show it holds to.
    fn placed(&self, shows: bool) -> (u64, u64) {
        let any = self.any.fetch_add(1, Ordering::SeqCst) + 1;
        let show = if shows {
            self.shows.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            self.shows.load(Ordering::SeqCst)
        };
        (any, show)
    }

    fn turned_off(&self) {
        self.any.fetch_add(1, Ordering::SeqCst);
        self.shows.fetch_add(1, Ordering::SeqCst);
    }

    fn hide_holds(&self, any: u64) -> bool {
        self.any.load(Ordering::SeqCst) == any
    }

    fn show_holds(&self, show: u64) -> bool {
        self.shows.load(Ordering::SeqCst) == show
    }
}

impl Indicator {
    pub fn create(app: &AppHandle) -> tauri::Result<Self> {
        let builder =
            WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("indicator.html".into()))
                .title("ahem")
                .inner_size(WIDTH * crate::platform::UI_ZOOM, HEIGHT * crate::platform::UI_ZOOM)
                .position(f64::from(OFF_SCREEN.x), f64::from(OFF_SCREEN.y))
                .decorations(false)
                .shadow(false)
                .resizable(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .focused(false)
                .focusable(false)
                .visible(true)
                // On macOS this needs `macOSPrivateApi` (tauri.conf.json).
                .transparent(true)
                .additional_browser_args(crate::platform::BROWSER_ARGS);
        let window = crate::platform::build_floating(|| builder.build())?;
        window.set_ignore_cursor_events(true)?;
        Ok(Self {
            window,
            state: Mutex::new(IndicatorState::default()),
            placements: Arc::default(),
        })
    }

    /// The setting `show_indicator`: turned off, the capsule goes at once and stays away.
    pub fn set_shown(&self, shown: bool) {
        self.state.lock().expect("indicator lock").off = !shown;
        if !shown {
            // A pending hide or show of a placement before is void.
            self.placements.turned_off();
            let _ = self.window.set_position(OFF_SCREEN);
        }
    }

    pub fn on_event(&self, event: &CoreEvent) {
        let Some(placement) = self.state.lock().expect("indicator lock").on_event(event) else {
            return;
        };
        let (any, show) =
            self.placements.placed(matches!(placement, Placement::Show | Placement::ShowFor(_)));
        let hide_after = match placement {
            Placement::Show => None,
            Placement::ShowFor(after) => {
                self.show(show);
                Some(after)
            }
            Placement::HideAfter(after) => Some(after),
        };
        if placement == Placement::Show {
            self.show(show);
        }
        // Turned off meanwhile: the show above is void, and it stays away.
        if self.state.lock().expect("indicator lock").off {
            self.placements.turned_off();
            let _ = self.window.set_position(OFF_SCREEN);
            return;
        }
        if let Some(after) = hide_after {
            let window = self.window.clone();
            let placements = Arc::clone(&self.placements);
            thread::spawn(move || {
                thread::sleep(after);
                if placements.hide_holds(any) {
                    let _ = window.set_position(OFF_SCREEN);
                }
            });
        }
    }

    /// Bottom center of the work area of the monitor under the mouse, on the main thread and not
    /// waited for: asked from the controller's thread, where to place it is a question to the
    /// event loop, which no longer answers while the app quits and waits for that thread. A
    /// newer show (or the capsule turned off) meanwhile makes this one void.
    fn show(&self, show: u64) {
        let window = self.window.clone();
        let placements = Arc::clone(&self.placements);
        let _ = self.window.run_on_main_thread(move || {
            if placements.show_holds(show) {
                place(&window);
            }
        });
    }
}

/// Puts the capsule at the bottom center of the work area of the monitor under the mouse
/// (`platform::PLACES_IN_POINTS`: on macOS all of it in points, so a second screen of another
/// scale gets it where it belongs).
fn place(window: &WebviewWindow) {
    let primary = window.primary_monitor().ok().flatten();
    let main_scale = primary.as_ref().map_or(1.0, tauri::Monitor::scale_factor);
    let monitor = window
        .cursor_position()
        .ok()
        .and_then(|cursor| {
            let (x, y) = mouse_point(cursor.x, cursor.y, main_scale);
            window.monitor_from_point(x, y).ok().flatten()
        })
        .or(primary);
    let Some(monitor) = monitor else { return };
    let area = monitor.work_area();
    let scale = monitor.scale_factor();
    let zoom = crate::platform::UI_ZOOM;
    let origin = (f64::from(area.position.x), f64::from(area.position.y));
    let size = (f64::from(area.size.width), f64::from(area.size.height));
    let _ = if crate::platform::PLACES_IN_POINTS {
        let (x, y) = position(
            (origin.0 / scale, origin.1 / scale),
            (size.0 / scale, size.1 / scale),
            1.0,
            zoom,
        );
        window.set_position(tauri::LogicalPosition { x, y })
    } else {
        let (x, y) = position(origin, size, scale, zoom);
        window.set_position(PhysicalPosition { x: x.round() as i32, y: y.round() as i32 })
    };
}

/// Where the mouse is, as monitors are found: on macOS in points, while it comes in points
/// times the main screen's scale.
fn mouse_point(x: f64, y: f64, main_scale: f64) -> (f64, f64) {
    if crate::platform::PLACES_IN_POINTS { (x / main_scale, y / main_scale) } else { (x, y) }
}

/// The window's top left in a work area (`origin` and `size`, `scale` units to a point):
/// centred, its middle (the capsule's) `MIDDLE` above the margin. The size is the one on that
/// monitor: `outer_size` is in the scale of the one the window is on now, and the system
/// resizes it for the new one after the move.
fn position(origin: (f64, f64), size: (f64, f64), scale: f64, zoom: f64) -> (f64, f64) {
    let window = (WIDTH * zoom * scale, HEIGHT * zoom * scale);
    let bottom = origin.1 + size.1;
    let middle = bottom - (crate::platform::INDICATOR_MARGIN + MIDDLE) * scale;
    (origin.0 + (size.0 - window.0) / 2.0, middle - window.1 / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::events::{BlockReason, SelectionProblem};
    use crate::domain::session::{DiscardReason, Failure, FailureKind, SessionId};

    /// The window holds the tallest and the widest capsule at every zoom, and the capsule's
    /// middle stays where it was when the window was 64 px tall and not zoomed.
    #[test]
    fn the_window_holds_the_largest_capsule_where_it_was() {
        // `indicator.css`: 8 px above and below three lines of 13 px at 1.3, its 1 px edge.
        let tallest = 8.0 + 3.0 * 13.0 * 1.3 + 8.0 + 2.0;
        let widest = 340.0 + 2.0;
        assert!(HEIGHT >= tallest && WIDTH >= widest);
        let area = (1920.0, 1080.0);
        for (scale, zoom) in [(1.0, 1.0), (2.0, 1.0), (1.0, 1.2), (1.5, 1.2)] {
            let at = position((0.0, 0.0), area, scale, zoom);
            let middle = at.1 + HEIGHT * zoom * scale / 2.0;
            let before = 1080.0 - crate::platform::INDICATOR_MARGIN * scale - 64.0 * scale / 2.0;
            assert!((middle - before).abs() <= 1.0, "{scale} {zoom}: {middle} {before}");
            let centre = at.0 + WIDTH * zoom * scale / 2.0;
            assert!((centre - 960.0).abs() <= 1.0);
        }
        // A monitor left of and above the main one.
        let at = position((-1920.0, -200.0), area, 1.0, 1.0);
        assert_eq!(at.0, -1920.0 + (1920.0 - 380.0) / 2.0);
    }

    /// In points (macOS), a screen's place is its pixels over its own scale and the window's
    /// size is in points: the same spot as in pixels, whatever the scale of the screen the
    /// window comes from; the mouse comes over the main screen's scale.
    #[test]
    fn a_place_in_points_is_the_same_spot_on_every_screen() {
        // A @1x screen right of a @2x MacBook whose 1512 points are 3024 pixels.
        let (scale, origin, size) = (1.0, (1512.0, 0.0), (1920.0, 1080.0));
        let points = position(
            (origin.0 / scale, origin.1 / scale),
            (size.0 / scale, size.1 / scale),
            1.0,
            1.0,
        );
        let pixels = position(origin, size, scale, 1.0);
        assert_eq!(points, (pixels.0 / scale, pixels.1 / scale));
        assert!(points.0 > 1512.0, "on the second screen");
        if crate::platform::PLACES_IN_POINTS {
            assert_eq!(mouse_point(3000.0, 1400.0, 2.0), (1500.0, 700.0));
        } else {
            assert_eq!(mouse_point(3000.0, 1400.0, 2.0), (3000.0, 1400.0));
        }
    }

    fn state(session: u64, state: SessionState) -> CoreEvent {
        CoreEvent::Session(SessionEvent::StateChanged { session: SessionId(session), state })
    }

    /// A hide right after a show does not keep it from showing, and hides it after its wait; a
    /// newer show, or turning the capsule off, voids a pending show; any placement voids a
    /// pending hide.
    #[test]
    fn a_show_holds_until_a_newer_show() {
        let placements = Placements::default();
        let (_, shown) = placements.placed(true);
        let (hidden, _) = placements.placed(false);
        assert!(placements.show_holds(shown), "a problem at once still shows");
        assert!(placements.hide_holds(hidden));
        let (_, newer) = placements.placed(true);
        assert!(!placements.show_holds(shown));
        assert!(!placements.hide_holds(hidden), "a newer dictation is not hidden");
        placements.turned_off();
        assert!(!placements.show_holds(newer), "turned off: nothing shows");
    }

    #[test]
    fn shows_while_a_dictation_runs() {
        let mut indicator = IndicatorState::default();
        assert_eq!(indicator.on_event(&state(1, SessionState::Recording)), Some(Placement::Show));
        assert_eq!(indicator.on_event(&state(1, SessionState::Transcribing)), None);
        assert_eq!(indicator.on_event(&state(1, SessionState::Inserting)), None);
        assert_eq!(
            indicator.on_event(&state(1, SessionState::Inserted)),
            Some(Placement::HideAfter(SUCCESS_LINGER))
        );
    }

    #[test]
    fn problems_stay_longer_than_success() {
        let mut indicator = IndicatorState::default();
        indicator.on_event(&state(1, SessionState::Recording));
        let failed = SessionState::Failed {
            failure: Failure { kind: FailureKind::InsertionBlocked, detail: String::new() },
        };
        assert_eq!(
            indicator.on_event(&state(1, failed)),
            Some(Placement::HideAfter(NOTICE_LINGER))
        );
        indicator.on_event(&state(2, SessionState::Recording));
        let discarded = SessionState::Discarded { reason: DiscardReason::NoSpeech };
        assert_eq!(
            indicator.on_event(&state(2, discarded)),
            Some(Placement::HideAfter(NOTICE_LINGER))
        );
    }

    #[test]
    fn a_failed_processing_stays_to_be_read_after_the_insertion() {
        let mut indicator = IndicatorState::default();
        indicator.on_event(&state(1, SessionState::Recording));
        let failure = Failure { kind: FailureKind::Processing, detail: String::new() };
        let processing =
            CoreEvent::Session(SessionEvent::ProcessingFailed { session: SessionId(1), failure });
        assert_eq!(indicator.on_event(&processing), None);
        assert_eq!(
            indicator.on_event(&state(1, SessionState::Inserted)),
            Some(Placement::HideAfter(NOTICE_LINGER))
        );
        // The next dictation is a success again.
        indicator.on_event(&state(2, SessionState::Recording));
        assert_eq!(
            indicator.on_event(&state(2, SessionState::Inserted)),
            Some(Placement::HideAfter(SUCCESS_LINGER))
        );
    }

    #[test]
    fn turned_off_it_shows_nothing_but_follows_what_runs() {
        let mut indicator = IndicatorState { off: true, ..IndicatorState::default() };
        assert_eq!(indicator.on_event(&state(1, SessionState::Recording)), None);
        let blocked = CoreEvent::Dictation(DictationEvent::Blocked {
            reason: BlockReason::NoModel,
            detail: String::new(),
        });
        assert_eq!(indicator.on_event(&blocked), None);
        // Turned on midway: the dictation that runs is known, so its end hides the capsule.
        indicator.off = false;
        assert_eq!(indicator.on_event(&state(2, SessionState::Recording)), Some(Placement::Show));
        assert_eq!(indicator.on_event(&state(1, SessionState::Inserted)), None);
        assert_eq!(
            indicator.on_event(&state(2, SessionState::Inserted)),
            Some(Placement::HideAfter(SUCCESS_LINGER))
        );
    }

    #[test]
    fn a_microphone_lost_midway_stays_to_be_read() {
        let mut indicator = IndicatorState::default();
        indicator.on_event(&state(4, SessionState::Recording));
        let lost = CoreEvent::Dictation(DictationEvent::MicrophoneLost {
            session: SessionId(4),
            detail: String::new(),
        });
        assert_eq!(indicator.on_event(&lost), None);
        assert_eq!(
            indicator.on_event(&state(4, SessionState::Inserted)),
            Some(Placement::HideAfter(NOTICE_LINGER))
        );
    }

    #[test]
    fn a_kept_text_is_a_success_and_a_lost_history_a_notice() {
        let mut indicator = IndicatorState::default();
        indicator.on_event(&state(3, SessionState::Transcribing));
        assert_eq!(
            indicator.on_event(&state(3, SessionState::Kept)),
            Some(Placement::HideAfter(SUCCESS_LINGER))
        );
        let lost = CoreEvent::Dictation(DictationEvent::HistoryFailed { detail: String::new() });
        assert_eq!(indicator.on_event(&lost), Some(Placement::ShowFor(NOTICE_LINGER)));
    }

    #[test]
    fn stays_while_another_dictation_runs() {
        let mut indicator = IndicatorState::default();
        indicator.on_event(&state(1, SessionState::Recording));
        indicator.on_event(&state(1, SessionState::Transcribing));
        assert_eq!(indicator.on_event(&state(2, SessionState::Recording)), Some(Placement::Show));
        assert_eq!(indicator.on_event(&state(1, SessionState::Inserted)), None);
        assert_eq!(
            indicator.on_event(&state(2, SessionState::Inserted)),
            Some(Placement::HideAfter(SUCCESS_LINGER))
        );
    }

    #[test]
    fn follows_rewrites_and_blocked_hotkeys() {
        let mut indicator = IndicatorState::default();
        let started =
            CoreEvent::Dictation(DictationEvent::SelectionStarted { mode_id: "m".into() });
        assert_eq!(indicator.on_event(&started), Some(Placement::Show));
        let failed = CoreEvent::Dictation(DictationEvent::SelectionFailed {
            problem: SelectionProblem::ReadOnly,
            detail: String::new(),
        });
        assert_eq!(indicator.on_event(&failed), Some(Placement::HideAfter(NOTICE_LINGER)));
        // Refused before it started: shown for the notice.
        assert_eq!(indicator.on_event(&failed), Some(Placement::ShowFor(NOTICE_LINGER)));
        let blocked = CoreEvent::Dictation(DictationEvent::Blocked {
            reason: BlockReason::NoModel,
            detail: String::new(),
        });
        assert_eq!(indicator.on_event(&blocked), Some(Placement::ShowFor(NOTICE_LINGER)));
        let level = CoreEvent::Dictation(DictationEvent::Level { permille: 10 });
        assert_eq!(indicator.on_event(&level), None);
    }
}
