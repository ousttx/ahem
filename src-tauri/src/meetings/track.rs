//! One track of a meeting: a device recorded to an Ogg Opus file, following the system's
//! choice of device.
//!
//! The thread owns the cpal stream (not `Send` everywhere). Every 0.1 s it moves the recorded
//! samples to 16 kHz, fills gaps with silence ([`Timeline`]) and writes them; every 0.5 s it
//! checks which device should be recorded and reopens the stream when that changed or failed.

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::Stream;
use cpal::traits::{DeviceTrait, HostTrait};

use super::timeline::Timeline;
use crate::audio::capture::{Sink, open_device};
use crate::audio::ogg_opus::OggOpusWriter;
use crate::audio::resample::Resampler;
use crate::domain::events::TrackProblem;
use crate::platform::{self, SystemAudio};

const DRAIN_EVERY: Duration = Duration::from_millis(100);
const CHECK_EVERY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The microphone of the settings while it is connected, else the default one.
    Microphone { preferred: Option<String> },
    /// The computer's sound.
    System,
}

impl Source {
    /// The device that should be recorded now, by cpal id; `None` when there is none.
    fn wanted(&self) -> Option<String> {
        let host = cpal::default_host();
        let id = |device: cpal::Device| device.id().ok().map(|id| id.to_string());
        match self {
            Self::Microphone { preferred: Some(preferred) } => {
                let connected = host.input_devices().ok().is_some_and(|mut devices| {
                    devices.any(|device| id(device).as_deref() == Some(preferred.as_str()))
                });
                if connected {
                    Some(preferred.clone())
                } else {
                    host.default_input_device().and_then(id)
                }
            }
            Self::Microphone { preferred: None } => host.default_input_device().and_then(id),
            // The tap records every device on macOS; it is reopened anyway when the output
            // changes, since the new device may run at another rate.
            Self::System => host.default_output_device().and_then(id),
        }
    }

    fn open(&self, wanted: &str, sink: &Arc<Sink>) -> Result<Open, String> {
        let (stream, rate, keep) = match self {
            Self::Microphone { .. } => {
                let device = cpal::default_host()
                    .input_devices()
                    .map_err(|error| error.to_string())?
                    .find(|device| device.id().is_ok_and(|id| id.to_string() == wanted))
                    .ok_or_else(|| format!("microphone {wanted} is gone"))?;
                let (stream, rate) = open_device(&device, sink).map_err(|e| e.to_string())?;
                (stream, rate, None)
            }
            Self::System => {
                let audio = platform::open_system_audio()?;
                let (stream, rate) = open_device(&audio.device, sink).map_err(|e| e.to_string())?;
                (stream, rate, Some(audio))
            }
        };
        Ok(Open { _stream: stream, _keep: keep, device: wanted.to_string(), rate })
    }
}

/// A stream being recorded. The stream is dropped before what it records from.
struct Open {
    _stream: Stream,
    _keep: Option<SystemAudio>,
    device: String,
    rate: u32,
}

#[derive(Default)]
struct Shared {
    sink: Arc<Sink>,
    /// Why the track records nothing of what is heard, if so; a missing device's is cleared
    /// once a stream opens.
    problem: Mutex<Option<TrackProblem>>,
}

pub struct Track {
    shared: Arc<Shared>,
    stop: mpsc::Sender<()>,
    thread: JoinHandle<io::Result<()>>,
}

impl Track {
    /// Creates the file and starts recording; `started` is the meeting's common start, so the
    /// tracks share one timeline.
    pub fn start(source: Source, path: &Path, serial: u32, started: Instant) -> io::Result<Self> {
        let writer = OggOpusWriter::new(BufWriter::new(File::create(path)?), serial)?;
        let shared = Arc::new(Shared::default());
        let (stop, stopped) = mpsc::channel();
        let state = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("meeting track".into())
            .spawn(move || record(source, writer, &state, &stopped, started))?;
        Ok(Self { shared, stop, thread })
    }

    /// Input level, 0.0–1.0.
    pub fn level(&self) -> f32 {
        self.shared.sink.level()
    }

