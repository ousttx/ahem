use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::services::{HOTKEYS_HELD, LlmJob, PendingContext, Recording, Services, SpeechEngine};
use crate::audio::capture::Captured;
use crate::audio::output::{self, OutputFormat};
use crate::audio::speech;
use crate::domain::app_rules::{self, AppId, RecentApps};
use crate::domain::clean_text::{self, CleanText};
use crate::domain::engine::{EngineEffect, EngineEvent, EngineInput, EngineLifecycle, EngineState};
use crate::domain::events::{
    BlockReason, CoreEvent, DictationEvent, FileEvent, FileProblem, SelectionProblem, SpeakersStep,
};
use crate::domain::gesture::{Gesture, HotkeyGesture, RecordingMode};
use crate::domain::modes::{self, HotkeyAction, Mode, ModeAction};
use crate::domain::replacements::{Replacement, Replacer};
use crate::domain::scheduler::{FileTaskId, Job, Scheduler, SchedulerInput};
use crate::domain::services::Service;
use crate::domain::session::{
    DictationResult, DictationSession, FailureKind, InsertionProblem, SessionEvent, SessionId,
    SessionInput, SessionMode, SessionState,
};
use crate::engines::cloud;
use crate::engines::sherpa::tidy;
use crate::engines::whisper_server::{TranscribeOptions, Transcript, prompt_from_dictionary};
use crate::engines::{hallucinations, languages};
use crate::files::{
    ExportFormat, Window, Word, export, merge, paragraphs, parse_words, replace_words,
    without_credits,
};
use crate::hotkeys;
use crate::meetings::store::FileTranscript;
use crate::meetings::transcript::{self, Labels, Speaker, SpeakerSegment, Turn};
use crate::platform::{FocusTarget, InsertError, SelectionError};
use crate::storage::settings::Settings;
use crate::storage::stats;

const LEVEL_INTERVAL: Duration = Duration::from_millis(50);
/// A headset is back from its headset mode within this; the sound returns then anyway. AirPods
/// on macOS stay in the headset mode about 2.2 s after the microphone closes, and their sound
/// is back 2.8–3.3 s after it.
const RESTORE_LIMIT: Duration = Duration::from_millis(5000);
/// The output format comes back at the start of the switch, not at its end: AirPods play again
/// 0.6–1.1 s later.
const SWITCH_SETTLE: Duration = Duration::from_millis(1300);
/// How often a streaming model gets the newly recorded audio.
const FEED_INTERVAL: Duration = Duration::from_millis(200);

/// The user's file and meeting tracks: one at a time, window by window (in `file_job.rs`).
mod file_job;

/// Why the language model gave no text; the detail never quotes the text or a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessingError {
    pub detail: String,
    pub kind: ProcessingFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingFailure {
    Failed,
    /// The ChatGPT plan's limit is reached: the interface links to its usage settings.
    PlanLimit,
    /// The model answered with nothing.
    Empty,
}

/// For tests: the backend builds its errors with their kind.
#[cfg(test)]
impl From<&str> for ProcessingError {
    fn from(detail: &str) -> Self {
        Self { detail: detail.into(), kind: ProcessingFailure::Failed }
    }
}

#[derive(Debug)]
pub enum Message {
    HotkeyPressed(HotkeyAction),
    HotkeyReleased(HotkeyAction),
    Settings(Box<Settings>),
    EngineReady,
    EngineFailed(String),
    EngineCrashed(String),
    EngineStopped,
    Transcribed {
        session: SessionId,
        result: Result<Transcript, String>,
    },
    /// The context of a dictation was read; `found`: some source had text.
    ContextRead {
        id: u64,
        found: bool,
    },
    /// The language model's answer for a dictation.
    Processed {
        session: SessionId,
        result: Result<String, ProcessingError>,
    },
    Inserted {
        session: SessionId,
        result: Result<(), InsertError>,
    },
    /// The streaming model's whole text so far for a dictation being recorded.
    LiveText {
        session: SessionId,
        text: String,
    },
    /// A piece of the live text was typed (or not).
    LiveTyped {
        session: SessionId,
        result: Result<(), InsertError>,
    },
    /// A selected-text rewrite finished.
    Rewritten {
        request: u64,
        result: Result<(), SelectionError>,
    },
    StartFile(Box<FileRequest>),
    /// Cancels the user's file (the Transcripts page), not a meeting track.
    CancelFile,
    /// Cancels a meeting track, not the user's file.
    CancelTrack,
    /// The next window of a file from its reader.
    FileWindow {
        file: FileTaskId,
        window: Window,
        audio: Captured,
        duration: Option<Duration>,
    },
    /// The file's audio ended without another window.
    FileEnded {
        file: FileTaskId,
    },
    FileReadFailed {
        file: FileTaskId,
        detail: String,
    },
    /// The engine's JSON with word times for one window.
    ChunkTranscribed {
        file: FileTaskId,
        chunk: u32,
        result: Result<String, String>,
    },
    FileProcessed {
        file: FileTaskId,
        result: Result<String, String>,
    },
    /// How far telling the voices of a file apart is, from 0 to 1.
    FileDiarizing {
        file: FileTaskId,
        share: f64,
    },
    /// The voices of a file; an error leaves the transcript without speakers.
    FileDiarized {
        file: FileTaskId,
        result: Result<Vec<SpeakerSegment>, String>,
    },
    /// Recognize the recording of the last failed dictation again.
    RetryDictation,
    /// The audio of a dictation the engine could not recognize, given back before its error.
    Unrecognized {
        session: SessionId,
        audio: Captured,
    },
    /// The result of the last file that could not be written (`held`), to be written where the
    /// user chose now, off this thread, with its entry in the list of transcripts.
    TakeHeld {
        reply: Sender<Option<HeldResult>>,
    },
    Shutdown,
}

/// The result of a file that could not be written, and its entry in the list of transcripts if
/// it could be kept there.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldResult {
    pub text: String,
    pub transcript: Option<i64>,
}

/// What to transcribe or process, and where the result goes.
#[derive(Debug, Clone)]
pub struct FileRequest {
    pub source: FileSource,
    /// The file name without folders: the title and what the interface shows.
    pub name: String,
    pub output: PathBuf,
    pub format: ExportFormat,
    pub mode_id: String,
    /// The user's replacements apply to the recognized words.
    pub replacements: bool,
    /// Tell the voices apart and name them so (only media, only with the speaker models).
    pub speakers: Option<Labels>,
    /// A meeting track: the words come back here instead of being saved or
    /// processed, and the interface gets no file events for it.
    pub reply: Option<Sender<TrackUpdate>>,
}

/// How the transcription of a meeting track goes.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackUpdate {
    Progress {
        done_ms: u64,
        total_ms: Option<u64>,
    },
    /// The merged words with the replacements; none if nobody spoke.
    Done(Vec<Word>),
    Failed {
        problem: FileProblem,
        detail: String,
    },
    Cancelled,
}

#[derive(Debug, Clone)]
pub enum FileSource {
    /// Audio or video, transcribed window by window.
    Media(PathBuf),
    /// A text file's contents, only processed by the mode's language model.
    Text(String),
}

struct FileJob {
    id: FileTaskId,
    request: FileRequest,
    mode: Mode,
    /// Read and queued in the scheduler.
    waiting: Option<(Window, Captured)>,
    /// At the engine.
    running: Option<Window>,
    parts: Vec<(Window, Vec<Word>)>,
    duration: Option<Duration>,
    /// No window comes after the ones read.
    ended: bool,
    holds_engine: bool,
    /// The merged transcript, kept to save it if the language model fails.
    words: Vec<Word>,
    /// The words as turns, by speaker when the voices were told apart: what the list of
    /// transcripts keeps.
    turns: Vec<Turn>,
    /// The language model's text, kept with the transcript.
    processed: Option<String>,
}

