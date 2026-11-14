//! Recording and transcribing meetings: one recording and one transcription at a
//! time. The tracks record on their own threads; the transcription runs on a worker thread
//! that hands each track to the orchestrator as a file job, then tells the voices apart.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use std::{fs, io};

use super::echo::{self, Echo, drop_echo_words};
use super::mix;
use super::speakers::SpeakerModels;
use super::store::{
    CALL_FILE, Kind, ME_FILE, MeetingState, MeetingsError, OTHERS_FILE, SharedMeetings,
};
use super::track::{Source, Track};
use super::transcript::{SpeakerSegment, others_speakers, turns};
use crate::audio::ogg_opus::OggOpusWriter;
use crate::dictation::{FileRequest, FileSource, Message, TrackUpdate};
use crate::domain::events::{CoreEvent, FileProblem, MeetingEvent, MeetingProblem, MeetingStep};
use crate::domain::modes;
use crate::files::{ExportFormat, FileDecoder, Word};
use crate::platform;
use crate::util::now_ms;

const STATUS_EVERY: Duration = Duration::from_millis(200);
const DURATION_EVERY: Duration = Duration::from_secs(5);
/// A meeting track waits this long for the user's file to finish before trying again.
const BUSY_RETRY: Duration = Duration::from_secs(2);

pub type Emit = Arc<dyn Fn(CoreEvent) + Send + Sync>;
/// The meeting being transcribed and its stop flag.
type Transcribing = Arc<Mutex<Option<(i64, Arc<AtomicBool>)>>>;

struct Recording {
    id: i64,
    me: Track,
    others: Track,
    started: Instant,
    /// Ends the status thread.
    done: Arc<AtomicBool>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MeetingError {
    /// A meeting is already being recorded.
    Recording,
    /// Nothing is being recorded.
    NotRecording,
    /// This meeting is being recorded or transcribed.
    InUse,
    /// A file's transcript: it has no recording to transcribe.
    NotAMeeting,
    Store(String),
}

impl From<MeetingsError> for MeetingError {
    fn from(error: MeetingsError) -> Self {
        Self::Store(error.to_string())
    }
}

pub struct Meetings {
    store: Arc<SharedMeetings>,
    speakers: SpeakerModels,
    inbox: Sender<Message>,
    emit: Emit,
    recording: Arc<Mutex<Option<Recording>>>,
    /// Held while a stop or a discard ends the recording taken from `recording`: quitting waits
    /// for it, so the meeting is saved or deleted before the app ends.
    ending: Mutex<()>,
    transcribing: Transcribing,
    /// Set while recording: dictation then leaves the sound on (no muting).
    active: Arc<AtomicBool>,
}

impl Meetings {
    pub fn new(
        store: Arc<SharedMeetings>,
        speakers: SpeakerModels,
        inbox: Sender<Message>,
        emit: Emit,
        active: Arc<AtomicBool>,
    ) -> Self {
        Self {
            store,
            speakers,
            inbox,
            emit,
            recording: Arc::new(Mutex::new(None)),
            ending: Mutex::new(()),
            transcribing: Arc::new(Mutex::new(None)),
            active,
        }
    }

    pub fn store(&self) -> &SharedMeetings {
        &self.store
    }

    pub fn speakers(&self) -> &SpeakerModels {
        &self.speakers
    }

    /// The meeting being recorded, if any.
    pub fn recording_id(&self) -> Option<i64> {
        self.recording.lock().expect("meetings lock").as_ref().map(|recording| recording.id)
    }

    pub fn transcribing_id(&self) -> Option<i64> {
        self.transcribing.lock().expect("meetings lock").as_ref().map(|(id, _)| *id)
    }