    pub fn problem(&self) -> Option<TrackProblem> {
        *self.shared.problem.lock().expect("track lock")
    }

    /// Stops and closes the file.
    pub fn stop(self) -> io::Result<()> {
        let _ = self.stop.send(());
        self.thread.join().map_err(|_| io::Error::other("the track thread panicked"))?
    }
}

/// Records until the stop. A write error (a full disk) ends the recording of this track: the
/// track reports the problem, shows no level and waits for the stop.
fn record(
    source: Source,
    writer: OggOpusWriter<BufWriter<File>>,
    shared: &Shared,
    stopped: &mpsc::Receiver<()>,
    started: Instant,
) -> io::Result<()> {
    let Err((error, stop_seen)) = record_until(source, writer, shared, stopped, started) else {
        return Ok(());
    };
    set_problem(shared, TrackProblem::CannotWrite, &format!("cannot write the recording: {error}"));
    shared.sink.level.store(0f32.to_bits(), Ordering::Relaxed);
    if !stop_seen {
        let _ = stopped.recv();
    }
    Err(error)
}

/// The error, and whether the stop had already arrived.
type RecordError = (io::Error, bool);

fn record_until(
    source: Source,
    mut writer: OggOpusWriter<BufWriter<File>>,
    shared: &Shared,
    stopped: &mpsc::Receiver<()>,
    started: Instant,
) -> Result<(), RecordError> {
    let mut timeline = Timeline::default();
    let mut open: Option<(Open, Resampler)> = None;
    let mut checked: Option<Instant> = None;
    loop {
        let stop = match stopped.recv_timeout(DRAIN_EVERY) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
            Err(RecvTimeoutError::Timeout) => false,
        };
        let failed = shared.sink.failure.lock().expect("track lock").take();
        let due = checked.is_none_or(|at| at.elapsed() >= CHECK_EVERY);
        if !stop && (due || failed.is_some()) {
            checked = Some(Instant::now());
            let wanted = source.wanted();
            let current = open.as_ref().map(|(open, _)| open.device.as_str());
            if needs_opening(failed.is_some(), wanted.as_deref(), current) {
                // What the old stream recorded goes in before the switch, at its own rate: the
                // stream is closed first, so none of its audio is left for the new one.
                if let Some((old, mut resampler)) = open.take() {
                    drop(old);
                    // No level until a new stream gives one: the device may not open.
                    shared.sink.level.store(0f32.to_bits(), Ordering::Relaxed);
                    let mut rest = resampler.push(&take(&shared.sink));
                    rest.extend(resampler.finish());
                    write(&mut writer, &mut timeline, started, &rest).map_err(|e| (e, stop))?;
                }
                if let Some(failure) = &failed {
                    log_line!("meeting track: {failure}; reopening");
                }
                open = match wanted.as_deref().map(|wanted| source.open(wanted, &shared.sink)) {
                    Some(Ok(new)) => {
                        device_opened(shared);
                        let resampler = Resampler::new(new.rate);
                        Some((new, resampler))
                    }
                    Some(Err(error)) => {
                        set_problem(shared, TrackProblem::NoDevice, &error);
                        None
                    }
                    None => {
                        set_problem(shared, TrackProblem::NoDevice, "no device");
                        None
                    }
                };
            }
        }
        let samples = take(&shared.sink);
        let mut out = match &mut open {
            Some((_, resampler)) => resampler.push(&samples),
            None => Vec::new(),
        };
        if stop && let Some((_, mut resampler)) = open.take() {
            out.extend(resampler.finish());
        }
        write(&mut writer, &mut timeline, started, &out).map_err(|e| (e, stop))?;
        if stop {
            writer.finish().map_err(|e| (e, true))?;
            return Ok(());
        }
    }
}

/// Whether the stream is opened (again): it failed, another device is wanted, or none is open
/// and none can be, which is a problem to say from the first check on.
fn needs_opening(failed: bool, wanted: Option<&str>, current: Option<&str>) -> bool {
    failed || wanted != current || current.is_none()
}