impl FileJob {
    /// The file's length, once its reader knows it.
    fn total_ms(&self) -> Option<u64> {
        self.duration.map(|duration| duration.as_millis() as u64)
    }

    /// The transcript in the file's format, as the list of transcripts saves it again: by
    /// speaker when the voices were asked for and told apart, else as plain words.
    fn exported(&self) -> String {
        match &self.request.speakers {
            Some(labels) => transcript::export_file(&self.turns, &self.request.format, labels),
            None => export(&self.words, &self.request.format),
        }
    }
}

struct Active {
    session: DictationSession,
    mode: Mode,
    /// Audio waiting for the engine; taken when the job starts.
    audio: Option<Captured>,
    target: Option<FocusTarget>,
    /// The hotkey that started it; only the same hotkey stops it (or settings that no longer
    /// name it end it). None for a recording recognized again.
    trigger: Option<HotkeyAction>,
    /// What the mode's language model sees besides the dictation; taken when it is called.
    context: Option<PendingContext>,
    /// From the microphone's opening until the transcript (or the end of the session).
    holds_engine: bool,
    /// Fed to a streaming model while recording.
    live: Option<Live>,
    /// How long the microphone recorded, for the statistics.
    recorded: Duration,
    /// The audio the engine gave back with its error, to be recognized again.
    returned: Option<Captured>,
    /// The dictation whose speech this is: its own, or for a recording recognized again the one
    /// that failed. Which failed recording is kept goes by it, not by the session's number.
    spoken: SessionId,
    /// Its text is ready to go in but waits for an earlier dictation's (`typing_waits`).
    insert_queued: bool,
}

impl Active {
    /// A dictation that holds the engine and has recorded nothing yet.
    fn new(session: DictationSession, mode: Mode, spoken: SessionId) -> Self {
        Self {
            session,
            mode,
            audio: None,
            target: None,
            trigger: None,
            context: None,
            holds_engine: true,
            live: None,
            recorded: Duration::ZERO,
            returned: None,
            spoken,
            insert_queued: false,
        }
    }
}

/// The recording of the last dictation that could not be recognized, in memory only (at 16 kHz
/// as the engine gave it back, or at the microphone's rate when it never reached the engine):
/// until it is recognized again, a dictation started after the failure gives text, a newer
/// failure replaces it, or the app quits.
struct FailedDictation {
    /// The dictation whose speech it is (for a retry, the one that failed first): the newest
    /// speech wins, an older one that fails later does not replace it.
    session: SessionId,
    /// The first dictation pressed after the failure (numbers are taken at the press): one
    /// pressed before it, still recording at the failure, does not let it go.
    since: SessionId,
    audio: Captured,
    mode_id: String,
}

/// A dictation fed to a streaming model (Nemotron) while it is recorded. Its text only grows,
/// so new text can be typed as it arrives; nothing typed is ever erased.
struct Live {
    /// The stream is open at the engine: a ready streaming one. Not yet while the engine loads,
    /// is being stopped or runs a model no longer chosen, or while another dictation's stream
    /// is open: it opens once a streaming engine is ready, or the other stream finished
    /// (`open_waiting_stream`); at an engine that does not stream, never, and the recording is
    /// recognized whole. An engine started again (after a crash) has no stream: it opens anew.
    opened: bool,
    /// Recorded samples already sent to the engine.
    fed: usize,
    last_feed: Instant,
    /// Text goes into the field while speaking: toggle mode (no hotkey modifiers are held) and
    /// a mode that inserts the transcript as it is.
    typing: bool,
    /// Auto mode with modifiers held: typing starts if a tap leaves the recording going.
    typing_after_tap: bool,
    /// What has been typed, with single spaces.
    typed: String,
    /// The piece being typed; one at a time, so pieces keep their order.
    in_flight: Option<String>,
    /// The engine's text so far, with single spaces and the replacements; a phrase that may
    /// still be completing at its end is not in it yet.
    latest: String,
    /// The final insertion waits for the piece being typed.
    insert_waiting: bool,
    /// The engine has the end of the recording; nothing to cancel.
    finished: bool,
}

impl Live {
    /// Its stream is open at the engine and not finished.
    fn streaming(&self) -> bool {
        self.opened && !self.finished
    }
}

pub struct Controller<S: Services> {
    services: S,
    settings: Settings,
    gesture: HotkeyGesture,
    /// The dictation hotkeys held now; their number is published in `HOTKEYS_HELD`. A set, so a
    /// key's auto-repeat or a lost release cannot make it count forever.
    held: HashSet<HotkeyAction>,
    engine: EngineLifecycle,
    scheduler: Scheduler,
    sessions: HashMap<SessionId, Active>,
    recording: Option<(SessionId, Box<dyn Recording>)>,
    next_session: u64,
    idle_timer: Option<(Instant, u64)>,
    /// Why the engine last failed; cleared when it becomes ready.
    engine_error: Option<String>,
    /// The engine started last is Whisper, which invents credits on silence: answers come from
    /// it, not from the one chosen since.
    whisper_started: bool,
    /// The engine started last streams: only its stream may be opened, once it is ready.
    streaming_started: bool,
    /// The output format before the microphone opened, to tell when a headset is back.
    output_before: Option<OutputFormat>,
    /// The microphone closed but the headset is still in its headset mode: the sound waits
    /// until it is back, so the music does not play muffled and then break off for the switch.
    restoring: Option<Restoring>,
    /// The selected-text rewrite in progress; one at a time.
    rewriting: Option<u64>,
    /// The programs dictated into lately, for the interface to offer in a mode's rules.
    recent_apps: Arc<Mutex<RecentApps>>,
    next_request: u64,
    /// One file at a time.
    file: Option<FileJob>,
    /// A window of a file that ended while the window was at the engine: it holds the engine
    /// until its answer, so a crash it causes restarts the engine for the work still waiting.
    ended_chunk: Option<FileTaskId>,
    next_file: u64,
    /// The result of the last file that could not be written (a drive gone, a full disk), held
    /// until the next file, so hours of work can be saved elsewhere (`Message::TakeHeld`).
    held_result: Option<HeldResult>,
    last_result: Arc<Mutex<Option<DictationResult>>>,
    failed: Option<FailedDictation>,
    /// Whether `failed` holds a recording, for the interface to ask (`retry_available`).
    retry_available: Arc<AtomicBool>,
    emit: Box<dyn Fn(CoreEvent) + Send>,
}

/// Waiting for a headset to come back from its headset mode.
struct Restoring {
    until: Instant,
    /// When the output format came back: the switch is under way.
    back_at: Option<Instant>,
}

impl Restoring {
    fn deadline(&self) -> Instant {
        self.back_at.map_or(self.until, |at| (at + SWITCH_SETTLE).min(self.until))
    }
}

/// A dictation at its press: what its microphone opens with.
struct Opening {
    /// Taken at the press: a dictation pressed before a failure is never taken for one after it.
    id: SessionId,
    mode_id: String,
    trigger: HotkeyAction,
    /// A rule for the program in focus chose the mode instead of the main one.
    by_app: bool,
    /// Read at the press, while the focus is still where the user was.
    context: PendingContext,
    /// The program's name is taken: the capsule shows it once recording starts; a source read
    /// later is shown when its text comes (`context_read`).
    context_taken: bool,
    /// Where the focus was at the press.
    target: Option<FocusTarget>,
    engine: SpeechEngine,
}