    /// Starts recording both tracks into a new meeting.
    pub fn start(&self, microphone: Option<String>) -> Result<i64, MeetingError> {
        let mut slot = self.recording.lock().expect("meetings lock");
        if slot.is_some() {
            return Err(MeetingError::Recording);
        }
        let (id, folder) = self.store.with(|store| {
            let id = store.create(now_ms())?;
            Ok((id, store.folder(id)?))
        })?;
        let started = Instant::now();
        let source = Source::Microphone { preferred: microphone };
        let tracks = Track::start(source, &folder.join(ME_FILE), 1, started).and_then(|me| {
            match Track::start(Source::System, &folder.join(OTHERS_FILE), 2, started) {
                Ok(others) => Ok((me, others)),
                Err(error) => {
                    // Its file is closed before the folder is deleted (Windows keeps it busy).
                    let _ = me.stop();
                    Err(error)
                }
            }
        });
        let (me, others) = match tracks {
            Ok(tracks) => tracks,
            Err(error) => {
                let _ = self.store.with(|store| store.delete(id));
                return Err(MeetingError::Store(error.to_string()));
            }
        };
        self.active.store(true, Ordering::Relaxed);
        let done = Arc::new(AtomicBool::new(false));
        *slot = Some(Recording { id, me, others, started, done: Arc::clone(&done) });
        drop(slot);
        (self.emit)(CoreEvent::Meeting(MeetingEvent::Started { meeting: id }));
        self.status_thread(done);
        Ok(id)
    }

    /// Stops the recording, keeps it and, with `transcribe`, starts its transcription. While
    /// another meeting is being transcribed this one stays recorded, to be transcribed later:
    /// the stop succeeded.
    pub fn stop(&self, transcribe: bool) -> Result<i64, MeetingError> {
        let ending = self.ending.lock().expect("meetings lock");
        let (id, saved) = self.finish_recording()?;
        drop(ending);
        (self.emit)(CoreEvent::Meeting(MeetingEvent::Recorded { meeting: id }));
        saved?;
        if transcribe && let Err(error) = self.transcribe(id) {
            log_line!("meeting {id}: not transcribed now: {error:?}");
        }
        Ok(id)
    }

    /// Stops the recording and deletes it.
    pub fn discard(&self) -> Result<(), MeetingError> {
        let ending = self.ending.lock().expect("meetings lock");
        // Its end need not be saved: it is deleted.
        let (id, _) = self.finish_recording()?;
        let deleted = self.store.with(|store| store.delete(id));
        drop(ending);
        (self.emit)(CoreEvent::Meeting(MeetingEvent::Discarded { meeting: id }));
        deleted?;
        Ok(())
    }

    /// Stops the recording: its id, and whether the database took its end. The recording is over
    /// either way (a full disk refuses the write), so the caller still tells the interface.
    fn finish_recording(&self) -> Result<(i64, Result<(), MeetingError>), MeetingError> {
        let recording = self.recording.lock().expect("meetings lock").take();
        let Some(Recording { id, me, others, started, done }) = recording else {
            return Err(MeetingError::NotRecording);
        };
        done.store(true, Ordering::Relaxed);
        self.active.store(false, Ordering::Relaxed);
        let duration = started.elapsed();
        let results = [me.stop(), others.stop()];
        for result in results {
            if let Err(error) = result {
                log_line!("meeting track: {error}");
            }
        }
        let duration_ms = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
        let saved = self.store.with(|store| store.set_recorded(id, duration_ms));
        Ok((id, saved.map_err(MeetingError::from)))
    }

    /// Transcribes a recorded meeting on a worker thread.
    pub fn transcribe(&self, id: i64) -> Result<(), MeetingError> {
        if self.recording_id() == Some(id) {
            return Err(MeetingError::InUse);
        }
        if self.store.with(|store| store.get(id))?.summary.kind == Kind::File {
            return Err(MeetingError::NotAMeeting);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut slot = self.transcribing.lock().expect("meetings lock");
            if slot.is_some() {
                return Err(MeetingError::InUse);
            }
            *slot = Some((id, Arc::clone(&cancel)));
        }
        let folder = self.store.with(|store| {
            store.set_state(id, MeetingState::Transcribing, None)?;
            store.folder(id)
        });
        let folder = match folder {
            Ok(folder) => folder,
            Err(error) => {
                *self.transcribing.lock().expect("meetings lock") = None;
                return Err(error.into());
            }
        };
        let job = Job {
            id,
            folder,
            store: Arc::clone(&self.store),
            speakers: self.speakers.clone(),
            inbox: self.inbox.clone(),
            emit: Arc::clone(&self.emit),
            cancel,
        };
        let slot = Arc::clone(&self.transcribing);
        let emit = Arc::clone(&self.emit);
        let store = Arc::clone(&self.store);
        thread::spawn(move || {
            let event = guarded(id, &store, move || job.run());
            platform::release_memory();
            // Free first: a window that reads the meetings on this event sees it finished.
            *slot.lock().expect("meetings lock") = None;
            emit(CoreEvent::Meeting(event));
        });
        Ok(())
    }