/// A stream opened: a missing device is no problem any more. A file that cannot be written
/// stays one: nothing is written after it.
fn device_opened(shared: &Shared) {
    let mut slot = shared.problem.lock().expect("track lock");
    if *slot == Some(TrackProblem::NoDevice) {
        *slot = None;
    }
}

/// Notes why the track records nothing; `detail` goes to the log once per problem.
fn set_problem(shared: &Shared, problem: TrackProblem, detail: &str) {
    let mut slot = shared.problem.lock().expect("track lock");
    if *slot != Some(problem) {
        log_line!("meeting track: {detail}");
    }
    *slot = Some(problem);
}

fn take(sink: &Sink) -> Vec<f32> {
    std::mem::take(&mut *sink.samples.lock().expect("track lock"))
}

fn write(
    writer: &mut OggOpusWriter<BufWriter<File>>,
    timeline: &mut Timeline,
    started: Instant,
    samples: &[f32],
) -> io::Result<()> {
    // A second at a time: a gap can be hours (the computer slept), and one buffer of that
    // length would take gigabytes.
    static SILENCE: [f32; 16_000] = [0.0; 16_000];
    let mut gap = timeline.gap(started.elapsed(), samples.len());
    while gap > 0 {
        let piece = gap.min(SILENCE.len());
        writer.write(&SILENCE[..piece])?;
        gap -= piece;
    }
    writer.write(samples)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::files::decode;

    /// A track with no device from the start says so at its first check, and goes on trying.
    #[test]
    fn a_track_without_a_device_tries_and_says_so() {
        assert!(needs_opening(false, None, None), "nothing to record from the start");
        assert!(needs_opening(false, Some("mic"), None));
        assert!(needs_opening(true, Some("mic"), Some("mic")));
        assert!(needs_opening(false, Some("usb"), Some("mic")));
        assert!(!needs_opening(false, Some("mic"), Some("mic")));
    }

    /// A track that cannot write says so, not that its device is missing; a device that opens
    /// again clears only a missing device.
    #[test]
    fn a_track_tells_a_full_disk_from_a_missing_device() {
        let shared = Shared::default();
        set_problem(&shared, TrackProblem::NoDevice, "no device");
        assert_eq!(*shared.problem.lock().unwrap(), Some(TrackProblem::NoDevice));
        device_opened(&shared);
        assert_eq!(*shared.problem.lock().unwrap(), None);
        set_problem(&shared, TrackProblem::CannotWrite, "cannot write the recording: full");
        device_opened(&shared);
        assert_eq!(*shared.problem.lock().unwrap(), Some(TrackProblem::CannotWrite));
        let event = crate::domain::events::MeetingEvent::Recording {
            meeting: 1,
            elapsed_ms: 0,
            me_level: 0,
            others_level: 0,
            me_problem: Some(TrackProblem::CannotWrite),
            others_problem: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["me_problem"], "cannot_write");
        assert!(json["others_problem"].is_null());
    }

    fn test_dir() -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("track-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Records three seconds of the computer's sound and of the microphone. Play something
    /// and, to check device switching, change the output or the microphone meanwhile.
    #[test]
    #[ignore = "uses the real devices: cargo test -- --ignored records_both_tracks --nocapture"]
    fn records_both_tracks() {
        let dir = test_dir();
        let started = Instant::now();
        let me =
            Track::start(Source::Microphone { preferred: None }, &dir.join("me.opus"), 1, started)
                .unwrap();
        let others = Track::start(Source::System, &dir.join("others.opus"), 2, started).unwrap();
        thread::sleep(Duration::from_secs(3));
        println!("levels: me {} others {}", me.level(), others.level());
        println!("problems: me {:?} others {:?}", me.problem(), others.problem());
        me.stop().unwrap();
        others.stop().unwrap();
        for name in ["me.opus", "others.opus"] {
            let decoded = decode(&dir.join(name)).unwrap();
            let seconds = decoded.samples.len() as f64 / 16_000.0;
            let peak = decoded.samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
            println!("{name}: {seconds:.2} s, peak {peak:.3}");
            assert!((2.7..=3.3).contains(&seconds), "{name}: {seconds} s");
        }
    }
}