impl<S: Services> Controller<S> {
    pub fn new(
        services: S,
        settings: Settings,
        last_result: Arc<Mutex<Option<DictationResult>>>,
        recent_apps: Arc<Mutex<RecentApps>>,
        emit: Box<dyn Fn(CoreEvent) + Send>,
    ) -> Self {
        Self {
            services,
            gesture: HotkeyGesture::new(settings.recording_mode),
            held: HashSet::new(),
            engine: EngineLifecycle::new(settings.unload_policy),
            settings,
            scheduler: Scheduler::new(),
            sessions: HashMap::new(),
            recording: None,
            next_session: 1,
            idle_timer: None,
            engine_error: None,
            whisper_started: false,
            streaming_started: false,
            output_before: None,
            restoring: None,
            rewriting: None,
            recent_apps,
            next_request: 1,
            file: None,
            ended_chunk: None,
            next_file: 1,
            held_result: None,
            last_result,
            failed: None,
            retry_available: Arc::new(AtomicBool::new(false)),
            emit,
        }
    }

    /// Whether a failed dictation can be recognized again, shared with the interface's commands.
    pub fn retry_available(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.retry_available)
    }

    /// Processes messages until `Shutdown` or until every sender is gone.
    pub fn run(mut self, inbox: Receiver<Message>) {
        loop {
            let message = match self.next_wake() {
                Some(at) => inbox.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match message {
                Ok(Message::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(message) => self.handle(message),
                Err(RecvTimeoutError::Timeout) => self.tick(Instant::now()),
            }
        }
        self.shutdown();
    }

    pub fn handle(&mut self, message: Message) {
        match message {
            Message::HotkeyPressed(
                trigger @ (HotkeyAction::Main | HotkeyAction::ByApp | HotkeyAction::Dictate { .. }),
            ) => self.dictation_hotkey(trigger, true),
            Message::HotkeyReleased(
                trigger @ (HotkeyAction::Main | HotkeyAction::ByApp | HotkeyAction::Dictate { .. }),
            ) => self.dictation_hotkey(trigger, false),
            Message::HotkeyPressed(HotkeyAction::Rewrite { mode_id }) => {
                self.rewrite_selection(&mode_id)
            }
            Message::HotkeyReleased(HotkeyAction::Rewrite { .. }) => {}
            Message::Processed { session, result } => {
                let input = match result {
                    Ok(text) => SessionInput::ProcessingDone { text },
                    Err(error) => match error.kind {
                        ProcessingFailure::PlanLimit => SessionInput::PlanLimitReached,
                        // The session tells an empty answer apart by itself.
                        ProcessingFailure::Empty => {
                            SessionInput::ProcessingDone { text: "".into() }
                        }
                        ProcessingFailure::Failed => {
                            SessionInput::ProcessingFailed { detail: error.detail }
                        }
                    },
                };
                self.session_input(session, input);
            }
            Message::Rewritten { request, result } => {
                if self.rewriting == Some(request) {
                    self.rewriting = None;
                }
                let event = match result {
                    Ok(()) => DictationEvent::SelectionDone,
                    Err(error) => DictationEvent::SelectionFailed {
                        problem: selection_problem(&error),
                        detail: error.to_string(),
                    },
                };
                (self.emit)(CoreEvent::Dictation(event));
            }
            Message::ContextRead { id, found } => self.context_read(id, found),
            Message::Settings(settings) => self.apply_settings(*settings),
            Message::EngineReady => self.engine_input(EngineInput::Loaded),
            Message::EngineFailed(detail) => {
                self.engine_lost(EngineInput::LoadFailed { detail: detail.clone() }, &detail);
            }
            Message::EngineCrashed(detail) => {
                self.engine_lost(EngineInput::Crashed { detail: detail.clone() }, &detail);
            }
            Message::EngineStopped => self.engine_input(EngineInput::Stopped),
            Message::Transcribed { session, result } => {
                self.schedule(SchedulerInput::JobFinished);
                let input = match result {
                    Ok(transcript) => {
                        let language = transcript
                            .language
                            .as_deref()
                            .and_then(languages::code_for)
                            .map(String::from)
                            .or(transcript.language);
                        // Credits Whisper invents on silence count as no speech; other engines
                        // do not invent them, and such a phrase is what was said.
                        let invented = self.whisper_started
                            && hallucinations::is_hallucination(&transcript.text);
                        let text = if invented {
                            String::new()
                        } else {
                            self.transcript_text(session, &transcript.text, language.as_deref())
                        };
                        if !text.trim().is_empty() {
                            self.new_text_from(session);
                        }
                        SessionInput::TranscriptReady { language, text }
                    }
                    Err(detail) => SessionInput::TranscriptionFailed { detail },
                };
                self.session_input(session, input);
            }
            Message::Unrecognized { session, audio } => {
                if let Some(active) = self.sessions.get_mut(&session) {
                    active.returned = Some(audio);
                }
            }
            Message::LiveText { session, text } => {
                let rules = self.live_rules(session);
                let rules = rules.as_deref().unwrap_or(&self.settings.replacements);
                let latest = Replacer::new(rules).settled(&tidy(&text));
                if let Some(live) = self.live(session) {
                    live.latest = latest;
                }
                self.type_live(session);
            }
            Message::LiveTyped { session, result } => {
                let Some(live) = self.live(session) else { return };
                let piece = live.in_flight.take();
                match result {
                    Ok(()) => live.typed.push_str(&piece.unwrap_or_default()),
                    // Focus moved or typing is blocked: the rest goes in with the final insertion.
                    Err(error) => {
                        // What went in before focus moved is typed, so it does not go in twice.
                        if let (InsertError::Interrupted { typed }, Some(piece)) = (&error, &piece)
                        {
                            live.typed.push_str(piece.get(..*typed).unwrap_or_default());
                        }
                        live.typing = false;
                        log_line!("live typing stopped: {error}");
                    }
                }
                if std::mem::take(&mut live.insert_waiting) {
                    self.begin_insert(session);
                } else {
                    self.type_live(session);
                }
            }
            Message::Inserted { session, result } => {
                let input = match result {
                    Ok(()) => SessionInput::InsertionDone,
                    Err(error) => SessionInput::InsertionFailed {
                        problem: match error {
                            InsertError::NoTarget => InsertionProblem::NoFocus,
                            InsertError::FocusChanged | InsertError::Interrupted { .. } => {
                                InsertionProblem::FocusChanged
                            }
                            InsertError::Blocked => InsertionProblem::Blocked,
                            InsertError::NoPermission => InsertionProblem::NoPermission,
                            InsertError::Unsupported | InsertError::Failed(_) => {
                                InsertionProblem::Other
                            }
                        },
                        detail: error.to_string(),
                    },
                };
                self.session_input(session, input);
            }
            Message::StartFile(request) => self.start_file(*request),
            Message::CancelFile => self.cancel_file(false),
            Message::CancelTrack => self.cancel_file(true),
            Message::FileWindow { file, window, audio, duration } => {
                let Some(job) = self.file_job(file) else { return };
                job.duration = duration;
                job.ended = window.last;
                job.waiting = Some((window, audio));
                self.schedule(SchedulerInput::EnqueueFileChunk { file, chunk: window.index });
            }
            Message::FileEnded { file } => {
                let Some(job) = self.file_job(file) else { return };
                job.ended = true;
                self.finish_file_if_done();
            }
            Message::FileReadFailed { file, detail } => {
                if self.file_job(file).is_some() {
                    self.fail_file(FileProblem::Read, detail);
                }
            }
            // The answer is taken before the engine slot is freed for the next job.
            Message::ChunkTranscribed { file, chunk, result } => {
                if self.ended_chunk == Some(file) {
                    self.ended_chunk = None;
                    self.engine_input(EngineInput::Release);
                } else {
                    self.chunk_transcribed(file, chunk, result);
                }
                self.schedule(SchedulerInput::JobFinished);
            }
            Message::FileDiarizing { file, share } => {
                let Some(job) = self.file_job(file) else { return };
                let total_ms = job.total_ms();
                let done_ms =
                    total_ms.map_or(0, |total| (share.clamp(0.0, 1.0) * total as f64) as u64);
                (self.emit)(CoreEvent::File(FileEvent::Speakers {
                    file: file.0,
                    step: SpeakersStep::Voices,
                    done_ms,
                    total_ms,
                }));
            }
            Message::FileDiarized { file, result } => {
                if self.file_job(file).is_some() {
                    let segments = result.unwrap_or_else(|detail| {
                        log_line!("file {}: speakers: {detail}", file.0);
                        Vec::new()
                    });
                    self.finish_words(&segments);
                }
            }
            Message::FileProcessed { file, result } => {
                if self.file_job(file).is_some() {
                    self.file_processed(result);
                }
            }
            Message::RetryDictation => self.retry_dictation(),
            Message::TakeHeld { reply } => {
                let _ = reply.send(self.held_result.clone());
            }
            Message::Shutdown => self.shutdown(),
        }
    }

    /// When the loop must wake up without a message: level updates, the idle timer.
    pub fn next_wake(&self) -> Option<Instant> {
        // The level while recording; the output format while waiting for a headset.
        let poll = (self.recording.is_some() || self.restoring.is_some())
            .then(|| Instant::now() + LEVEL_INTERVAL);
        let idle = self.idle_timer.map(|(at, _)| at);
        let restoring = self.restoring.as_ref().map(Restoring::deadline);
        poll.into_iter().chain(idle).chain(restoring).min()
    }

    pub fn tick(&mut self, now: Instant) {
        if self.restoring.as_ref().is_some_and(|restoring| restoring.back_at.is_none())
            && self.output_settled()
            && let Some(restoring) = &mut self.restoring
        {
            restoring.back_at = Some(now);
        }
        if self.restoring.as_ref().is_some_and(|restoring| now >= restoring.deadline()) {
            self.restore_sound();
        }
        if let Some((id, recording)) = &self.recording {
            let permille = (recording.level() * 1000.0).clamp(0.0, 1000.0) as u16;
            (self.emit)(CoreEvent::Dictation(DictationEvent::Level { permille }));
            if let Some(live) = self.sessions.get_mut(id).and_then(|active| active.live.as_mut())
                && live.opened
                && now.saturating_duration_since(live.last_feed) >= FEED_INTERVAL
            {
                let samples = recording.samples_since(live.fed);
                live.fed += samples.len();
                live.last_feed = now;
                if !samples.is_empty() {
                    self.services.feed_live(*id, samples);
                }
            }
        }
        if let Some((at, token)) = self.idle_timer
            && now >= at
        {
            self.idle_timer = None;
            self.engine_input(EngineInput::IdleTimeout { token });
        }
    }

    fn gesture(&mut self, gesture: Gesture, trigger: HotkeyAction) {
        match gesture {
            Gesture::Start => self.start_dictation(trigger),
            Gesture::Stop => self.finish_recording(),
            Gesture::Latch => self.hands_free(),
        }
    }

    /// The hotkey of the dictation being recorded.
    fn recording_trigger(&self) -> Option<HotkeyAction> {
        let (id, _) = self.recording.as_ref()?;
        self.sessions.get(id).and_then(|active| active.trigger.clone())
    }

    /// The mode a hotkey dictates with: a mode's own, the main one for the main hotkey, and for
    /// the "by application" hotkey the one the user chose for the program in focus, else the
    /// main one. Also notes the program as a recent one.
    fn chosen_mode(
        &mut self,
        trigger: &HotkeyAction,
        target: Option<&FocusTarget>,
    ) -> (String, bool, Option<AppId>) {
        let app = target.and_then(|target| self.services.app_identity(target));
        if let Some(app) = &app {
            self.recent_apps.lock().expect("recent apps lock").add(app.clone());
        }
        let main = self.settings.main_mode_id.clone();
        let (mode_id, by_app) = match trigger {
            HotkeyAction::Dictate { mode_id } => (mode_id.clone(), false),
            HotkeyAction::Main | HotkeyAction::Rewrite { .. } => (main, false),
            HotkeyAction::ByApp => {
                let ruled = app
                    .as_ref()
                    .and_then(|app| app_rules::mode_for(&self.settings.app_rules, app))
                    .map(String::from)
                    .filter(|id| self.settings.modes.iter().any(|mode| mode.id == *id));
                match ruled {
                    Some(id) if id != main => (id, true),
                    _ => (main, false),
                }
            }
        };
        (mode_id, by_app, app)
    }

    /// Starts reading what the mode lets its language model see; nothing for other modes.
    fn pending_context(
        &mut self,
        mode_id: &str,
        target: Option<FocusTarget>,
        app: Option<AppId>,
    ) -> PendingContext {
        let ModeAction::Process { context: sources, .. } = self.mode(mode_id).action else {
            return PendingContext::none();
        };
        let id = self.next_request;
        self.next_request += 1;
        let read = sources.reads().then(|| self.services.read_context(id, target, sources));
        let app = app.filter(|_| sources.app).map(|app| app.name);
        PendingContext { id, app, read }
    }

    /// The capsule shows that the dictation takes context once some source had text.
    fn context_read(&mut self, id: u64, found: bool) {
        if !found || id == 0 {
            return;
        }
        let session = self.sessions.iter().find_map(|(session, active)| {
            active.context.as_ref().is_some_and(|context| context.id == id).then_some(*session)
        });
        if let Some(session) = session {
            (self.emit)(CoreEvent::Dictation(DictationEvent::ContextTaken { session }));
        }
    }

    fn mode(&self, mode_id: &str) -> Mode {
        let find = |id: &str| self.settings.modes.iter().find(|mode| mode.id == id).cloned();
        find(mode_id).or_else(|| find(modes::PLAIN)).unwrap_or_else(modes::plain_mode)
    }

    /// The language-model work of a mode, if it has a connection that still exists: its own, or
    /// for plain dictation the clean-up when "clean text" uses a model.
    fn llm_job(&self, mode: &Mode) -> Option<LlmJob> {
        let (connection_id, model, instruction, output_language) =
            match (&mode.action, &self.settings.clean_text) {
                (
                    ModeAction::Process {
                        connection_id, model, instruction, output_language, ..
                    },
                    _,
                ) => (connection_id, model, instruction.clone(), output_language.clone()),
                (ModeAction::Plain, CleanText::Model { connection_id, model }) => {
                    (connection_id, model, clean_text::INSTRUCTION.to_string(), None)
                }
                _ => return None,
            };
        let connection =
            self.settings.connections.iter().find(|connection| connection.id == *connection_id)?;
        Some(LlmJob {
            connection: connection.clone(),
            model: model.clone(),
            instruction,
            output_language,
        })
    }

    /// Whether the dictation is plain and "clean text" is on.
    fn cleans(&self, session: SessionId) -> bool {
        self.settings.clean_text != CleanText::Off
            && self
                .sessions
                .get(&session)
                .is_some_and(|active| active.mode.action == ModeAction::Plain)
    }

    /// The user's replacements and the fillers in one list, for a dictation typed while it is
    /// spoken: its typed text and its final text must agree, so both remove the fillers the
    /// same way. `None` when nothing is cleaned.
    fn live_rules(&self, session: SessionId) -> Option<Vec<Replacement>> {
        if !self.cleans(session) {
            return None;
        }
        let english = self.settings.spoken_language.as_deref() == Some("en");
        let mut rules = self.settings.replacements.clone();
        rules.extend(clean_text::filler_rules(&self.settings.fillers, english));
        Some(rules)
    }

    /// The recognized text with the fillers removed when "clean text" is on, and the user's
    /// replacements applied.
    fn transcript_text(&self, session: SessionId, text: &str, language: Option<&str>) -> String {
        // Text typed while speaking was cleaned piece by piece: the rest must continue it.
        let typed = self
            .sessions
            .get(&session)
            .and_then(|active| active.live.as_ref())
            .is_some_and(|live| !live.typed.is_empty() || live.in_flight.is_some());
        if typed && let Some(rules) = self.live_rules(session) {
            return Replacer::new(&rules).apply(text);
        }
        let replacer = Replacer::new(&self.settings.replacements);
        if self.cleans(session) {
            let english = language.or(self.settings.spoken_language.as_deref()) == Some("en");
            return replacer.apply(&clean_text::remove_fillers(
                text,
                &self.settings.fillers,
                english,
            ));
        }
        replacer.apply(text)
    }

    /// A dictation hotkey went down or up. A recording started by one hotkey is stopped only by
    /// the same hotkey (`apply_settings` ends one whose hotkey is gone).
    fn dictation_hotkey(&mut self, trigger: HotkeyAction, pressed: bool) {
        if pressed {
            self.held.insert(trigger.clone());
        } else {
            self.held.remove(&trigger);
        }
        self.publish_held();
        if self.recording_trigger().is_some_and(|current| current != trigger) {
            return;
        }
        let now = Instant::now();
        let gesture = if pressed { self.gesture.pressed(now) } else { self.gesture.released(now) };
        if let Some(gesture) = gesture {
            self.gesture(gesture, trigger);
        }
    }

    fn rewrite_selection(&mut self, mode_id: &str) {
        if self.rewriting.is_some() {
            return;
        }
        let Some(job) = self.llm_job(&self.mode(mode_id)) else {
            let detail = "the mode has no language model";
            return self.refuse_selection(SelectionProblem::Processing, detail);
        };
        let Some(target) = self.services.focus_target() else {
            return self.refuse_selection(SelectionProblem::NoFocus, "");
        };
        let request = self.next_request;
        self.next_request += 1;
        self.rewriting = Some(request);
        (self.emit)(CoreEvent::Dictation(DictationEvent::SelectionStarted {
            mode_id: mode_id.to_string(),
        }));
        self.services.rewrite_selection(request, target, job);
    }

    /// The selected text is not rewritten, and the user is told why.
    fn refuse_selection(&mut self, problem: SelectionProblem, detail: &str) {
        let event = DictationEvent::SelectionFailed { problem, detail: detail.to_string() };
        (self.emit)(CoreEvent::Dictation(event));
    }

    fn block(&mut self, reason: BlockReason, detail: String) {
        self.gesture.reset();
        (self.emit)(CoreEvent::Dictation(DictationEvent::Blocked { reason, detail }));
    }

    fn start_dictation(&mut self, trigger: HotkeyAction) {
        let engine = match self.usable_engine() {
            Ok(engine) => engine,
            Err(reason) => return self.block(reason, String::new()),
        };
        let target = self.services.focus_target();
        let (mode_id, by_app, app) = self.chosen_mode(&trigger, target.as_ref());
        let context = self.pending_context(&mode_id, target, app);
        // The last dictation still waits for its headset, which is still in its headset mode:
        // the microphone opens without a switch, and muted sound stays muted. The format from
        // before the last dictation is kept to wait for.
        self.restoring = None;
        let opening = Opening {
            id: self.take_session_id(),
            mode_id,
            trigger,
            by_app,
            // The program's name is context taken already; the rest once it has been read.
            context_taken: context.app.is_some(),
            context,
            target,
            engine,
        };
        self.open_microphone(opening);
    }

    fn open_microphone(&mut self, opening: Opening) {
        let Opening { id, mode_id, trigger, by_app, context, context_taken, target, engine } =
            opening;
        let mode_id = mode_id.as_str();
        // Silence before the microphone opens, so a headset switching modes is not heard.
        if self.output_before.is_none() {
            self.output_before = self.services.output_format();
        }
        if self.settings.mute_while_recording {
            self.services.mute();
        }
        let recording = match self.services.start_recording(self.settings.microphone_id.clone()) {
            Ok(recording) => recording,
            Err(detail) => {
                self.output_before = None;
                if self.settings.mute_while_recording {
                    self.services.unmute();
                }
                return self.block(BlockReason::Microphone, detail);
            }
        };
        let (mode, session, events) = self.new_session(id, mode_id, true);
        let streams = engine.streams();
        // Typing while a hotkey is held is safe unless it keeps Ctrl, Alt or Win down.
        let free_hands = self.gesture.hands_free()
            || !self.trigger_hotkey(&trigger, &mode).is_some_and(hotkeys::holds_shortcut_modifier);
        let may_type =
            mode.action == ModeAction::Plain && !session.post_processes() && target.is_some();
        let live = streams.then(|| Live {
            opened: false,
            fed: 0,
            last_feed: Instant::now(),
            typing: free_hands && may_type,
            typing_after_tap: !free_hands && may_type && self.gesture.mode() == RecordingMode::Auto,
            typed: String::new(),
            in_flight: None,
            latest: String::new(),
            insert_waiting: false,
            finished: false,
        });
        self.sessions.insert(
            id,
            Active {
                target,
                trigger: Some(trigger),
                context: Some(context),
                live,
                ..Active::new(session, mode, id)
            },
        );
        self.recording = Some((id, recording));
        for event in events {
            (self.emit)(CoreEvent::Session(event));
        }
        if by_app {
            (self.emit)(CoreEvent::Dictation(DictationEvent::ModeByApp {
                session: id,
                mode_id: mode_id.to_string(),
            }));
        }
        if context_taken {
            (self.emit)(CoreEvent::Dictation(DictationEvent::ContextTaken { session: id }));
        }
        self.schedule(SchedulerInput::RecordingStarted);
        self.engine_input(EngineInput::Acquire);
        // At once if the engine is ready, else once it is; it waits while another dictation's
        // stream is open.
        self.open_waiting_stream();
    }

    /// Opens the stream of the recording that has none yet, only at a ready streaming engine
    /// that runs the configuration chosen: one still loading may be stopped for another model
    /// before it is ready (its text would be typed, then not continued by the new one), and an
    /// engine that does not stream must never be kept for a stream (`EngineFinishing`). The
    /// engine has one stream at a time: it waits while another dictation's stream is open and
    /// not finished.
    fn open_waiting_stream(&mut self) {
        if self.engine.state() != EngineState::Ready
            || self.engine.restarting()
            || !self.streaming_started
        {
            return;
        }
        let Some((id, recording)) = &self.recording else { return };
        let (id, rate) = (*id, recording.rate());
        // Another dictation's stream; this one's own needs nothing either.
        let busy = self.open_stream().is_some();
        let Some(live) = self.live(id) else { return };
        if busy || live.opened {
            return;
        }
        live.opened = true;
        self.services.start_live(id, self.settings.spoken_language.clone(), rate);
    }

    fn finish_recording(&mut self) {
        let Some((id, recording)) = self.recording.take() else { return };
        let interrupted = recording.interrupted();
        let stopped = recording.stop();
        if stopped.is_ok()
            && let Some(detail) = interrupted
        {
            let event = DictationEvent::MicrophoneLost { session: id, detail };
            (self.emit)(CoreEvent::Dictation(event));
        }
        // The microphone is closed. A headset is let come back from its headset mode before the
        // sound returns, or the music would play muffled and then break off for the switch.
        let settled = self.output_settled();
        let until = Instant::now() + RESTORE_LIMIT;
        self.restoring = Some(Restoring { until, back_at: None });
        self.recording_stopped(id, stopped);
        if settled {
            self.restore_sound();
        }
        // Only after the dictation is queued, so a waiting file chunk does not go first.
        self.schedule(SchedulerInput::RecordingEnded);
    }

    /// Whether the output is back in the format it had before the microphone opened.
    fn output_settled(&self) -> bool {
        self.output_before
            .as_ref()
            .is_none_or(|before| output::settled(before, self.services.output_format().as_ref()))
    }

    /// The sound comes back.
    fn restore_sound(&mut self) {
        self.restoring = None;
        self.output_before = None;
        self.services.unmute();
    }

    fn recording_stopped(&mut self, id: SessionId, stopped: Result<Captured, String>) {
        let audio = match stopped {
            Ok(audio) => audio,
            Err(detail) => return self.session_input(id, SessionInput::RecordingFailed { detail }),
        };
        let retry = self.sessions.get(&id).is_some_and(|active| !active.session.inserts());
        // A recording recognized again passed the threshold of speech the first time.
        let speech = if retry {
            audio.duration()
        } else {
            speech::speech_duration(&audio.samples, audio.rate)
        };
        if let Some(active) = self.sessions.get_mut(&id) {
            active.recorded = audio.duration();
        }
        self.session_input(
            id,
            SessionInput::RecordingStopped { duration: audio.duration(), speech },
        );
        let waiting = self
            .sessions
            .get(&id)
            .is_some_and(|active| *active.session.state() == SessionState::Transcribing);
        if !waiting {
            return;
        }
        if let Some(active) = self.sessions.get_mut(&id) {
            active.audio = Some(audio);
        }
        if let Some(detail) = self.engine_error.clone() {
            return self.session_input(id, SessionInput::TranscriptionFailed { detail });
        }
        self.schedule(if retry {
            SchedulerInput::EnqueueRetry(id)
        } else {
            SchedulerInput::EnqueueDictation(id)
        });
    }

    fn session_input(&mut self, id: SessionId, input: SessionInput) {
        let Some(active) = self.sessions.get_mut(&id) else { return };
        let events = match active.session.handle(input) {
            Ok(events) => events,
            Err(error) => return log_line!("{error}"),
        };
        let state = active.session.state().clone();
        let recorded = active.recorded;
        let release = active.holds_engine
            && !matches!(state, SessionState::Recording | SessionState::Transcribing);
        if release {
            active.holds_engine = false;
        }
        for event in events {
            let finished = match &event {
                SessionEvent::Finished { result, .. } => Some(result.clone()),
                _ => None,
            };
            if let Some(result) = &finished {
                *self.last_result.lock().expect("last result lock") = Some(result.clone());
            }
            (self.emit)(CoreEvent::Session(event));
            let Some(result) = finished else { continue };
            self.count(&result, recorded);
            if self.settings.history_enabled {
                let event = match self.services.save_history(&result, self.settings.history_limit) {
                    Ok(()) => DictationEvent::HistorySaved,
                    Err(detail) => DictationEvent::HistoryFailed { detail },
                };
                (self.emit)(CoreEvent::Dictation(event));
            }
        }
        if release {
            self.engine_input(EngineInput::Release);
        }
        if state == SessionState::PostProcessing {
            self.begin_processing(id);
        } else if state == SessionState::Inserting {
            self.begin_insert(id);
        } else if state.is_finished() {
            let Some(mut removed) = self.sessions.remove(&id) else { return };
            if removed.live.as_ref().is_some_and(|live| !live.finished) {
                self.services.cancel_live(id);
                self.open_waiting_stream();
                // An engine kept only for this stream stops now.
                self.schedule(SchedulerInput::CancelDictation(id));
            }
            // Not recognized (an engine or service error): the recording stays for a retry.
            if let SessionState::Failed { failure } = &state
                && failure.kind == FailureKind::Transcription
                && self.failed.as_ref().is_none_or(|failed| failed.session.0 < removed.spoken.0)
                && let Some(audio) = removed.returned.take().or(removed.audio.take())
            {
                // The engine gives it back at 16 kHz; one that never reached the engine (it would
                // not load) keeps the microphone's rate, and the engine's thread converts it.
                let mode_id = removed.mode.id.clone();
                // Numbers are taken at the press: a dictation recording now was pressed before
                // this failure.
                let since = SessionId(self.next_session);
                let session = removed.spoken;
                self.failed = Some(FailedDictation { session, since, audio, mode_id });
                self.set_retry_available(true);
            }
            self.schedule(SchedulerInput::CancelDictation(id));
            // Its text is in (or never will be): the next dictation's goes on.
            self.resume_typing();
        }
    }

    /// Text goes in in the order the dictations were made: a later one types nothing, live or
    /// at the end, while an earlier one still has text to put in, so two never mix in a field.
    fn typing_waits(&self, id: SessionId) -> bool {
        self.sessions.iter().any(|(other, active)| {
            other.0 < id.0
                && active.session.inserts()
                && active.target.is_some()
                && !active.session.state().is_finished()
        })
    }

    /// An earlier dictation is done: the next ones type what waited, oldest first.
    fn resume_typing(&mut self) {
        let mut ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        for id in ids {
            if self.typing_waits(id) {
                break;
            }
            let queued =
                self.sessions.get_mut(&id).map(|active| std::mem::take(&mut active.insert_queued));
            if queued == Some(true) {
                self.begin_insert(id);
            } else {
                self.type_live(id);
            }
        }
    }

    /// A new dictation, started after the failure, gave text: the recording of the older failed
    /// one is let go. One without speech (a touch of the hotkey, credits Whisper invents) leaves
    /// it, and so does one pressed before it failed (still recording then).
    fn new_text_from(&mut self, session: SessionId) {
        let new = self.sessions.get(&session).is_some_and(|active| active.session.inserts());
        if new && self.failed.as_ref().is_some_and(|failed| session.0 >= failed.since.0) {
            self.failed = None;
            self.set_retry_available(false);
        }
    }

    fn set_retry_available(&mut self, available: bool) {
        self.retry_available.store(available, Ordering::Relaxed);
        (self.emit)(CoreEvent::Dictation(DictationEvent::RetryAvailable { available }));
    }

    /// Recognizes the kept recording again with the engine chosen now. The text is not typed:
    /// the window it was meant for is long gone. It goes to the history and the last result.
    fn retry_dictation(&mut self) {
        // A late second press after the retry started: nothing to say.
        if self.failed.is_none() {
            return;
        }
        // Not `block`: a dictation may be recording, and its hotkey gesture goes on.
        if let Err(reason) = self.usable_engine() {
            let event = DictationEvent::Blocked { reason, detail: String::new() };
            return (self.emit)(CoreEvent::Dictation(event));
        }
        let Some(FailedDictation { session: spoken, audio, mode_id, .. }) = self.failed.take()
        else {
            return;
        };
        self.set_retry_available(false);
        // Its start as a recording is not announced: nothing is recorded again.
        let id = self.take_session_id();
        let (mode, session, _) = self.new_session(id, &mode_id, false);
        self.sessions.insert(
            id,
            Active { context: Some(PendingContext::none()), ..Active::new(session, mode, spoken) },
        );
        self.engine_input(EngineInput::Acquire);
        self.recording_stopped(id, Ok(audio));
    }

    /// The number of a new dictation, in the order they were asked for.
    fn take_session_id(&mut self) -> SessionId {
        let id = SessionId(self.next_session);
        self.next_session += 1;
        id
    }

    /// The dictation session `id` in `mode_id`: its mode, the session and its first events.
    /// `insert` is off for a recording recognized again.
    fn new_session(
        &mut self,
        id: SessionId,
        mode_id: &str,
        insert: bool,
    ) -> (Mode, DictationSession, Vec<SessionEvent>) {
        let mode = self.mode(mode_id);
        let post_process = mode.processes() || self.llm_job(&mode).is_some();
        let session_mode = SessionMode { id: mode.id.clone(), post_process, insert };
        let (session, events) = DictationSession::start(id, session_mode);
        (mode, session, events)
    }

    /// The combination held for a dictation: the main hotkey, or the mode's own.
    fn trigger_hotkey<'a>(&'a self, trigger: &HotkeyAction, mode: &'a Mode) -> Option<&'a str> {
        match trigger {
            HotkeyAction::Main => Some(self.settings.hotkey.as_str()),
            HotkeyAction::ByApp => self.settings.app_hotkey.as_deref(),
            _ => mode.hotkey.as_deref(),
        }
    }

    /// An inserted dictation with words counts in the statistics, whatever the history setting.
    fn count(&mut self, result: &DictationResult, recorded: Duration) {
        let words = stats::count_words(result.text());
        if !result.inserted || words == 0 {
            return;
        }
        match self.services.count_dictation(words, recorded) {
            Ok(()) => (self.emit)(CoreEvent::Dictation(DictationEvent::StatsChanged)),
            Err(detail) => log_line!("cannot count the dictation: {detail}"),
        }
    }

    /// Auto mode, a tap: the hotkey is up and the recording goes on, so live text may be typed.
    fn hands_free(&mut self) {
        let Some(id) = self.recording.as_ref().map(|(id, _)| *id) else { return };
        let Some(live) = self.live(id) else { return };
        if std::mem::take(&mut live.typing_after_tap) {
            live.typing = true;
            self.type_live(id);
        }
    }

    fn live(&mut self, id: SessionId) -> Option<&mut Live> {
        self.sessions.get_mut(&id).and_then(|active| active.live.as_mut())
    }

    /// Types the next piece of the live text while the dictation is recorded or transcribed.
    fn type_live(&mut self, id: SessionId) {
        if self.typing_waits(id) {
            return;
        }
        let Some(active) = self.sessions.get_mut(&id) else { return };
        let speaking =
            matches!(active.session.state(), SessionState::Recording | SessionState::Transcribing);
        let target = active.target;
        let Some(live) = active.live.as_mut() else { return };
        if !speaking || !live.typing || live.in_flight.is_some() {
            return;
        }
        let Some(piece) = live.latest.strip_prefix(live.typed.as_str()) else {
            // The model changed text that is already typed: typing stops, and nothing more goes
            // in at the end either, so nothing is doubled; the history keeps the whole text.
            live.typing = false;
            return;
        };
        if piece.is_empty() {
            return;
        }
        let piece = piece.to_string();
        live.in_flight = Some(piece.clone());
        self.services.type_live(id, piece, target);
    }

    fn begin_processing(&mut self, id: SessionId) {
        let Some(active) = self.sessions.get_mut(&id) else { return };
        let text = active.session.text().unwrap_or_default().to_string();
        let context = active.context.take().unwrap_or_else(PendingContext::none);
        let mode = active.mode.clone();
        match self.llm_job(&mode) {
            Some(job) => self.services.process(id, text, job, context),
            // The connection was removed after the recording started: insert the transcript.
            None => self.session_input(
                id,
                SessionInput::ProcessingFailed {
                    detail: "the language model is not set up".into(),
                },
            ),
        }
    }

    fn begin_insert(&mut self, id: SessionId) {
        let waits = self.typing_waits(id);
        let Some(active) = self.sessions.get_mut(&id) else { return };
        if waits {
            active.insert_queued = true;
            return;
        }
        let mut text = active.session.text().unwrap_or_default().to_string();
        if let Some(live) = active.live.as_mut() {
            if live.in_flight.is_some() {
                live.insert_waiting = true;
                return;
            }
            if !live.typed.is_empty() {
                // Typed while speaking: only the rest goes in. Should the final text not continue
                // what was typed, nothing more is inserted rather than doubling the text.
                text = text.strip_prefix(live.typed.as_str()).unwrap_or_default().to_string();
                if text.is_empty() {
                    return self.session_input(id, SessionInput::InsertionDone);
                }
            }
        }
        let Some(active) = self.sessions.get(&id) else { return };
        // Text goes only into the window focused at the hotkey press.
        match active.target {
            Some(target) => self.services.begin_insert(id, text, Some(target)),
            None => self.session_input(
                id,
                SessionInput::InsertionFailed {
                    problem: InsertionProblem::NoFocus,
                    detail: "no focused window at the hotkey press".into(),
                },
            ),
        }
    }

    /// The engine failed to load or crashed: what waits for it fails too. Not when the message
    /// is stale (of a load that was being stopped): the engine starts again for what waits.
    /// A crash under other work than the request that crashed starts the engine again: what
    /// waits for it waits on.
    fn engine_lost(&mut self, input: EngineInput, detail: &str) {
        let before = self.engine.state();
        self.engine_input(input);
        let after = self.engine.state();
        if after != before && after != EngineState::Loading {
            self.fail_waiting(detail);
        }
    }

    /// Fails dictations whose audio waits for an engine that will not come.
    fn fail_waiting(&mut self, detail: &str) {
        let waiting: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, active)| active.audio.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            self.session_input(id, SessionInput::TranscriptionFailed { detail: detail.into() });
        }
        // A file still being transcribed waits for the same engine.
        if self.file.as_ref().is_some_and(|job| job.holds_engine) {
            self.fail_file(FileProblem::Transcription, detail.into());
        }
    }

    fn schedule(&mut self, input: SchedulerInput) {
        match self.scheduler.handle(input) {
            Ok(Some(job)) => self.run_job(job),
            Ok(None) => {}
            Err(error) => log_line!("{error}"),
        }
        // The old engine's last request and stream are done: it stops, and what waits goes to
        // the new one.
        if self.engine.restarting() && !self.scheduler.busy() && self.open_stream().is_none() {
            self.engine_input(EngineInput::Idle);
        }
    }

    /// The dictation whose stream is open at the engine and not finished: one at a time.
    fn open_stream(&self) -> Option<SessionId> {
        self.sessions
            .iter()
            .find(|(_, active)| active.live.as_ref().is_some_and(Live::streaming))
            .map(|(id, _)| *id)
    }

    fn run_job(&mut self, job: Job) {
        match job {
            Job::Dictation { session } => {
                let Some(active) = self.sessions.get_mut(&session) else {
                    return self.schedule(SchedulerInput::JobFinished);
                };
                let translate = active.mode.action == ModeAction::TranslateToEnglish;
                let options = transcribe_options(&self.settings, translate, false);
                match active.audio.take() {
                    Some(audio) => {
                        // A stream that never opened (the recording ended before a streaming
                        // engine was ready, or the engine does not stream) is transcribed as a
                        // whole.
                        match active.live.as_mut().filter(|live| live.opened) {
                            Some(live) => {
                                let rest = audio.samples.get(live.fed..).unwrap_or_default();
                                if !rest.is_empty() {
                                    self.services.feed_live(session, rest.to_vec());
                                }
                                live.fed = audio.samples.len();
                                live.finished = true;
                                self.services.finish_live(session, audio, options);
                                // The engine's stream is free for the one being recorded.
                                self.open_waiting_stream();
                            }
                            None => self.services.transcribe(session, audio, options),
                        }
                    }
                    None => self.schedule(SchedulerInput::JobFinished),
                }
            }
            Job::FileChunk { file, chunk } => {
                let translate = self
                    .file_job(file)
                    .is_some_and(|job| job.mode.action == ModeAction::TranslateToEnglish);
                let options = transcribe_options(&self.settings, translate, true);
                let Some(job) = self.file_job(file) else {
                    return self.schedule(SchedulerInput::JobFinished);
                };
                let Some((window, audio)) =
                    job.waiting.take_if(|(window, _)| window.index == chunk)
                else {
                    return self.schedule(SchedulerInput::JobFinished);
                };
                job.running = Some(window);
                self.services.transcribe_chunk(file, chunk, audio, options);
                // The reader prepares the next window while the engine works.
                if !window.last {
                    self.services.next_file_window(file);
                }
            }
        }
    }

    /// The chosen model if it is downloaded, or the chosen cloud engine if its service is
    /// connected.
    fn speech_engine(&self) -> Option<SpeechEngine> {
        let id = self.settings.model_id.as_deref()?;
        let Some(spec) = cloud::find(id) else {
            return self.services.ready_model(id).map(SpeechEngine::Model);
        };
        let connection = self
            .settings
            .connections
            .iter()
            .find(|connection| Service::of(connection) == Some(spec.service))?;
        Some(SpeechEngine::Cloud { spec, connection: connection.clone() })
    }

    /// The engine to recognize with, or why there is none.
    fn usable_engine(&self) -> Result<SpeechEngine, BlockReason> {
        let engine = self.speech_engine().ok_or(BlockReason::NoModel)?;
        if !self.engine_available(&engine) {
            return Err(BlockReason::NoEngine);
        }
        Ok(engine)
    }

    /// A cloud engine needs no program on this computer.
    fn engine_available(&self, engine: &SpeechEngine) -> bool {
        match engine {
            SpeechEngine::Model(model) => self.services.engine_available(model),
            SpeechEngine::Cloud { .. } => true,
        }
    }

    fn engine_input(&mut self, input: EngineInput) {
        let before = self.engine.state();
        let transition = match self.engine.handle(input) {
            Ok(transition) => transition,
            Err(error) => return log_line!("{error}"),
        };
        for event in transition.events {
            if let EngineEvent::Failed { detail } = &event {
                self.engine_error = Some(detail.clone());
            }
            (self.emit)(CoreEvent::Engine(event));
        }
        let after = self.engine.state();
        if before != after {
            if after == EngineState::Ready {
                self.engine_error = None;
                // A recording that began while the engine loaded, was being stopped or ran a model
                // no longer chosen opens its stream now, if this one streams.
                self.open_waiting_stream();
                self.schedule(SchedulerInput::EngineReady);
            } else if before == EngineState::Ready {
                self.schedule(SchedulerInput::EngineUnavailable);
            }
        }
        for effect in transition.effects {
            match effect {
                EngineEffect::StartEngine => match self.speech_engine() {
                    Some(engine) => {
                        // A new attempt: the error of the last one must not fail a dictation
                        // that stops before this load ends.
                        self.engine_error = None;
                        self.whisper_started = engine.is_whisper();
                        self.streaming_started = engine.streams();
                        // The engine's thread drops its stream when it starts an engine: one left
                        // open from before (an engine that crashed) opens again at this one if it
                        // streams, from the recording's start; else it is recognized whole.
                        for active in self.sessions.values_mut() {
                            if let Some(live) = active.live.as_mut().filter(|live| live.streaming())
                            {
                                live.opened = false;
                                live.fed = 0;
                            }
                        }
                        self.services.start_engine(engine);
                    }
                    None => {
                        let detail = "the selected model is not downloaded or its service is not \
                                      connected"
                            .to_string();
                        self.engine_input(EngineInput::LoadFailed { detail: detail.clone() });
                        self.fail_waiting(&detail);
                    }
                },
                EngineEffect::StopEngine => self.services.stop_engine(),
                EngineEffect::StartIdleTimer { token, after } => {
                    self.idle_timer = Some((Instant::now() + after, token));
                }
                EngineEffect::CancelIdleTimer { .. } => self.idle_timer = None,
            }
        }
        // Another engine was chosen: the running one takes no new request, only finishes the
        // one at it and a stream open at it (a dictation typed as it is spoken goes on where
        // it began; only local models stream), then stops.
        if self.engine.restarting() {
            self.schedule(match self.open_stream() {
                Some(session) => SchedulerInput::EngineFinishing(session),
                None => SchedulerInput::EngineUnavailable,
            });
        }
    }

    /// Only this thread changes the count.
    fn publish_held(&self) {
        HOTKEYS_HELD.store(self.held.len() as u32, Ordering::Relaxed);
    }

    fn apply_settings(&mut self, settings: Settings) {
        // Changed hotkeys were registered again, and a key held meanwhile may send no release:
        // the count starts over. A key still held is then not counted, which only shortens how
        // long typing waits for its modifiers.
        self.held.clear();
        self.publish_held();
        let engine_before = self.speech_engine();
        let old = std::mem::replace(&mut self.settings, settings);
        self.gesture.set_mode(self.settings.recording_mode);
        if old.unload_policy != self.settings.unload_policy {
            self.engine_input(EngineInput::PolicyChanged(self.settings.unload_policy));
        }
        // Another model, or the same cloud engine through another connection (another key): the
        // running engine must not go on with the old one.
        if old.model_id != self.settings.model_id || self.speech_engine() != engine_before {
            self.engine_input(EngineInput::Restart);
            let translated = crate::engines::translates(old.model_id.as_deref());
            self.stop_a_file_the_model_cannot_go_on_with(translated);
        }
        if old.mute_while_recording && !self.settings.mute_while_recording {
            self.services.unmute();
        }
        // Only its own hotkey stops a recording: one whose hotkey the settings no longer name (a
        // mode deleted, its hotkey cleared, the translation one with a model that does not
        // translate) ends now, as at that hotkey, so the microphone does not stay open.
        if self.recording_trigger().is_some_and(|trigger| !names_hotkey(&self.settings, &trigger)) {
            self.gesture.forget();
            self.finish_recording();
        }
    }

    fn shutdown(&mut self) {
        if let Some((_, recording)) = self.recording.take() {
            let _ = recording.stop();
        }
        self.services.shutdown();
    }
}