    /// Stops the transcription; the recording stays.
    pub fn cancel_transcription(&self, id: i64) {
        let slot = self.transcribing.lock().expect("meetings lock");
        if let Some((current, cancel)) = slot.as_ref()
            && *current == id
        {
            cancel.store(true, Ordering::Relaxed);
            let _ = self.inbox.send(Message::CancelTrack);
        }
    }

    pub fn delete(&self, id: i64) -> Result<(), MeetingError> {
        if self.recording_id() == Some(id) || self.transcribing_id() == Some(id) {
            return Err(MeetingError::InUse);
        }
        self.store.with(|store| store.delete(id))?;
        (self.emit)(CoreEvent::Meeting(MeetingEvent::Changed));
        Ok(())
    }

    pub fn rename(&self, id: i64, title: Option<&str>) -> Result<(), MeetingError> {
        self.store.with(|store| store.rename(id, title))?;
        (self.emit)(CoreEvent::Meeting(MeetingEvent::Changed));
        Ok(())
    }

    /// A file entry with no saved file got one ("Save as…"): it is its saved file from now on.
    pub fn file_saved(&self, id: i64, output: &Path) -> Result<(), MeetingError> {
        self.store.with(|store| store.set_output(id, output))?;
        (self.emit)(CoreEvent::Meeting(MeetingEvent::Changed));
        Ok(())
    }

    /// Tells every window to read the meetings again, e.g. after the speaker models changed.
    pub fn changed(&self) {
        (self.emit)(CoreEvent::Meeting(MeetingEvent::Changed));
    }

    /// Stops a recording left going when the app quits; it is kept. A stop or a discard already
    /// on its way is waited for: the commands run off the main thread, and quitting would end it
    /// halfway (a discarded meeting kept, a stopped one without its end).
    pub fn shutdown(&self) {
        let _ending = self.ending.lock().expect("meetings lock");
        if self.recording_id().is_some() {
            let _ = self.finish_recording();
        }
    }

    fn status_thread(&self, done: Arc<AtomicBool>) {
        let emit = Arc::clone(&self.emit);
        let recording = Arc::clone(&self.recording);
        let store = Arc::clone(&self.store);
        thread::spawn(move || {
            let mut saved = Instant::now();
            while !done.load(Ordering::Relaxed) {
                // Held until the event is out: a stop waits for it, so no "recording" follows
                // the end and no shorter length overwrites the final one.
                let current = recording.lock().expect("meetings lock");
                if let Some(event) = current.as_ref().map(status) {
                    // The length so far is kept, so a crash leaves a meeting with its length.
                    if let MeetingEvent::Recording { meeting, elapsed_ms, .. } = &event
                        && saved.elapsed() >= DURATION_EVERY
                    {
                        saved = Instant::now();
                        let duration = i64::try_from(*elapsed_ms).unwrap_or(i64::MAX);
                        let _ = store.with(|store| store.set_duration(*meeting, duration));
                    }
                    emit(CoreEvent::Meeting(event));
                }
                drop(current);
                thread::sleep(STATUS_EVERY);
            }
        });
    }
}

fn status(recording: &Recording) -> MeetingEvent {
    let level = |value: f32| (value * 1000.0).clamp(0.0, 1000.0) as u16;
    MeetingEvent::Recording {
        meeting: recording.id,
        elapsed_ms: recording.started.elapsed().as_millis() as u64,
        me_level: level(recording.me.level()),
        others_level: level(recording.others.level()),
        me_problem: recording.me.problem(),
        others_problem: recording.others.problem(),
    }
}

struct Job {
    id: i64,
    folder: PathBuf,
    store: Arc<SharedMeetings>,
    speakers: SpeakerModels,
    inbox: Sender<Message>,
    emit: Emit,
    cancel: Arc<AtomicBool>,
}

enum Ended {
    Cancelled,
    Failed(MeetingProblem, String),
}

impl Job {
    /// The transcription and its final event, for the caller to send once the slot is free.
    fn run(self) -> MeetingEvent {
        match self.transcript() {
            Ok(()) => MeetingEvent::Done { meeting: self.id },
            Err(Ended::Cancelled) => {
                // As it was: a meeting transcribed before keeps that transcript and stays done.
                let _ = self.store.with(|store| store.rest(self.id));
                MeetingEvent::Cancelled { meeting: self.id }
            }
            Err(Ended::Failed(problem, detail)) => {
                let _ = self.store.with(|store| {
                    store.set_state(self.id, MeetingState::Failed, Some(problem.code()))
                });
                MeetingEvent::Failed { meeting: self.id, problem, detail }
            }
        }
    }

    fn transcript(&self) -> Result<(), Ended> {
        // What a crash or a quit in the middle of an earlier run left behind.
        let _ = fs::remove_file(self.folder.join(CLEAN_FILE));
        let _ = fs::remove_file(mix::part_path(&self.folder.join(CALL_FILE)));
        // Recorded from speakers: the echo of the others is silenced in a copy of the
        // microphone track, which is transcribed instead and deleted afterwards.
        // Reading both tracks for the echo takes a while on a long call: the first step shows
        // at once, and a stop is seen before the tracks are transcribed.
        self.progress(MeetingStep::Me, 0, None);
        let cleaned = self.without_echo();
        let copy = cleaned.as_ref().and_then(|cleaned| cleaned.copy.clone());
        if let Err(stopped) = self.stopped() {
            if let Some(path) = &copy {
                let _ = fs::remove_file(path);
            }
            return Err(stopped);
        }
        let me_path = copy.clone().unwrap_or_else(|| self.folder.join(ME_FILE));
        // A stop here is seen by the transcription of the track, which also deletes the copy.
        self.call_file(&me_path);
        let me = self.track(MeetingStep::Me, &me_path);
        if let Some(path) = &copy {
            let _ = fs::remove_file(path);
        }
        let mut me = me?;
        let others = self.track(MeetingStep::Others, &self.folder.join(OTHERS_FILE))?;
        if let Some(cleaned) = &cleaned {
            me = drop_echo_words(me, &others, cleaned.echo.delay_seconds());
        }
        if me.is_empty() && others.is_empty() {
            return Err(Ended::Failed(MeetingProblem::NoSpeech, String::new()));
        }
        let segments = if others.is_empty() { Vec::new() } else { self.voices(&others)? };
        let turns = turns(me, others_speakers(others, &segments));
        self.store
            .with(|store| store.set_transcript(self.id, &turns))
            .map_err(|error| Ended::Failed(MeetingProblem::Save, error.to_string()))
    }

    /// Both tracks in one file to listen to, once: the microphone without the echo where it was
    /// silenced, so the others are not heard twice. A failure costs only this file.
    fn call_file(&self, me: &Path) {
        let target = self.folder.join(CALL_FILE);
        if target.exists() {
            return;
        }
        let others = self.folder.join(OTHERS_FILE);
        if let Err(error) = mix::mix(&[me, &others], &target, &self.cancel)
            && !self.cancel.load(Ordering::Relaxed)
        {
            log_line!("meeting {}: no call file: {error}", self.id);
        }
    }

    /// The echo, if the meeting was recorded from speakers, and the microphone track with it
    /// silenced when there is something to silence. Any problem here leaves the track as
    /// recorded.
    fn without_echo(&self) -> Option<Cleaned> {
        let me = self.folder.join(ME_FILE);
        // A long call takes a while to read: a stop is seen block by block.
        let me_energy = echo::file_energies(&me, &self.cancel).ok()?;
        let others_energy =
            echo::file_energies(&self.folder.join(OTHERS_FILE), &self.cancel).ok()?;
        let found = echo::find_echo(&me_energy, &others_energy)?;
        let mask = echo::echo_mask(&me_energy, &others_energy, &found);
        let silenced = mask.iter().filter(|frame| **frame).count();
        log_line!(
            "meeting {}: echo {} ms late, correlation {:.2}; {silenced} of {} frames silenced",
            self.id,
            (found.delay_seconds() * 1000.0).round(),
            found.correlation,
            mask.len()
        );
        // Nothing to silence: the track is used as recorded, not encoded once more.
        if silenced == 0 {
            return Some(Cleaned { copy: None, echo: found });
        }
        let path = self.folder.join(CLEAN_FILE);
        match write_gated(&me, &path, &mask, &self.cancel) {
            Ok(()) => Some(Cleaned { copy: Some(path), echo: found }),
            Err(error) => {
                if !self.cancel.load(Ordering::Relaxed) {
                    log_line!("meeting {}: cannot silence the echo: {error}", self.id);
                }
                let _ = fs::remove_file(&path);
                // The echo is known all the same: its words still leave the user's turns.
                Some(Cleaned { copy: None, echo: found })
            }
        }
    }