/// Whether `settings` name the hotkey of `trigger`: the main one always; another only in a valid
/// set (otherwise the main hotkey alone is registered).
fn names_hotkey(settings: &Settings, trigger: &HotkeyAction) -> bool {
    *trigger == HotkeyAction::Main
        || settings
            .hotkey_actions(crate::engines::translates(settings.model_id.as_deref()))
            .is_ok_and(|actions| actions.iter().any(|(_, action)| action == trigger))
}

fn selection_problem(error: &SelectionError) -> SelectionProblem {
    match error {
        SelectionError::NoFocus | SelectionError::Insert(InsertError::NoTarget) => {
            SelectionProblem::NoFocus
        }
        SelectionError::FocusChanged
        | SelectionError::Insert(InsertError::FocusChanged | InsertError::Interrupted { .. }) => {
            SelectionProblem::FocusChanged
        }
        SelectionError::Password => SelectionProblem::Password,
        SelectionError::NotSupported => SelectionProblem::NotSupported,
        SelectionError::NoSelection => SelectionProblem::NoSelection,
        SelectionError::SeveralRanges => SelectionProblem::SeveralRanges,
        SelectionError::ReadOnly => SelectionProblem::ReadOnly,
        SelectionError::Changed => SelectionProblem::Changed,
        SelectionError::Processing(_) => SelectionProblem::Processing,
        SelectionError::Insert(InsertError::Blocked) => SelectionProblem::InsertionBlocked,
        SelectionError::NoPermission | SelectionError::Insert(InsertError::NoPermission) => {
            SelectionProblem::NoPermission
        }
        SelectionError::Insert(_) => SelectionProblem::Insertion,
        SelectionError::Unsupported => SelectionProblem::Unsupported,
        SelectionError::Failed(_) => SelectionProblem::Failed,
    }
}

/// How the engine transcribes: the spoken language and the dictionary of the settings;
/// `words` asks for word times (files).
fn transcribe_options(settings: &Settings, translate: bool, words: bool) -> TranscribeOptions {
    TranscribeOptions {
        language: settings.spoken_language.clone(),
        translate,
        prompt: prompt_from_dictionary(&settings.dictionary),
        words,
    }
}

#[cfg(test)]
mod tests;