    fn stopped(&self) -> Result<(), Ended> {
        if self.cancel.load(Ordering::Relaxed) { Err(Ended::Cancelled) } else { Ok(()) }
    }

    /// One track through the orchestrator, as a file. While the user's own file is being
    /// transcribed the track waits its turn instead of failing.
    fn track(&self, step: MeetingStep, path: &Path) -> Result<Vec<Word>, Ended> {
        self.progress(step, 0, None);
        loop {
            match self.track_once(step, path)? {
                Some(words) => return Ok(words),
                None => thread::sleep(BUSY_RETRY),
            }
        }
    }

    /// The track's words, or `None` while the orchestrator is busy with the user's file.
    fn track_once(&self, step: MeetingStep, path: &Path) -> Result<Option<Vec<Word>>, Ended> {
        self.stopped()?;
        let (reply, updates) = mpsc::channel();
        let request = FileRequest {
            source: FileSource::Media(path.to_path_buf()),
            name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            // A track saves nothing: its words come back in `TrackUpdate::Done`.
            output: PathBuf::new(),
            format: ExportFormat::Txt,
            mode_id: modes::PLAIN.into(),
            replacements: true,
            speakers: None,
            reply: Some(reply),
        };
        if self.inbox.send(Message::StartFile(Box::new(request))).is_err() {
            return Err(Ended::Cancelled);
        }
        // A stop asked for just before the start reached the orchestrator ahead of the track.
        if self.cancel.load(Ordering::Relaxed) {
            let _ = self.inbox.send(Message::CancelTrack);
        }
        for update in updates {
            match update {
                TrackUpdate::Progress { done_ms, total_ms } => {
                    self.progress(step, done_ms, total_ms)
                }
                TrackUpdate::Done(words) => return Ok(Some(words)),
                TrackUpdate::Failed { problem: FileProblem::Busy, .. } => return Ok(None),
                TrackUpdate::Failed { problem, detail } => {
                    return Err(Ended::Failed(problem.into(), detail));
                }
                TrackUpdate::Cancelled => return Err(Ended::Cancelled),
            }
        }
        // The orchestrator ended (the app is quitting).
        Err(Ended::Cancelled)
    }

    /// The voices of the computer's sound, by sentence of its `words`; none without the models.
    /// A failure keeps the transcript with one speaker for the computer's sound.
    fn voices(&self, words: &[Word]) -> Result<Vec<SpeakerSegment>, Ended> {
        self.stopped()?;
        if !self.speakers.ready() {
            return Ok(Vec::new());
        }
        // First where voices speak, with no measure of how far; the first share means the
        // second model has started.
        self.progress(MeetingStep::Segments, 0, None);
        // How far it is, as the share of the recording's length. The worker reads the track
        // itself: a long recording is not held in this process too.
        let total_ms = self
            .store
            .with(|store| store.get(self.id))
            .map_or(0, |meeting| u64::try_from(meeting.summary.duration_ms).unwrap_or(0));
        let mut report = |share: f64| {
            let done_ms = (share.clamp(0.0, 1.0) * total_ms as f64) as u64;
            self.progress(MeetingStep::Voices, done_ms, Some(total_ms));
        };
        let found =
            self.speakers.diarize(&self.folder.join(OTHERS_FILE), words, &self.cancel, &mut report);
        self.stopped()?;
        match found {
            Ok(segments) => Ok(segments),
            Err(detail) => {
                log_line!("meeting {}: speakers: {detail}", self.id);
                Ok(Vec::new())
            }
        }
    }

    fn progress(&self, step: MeetingStep, done_ms: u64, total_ms: Option<u64>) {
        (self.emit)(CoreEvent::Meeting(MeetingEvent::Progress {
            meeting: self.id,
            step,
            done_ms,
            total_ms,
        }));
    }
}

/// The microphone copy without the echo, deleted after its transcription.
const CLEAN_FILE: &str = "me-without-echo.opus";

/// Runs a transcription; a panic in it (a decoder on a damaged track) fails the meeting instead
/// of leaving it transcribing, and the slot is freed all the same.
fn guarded(id: i64, store: &SharedMeetings, run: impl FnOnce() -> MeetingEvent) -> MeetingEvent {
    panic::catch_unwind(AssertUnwindSafe(run)).unwrap_or_else(|_| {
        let problem = MeetingProblem::Read;
        let _ = store.with(|store| store.set_state(id, MeetingState::Failed, Some(problem.code())));
        // The problem says it in the user's language; the panic itself went to the log.
        MeetingEvent::Failed { meeting: id, problem, detail: String::new() }
    })
}

/// The echo found in the microphone track, and the copy with it silenced (`None` when nothing
/// needed silencing or the copy could not be written: the track is used as recorded).
struct Cleaned {
    copy: Option<PathBuf>,
    echo: Echo,
}

fn write_gated(
    source: &Path,
    target: &Path,
    mask: &[bool],
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut decoder = FileDecoder::open(source).map_err(|e| e.to_string())?;
    let file = fs::File::create(target).map_err(|e| e.to_string())?;
    let mut writer = OggOpusWriter::new(io::BufWriter::new(file), 3).map_err(|e| e.to_string())?;
    let mut position = 0;
    while let Some(mut block) = decoder.next_block().map_err(|e| e.to_string())? {
        if cancel.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        echo::gate(&mut block, position, mask);
        position += block.len();
        writer.write(&block).map_err(|e| e.to_string())?;
    }
    writer.finish().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::meetings::store::FileTranscript;

    #[test]
    fn a_file_transcript_is_not_transcribed_as_a_meeting() {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts").join(
            format!("service-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)),
        );
        let _ = fs::remove_dir_all(&dir);
        let store =
            Arc::new(SharedMeetings::new(dir.join("meetings.sqlite3"), dir.join("meetings")));
        let file = FileTranscript {
            source: dir.join("talk.mp3"),
            output: Some(dir.join("talk.md")),
            duration_ms: 1_000,
            turns: Vec::new(),
            processed: None,
        };
        let id = store.with(|store| store.add_file(1, &file)).unwrap();
        let (inbox, messages) = mpsc::channel();
        let meetings = Meetings::new(
            Arc::clone(&store),
            SpeakerModels::new(dir.join("speakers")),
            inbox,
            Arc::new(|_: CoreEvent| {}),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(meetings.transcribe(id), Err(MeetingError::NotAMeeting));
        assert_eq!(meetings.transcribing_id(), None);
        assert!(messages.try_recv().is_err(), "nothing went to the orchestrator");
        let entry = store.with(|store| store.get(id)).unwrap();
        assert_eq!(entry.summary.state, MeetingState::Done);
    }

    /// A panic while a meeting is transcribed fails it with a reading problem; the slot is freed
    /// by the caller all the same.
    #[test]
    fn a_panic_while_transcribing_fails_the_meeting() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("service-panic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let store = SharedMeetings::new(dir.join("m.sqlite3"), dir.join("meetings"));
        let id = store.with(|store| store.create(1)).unwrap();
        let event = guarded(id, &store, || panic!("damaged track"));
        assert!(matches!(event, MeetingEvent::Failed { problem: MeetingProblem::Read, .. }));
        let entry = store.with(|store| store.get(id)).unwrap();
        assert_eq!(entry.summary.state, MeetingState::Failed);
    }

    /// The copy without the echo cannot be written: the echo found still takes its words out of
    /// the user's turns.
    #[test]
    fn an_echo_without_its_copy_is_still_known() {
        use crate::audio::ogg_opus::OggOpusWriter;
        use crate::meetings::echo::tests::{MY_TURNS, OTHERS_TURNS, mix, through_the_room, voice};

        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("service-echo-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(CLEAN_FILE)).unwrap();
        let others = voice(7, 20.0, &OTHERS_TURNS, 0.3);
        let me = mix(&through_the_room(&others, 120, 0.25), &voice(99, 20.0, &MY_TURNS, 0.4));
        for (name, samples) in [(ME_FILE, &me), (OTHERS_FILE, &others)] {
            let file = fs::File::create(dir.join(name)).unwrap();
            let mut writer = OggOpusWriter::new(file, 1).unwrap();
            writer.write(samples).unwrap();
            writer.finish().unwrap();
        }
        let (inbox, _messages) = mpsc::channel();
        let job = Job {
            id: 1,
            folder: dir.clone(),
            store: Arc::new(SharedMeetings::new(dir.join("m.sqlite3"), dir.join("meetings"))),
            speakers: SpeakerModels::new(dir.join("speakers")),
            inbox,
            emit: Arc::new(|_: CoreEvent| {}),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let cleaned = job.without_echo().expect("the echo is found");
        assert!(cleaned.copy.is_none(), "the copy's place is taken by a folder");
        assert!(cleaned.echo.lag > 0);
    }
}
