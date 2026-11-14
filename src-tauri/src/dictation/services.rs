//! What the orchestrator needs from the outside world, and the real implementation.
//! Asynchronous operations report back as [`Message`]s.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::engine_worker::{self, EngineRequest};
use super::file_reader;
use super::{Message, ProcessingError, ProcessingFailure};
use crate::audio::capture::{Captured, Recorder};
use crate::audio::mute::MuteGuard;
use crate::audio::output::{self, OutputFormat};
use crate::audio::speech;
use crate::domain::app_rules::AppId;
use crate::domain::context::{self, Context, ContextSources, NO_CONTEXT};
use crate::domain::modes::{Connection, ProviderKind};
use crate::domain::scheduler::FileTaskId;
use crate::domain::session::{DictationResult, SessionId};
use crate::engines::chatgpt;
use crate::engines::cloud::{CloudApi, CloudSpec};
use crate::engines::llm::{self, LlmError};
use crate::engines::whisper_server::TranscribeOptions;
use crate::files::Word;
use crate::meetings::speakers::SpeakerModels;
use crate::meetings::store::{FileTranscript, SharedMeetings};
use crate::platform::{self, DefaultOutput, FocusTarget, InsertError, SelectionError};
use crate::runtimes::http;
use crate::storage::catalog;
use crate::storage::history::{NewEntry, Retention, SharedHistory};
use crate::storage::models::{ModelStore, ReadyModel};
use crate::storage::secrets::SecretStore;
use crate::storage::stats::SharedStats;
use crate::util::now_ms;

/// How long insertion waits for the user to release Ctrl, Alt, Shift or Win.
const MODIFIER_WAIT: Duration = Duration::from_secs(5);
/// How many dictation hotkeys are held (the controller's set of them): the insertion of the
/// dictation before waits for their release however long the next one is held.
pub static HOTKEYS_HELD: AtomicU32 = AtomicU32::new(0);

/// What a language-model mode asks for; the key is looked up by the connection id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmJob {
    pub connection: Connection,
    pub model: String,
    pub instruction: String,
    pub output_language: Option<String>,
}

/// How long the model waits for context still being read; a source that takes longer (a hung
/// program) is left out.
const CONTEXT_WAIT: Duration = Duration::from_secs(3);

/// The context of a dictation: read at the hotkey press, waited for when the model is called.
pub struct PendingContext {
    /// Names the `ContextRead` reply; 0 when nothing is read.
    pub id: u64,
    /// The program's name, known at the press.
    pub app: Option<String>,
    /// The selected text and the clipboard, read on another thread.
    pub read: Option<Receiver<Context>>,
}

impl PendingContext {
    pub fn none() -> Self {
        Self { id: 0, app: None, read: None }
    }

    pub fn wait(self) -> Context {
        let read = self.read.and_then(|read| read.recv_timeout(CONTEXT_WAIT).ok());
        Context { app: self.app, ..read.unwrap_or_default() }
    }
}

/// What recognizes speech: a downloaded model, or a cloud service the user chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpeechEngine {
    Model(ReadyModel),
    /// The key is looked up by the connection id when the engine starts.
    Cloud {
        spec: CloudSpec,
        connection: Connection,
    },
}

impl SpeechEngine {
    /// A streaming model (Nemotron): it takes a dictation while it is recorded. Only a model on
    /// this computer streams.
    pub fn streams(&self) -> bool {
        matches!(
            self,
            Self::Model(ReadyModel { sherpa: Some(spec), .. })
                if spec.recognizer == crate::storage::catalog::Recognizer::OnlineTransducer
        )
    }

    /// Whisper, on this computer or at a cloud service: it invents credits on silence.
    pub fn is_whisper(&self) -> bool {
        match self {
            Self::Model(model) => model.sherpa.is_none() && model.llama.is_none(),
            Self::Cloud { spec, .. } => spec.api == CloudApi::Whisper,
        }
    }
}

/// A window's answer without words, in the engines' form (`files::parse_words`).
const NO_WORDS: &str = r#"{"segments":[]}"#;

pub trait Recording: Send {
    /// Input level, 0.0–1.0.
    fn level(&self) -> f32;
    /// The device's sample rate.
    fn rate(&self) -> u32;
    /// Samples recorded from position `from` on, for a streaming model; recording goes on.
    fn samples_since(&self, from: usize) -> Vec<f32>;
    fn stop(self: Box<Self>) -> Result<Captured, String>;
    /// Why the device stopped delivering midway, if it did (a headset that went away).
    fn interrupted(&self) -> Option<String> {
        None
    }
}

pub trait Services: Send + 'static {
    fn ready_model(&self, model_id: &str) -> Option<ReadyModel>;
    /// Whether the model's engine is installed: whisper-server and llama-server are separate
    /// programs, sherpa-onnx is part of the app.
    fn engine_available(&self, model: &ReadyModel) -> bool;
    fn focus_target(&self) -> Option<FocusTarget>;
    /// The program of a focus target; `None` for this app and when the OS does not tell.
    fn app_identity(&self, target: &FocusTarget) -> Option<AppId>;
    fn start_recording(&mut self, microphone: Option<String>)
    -> Result<Box<dyn Recording>, String>;
    fn mute(&mut self);
    fn unmute(&mut self);
    /// The default output device and its format, to tell when a headset is back from its
    /// headset mode.
    fn output_format(&self) -> Option<OutputFormat>;
    /// Replies with `EngineReady` or `EngineFailed`.
    fn start_engine(&mut self, engine: SpeechEngine);
    /// Replies with `EngineStopped`.
    fn stop_engine(&mut self);
    /// Replies with `Transcribed` (and `EngineCrashed` first if the engine died); a failure
    /// gives the audio back first in `Unrecognized`, at 16 kHz.
    fn transcribe(&mut self, session: SessionId, audio: Captured, options: TranscribeOptions);
    /// A dictation starts with a streaming model: it gets the audio while recording.
    fn start_live(&mut self, session: SessionId, language: Option<String>, rate: u32);
    /// More recorded audio for the live stream; replies with `LiveText` when the text changed.
    fn feed_live(&mut self, session: SessionId, samples: Vec<f32>);
    /// The recording ended; replies with `Transcribed` (the whole audio is the fallback), as
    /// `transcribe` does on a failure.
    fn finish_live(&mut self, session: SessionId, audio: Captured, options: TranscribeOptions);
    /// The dictation ended without a transcript.
    fn cancel_live(&mut self, session: SessionId);
    /// Types a piece of the live text where the dictation goes; replies with `LiveTyped`.
    fn type_live(&mut self, session: SessionId, text: String, target: Option<FocusTarget>);
    /// Replies with `Inserted`.
    fn begin_insert(&mut self, session: SessionId, text: String, target: Option<FocusTarget>);
    /// Sends the transcript to a language model; replies with `Processed`.
    fn process(&mut self, session: SessionId, text: String, job: LlmJob, context: PendingContext);
    /// Starts reading the ticked sources of `target` (the selected text, the clipboard); the
    /// text comes on the returned channel, and `ContextRead { id }` tells whether there was any.
    fn read_context(
        &mut self,
        id: u64,
        target: Option<FocusTarget>,
        sources: ContextSources,
    ) -> Receiver<Context>;
    /// Reads the selected text in `target`, rewrites it with the language model and replaces
    /// it if nothing changed meanwhile; replies with `Rewritten`.
    fn rewrite_selection(&mut self, request: u64, target: FocusTarget, job: LlmJob);
    /// Adds a finished dictation to the history and trims it to `limit` entries.
    fn save_history(&mut self, result: &DictationResult, limit: Option<u32>) -> Result<(), String>;
    /// Counts an inserted dictation in the statistics: its words and recording time, no text.
    fn count_dictation(&mut self, words: u64, recording: Duration) -> Result<(), String>;
    /// Starts reading a media file; replies with its first `FileWindow` (or `FileEnded`,
    /// `FileReadFailed`).
    fn open_file(&mut self, file: FileTaskId, path: PathBuf);
    /// Asks the file's reader for its next window.
    fn next_file_window(&mut self, file: FileTaskId);
    fn close_file(&mut self, file: FileTaskId);
    /// Replies with `ChunkTranscribed` (and `EngineCrashed` first if the engine died).
    fn transcribe_chunk(
        &mut self,
        file: FileTaskId,
        chunk: u32,
        audio: Captured,
        options: TranscribeOptions,
    );
    /// Tells the voices of a media file apart, by sentence of its `words`; replies with
    /// `FileDiarized`.
    fn diarize_file(&mut self, file: FileTaskId, path: PathBuf, words: Vec<Word>);
    /// A file's whole text to the language model; replies with `FileProcessed`.
    fn process_file(&mut self, file: FileTaskId, text: String, job: LlmJob);
    /// Writes a transcript where the user chose to save it.
    fn write_file(&mut self, path: &Path, text: &str) -> Result<(), String>;
    /// Keeps a transcribed file in the list of transcripts; returns its entry.
    fn keep_transcript(&mut self, transcript: FileTranscript) -> Result<i64, String>;
    /// Stops the engine; waits for it a moment, not for a request it is running.
    fn shutdown(&mut self);
}

impl Recording for Recorder {
    fn level(&self) -> f32 {
        Recorder::level(self)
    }

    fn rate(&self) -> u32 {
        Recorder::rate(self)
    }

    fn samples_since(&self, from: usize) -> Vec<f32> {
        Recorder::samples_since(self, from)
    }

    fn stop(self: Box<Self>) -> Result<Captured, String> {
        Recorder::stop(*self).map_err(|error| error.to_string())
    }

    fn interrupted(&self) -> Option<String> {
        Recorder::interrupted(self)
    }
}

pub struct RealServices {
    models: ModelStore,
    executable: PathBuf,
    llama_executable: PathBuf,
    engine: Option<Sender<EngineRequest>>,
    worker: Option<JoinHandle<()>>,
    /// Set when the app quits: a model still loading is not waited for.
    quit: Arc<AtomicBool>,
    replies: Sender<Message>,
    mute: MuteGuard<DefaultOutput>,
    secrets: SecretStore,
    history: Arc<SharedHistory>,
    stats: Arc<SharedStats>,
    /// Meetings and transcribed files.
    transcripts: Arc<SharedMeetings>,
    /// Readers of open files by task.
    files: HashMap<FileTaskId, Sender<()>>,
    /// Set when the file whose voices are being told apart ends: stops that work.
    diarizing: Arc<AtomicBool>,
    /// A meeting is being recorded: muting would silence the call and its recording.
    meeting: Arc<AtomicBool>,
    speakers: SpeakerModels,
}

/// Whether the program that runs a model is on this computer: a sherpa-onnx model runs inside
/// ahem, a llama.cpp model needs `llama-server`, a Whisper model `whisper-server`.
pub fn engine_installed(
    sherpa: bool,
    llama: bool,
    whisper_server: &Path,
    llama_server: &Path,
) -> bool {
    match (sherpa, llama) {
        (true, _) => true,
        (false, true) => llama_server.is_file(),
        (false, false) => whisper_server.is_file(),
    }
}

pub struct Paths {
    /// whisper-server.
    pub executable: PathBuf,
    pub llama_executable: PathBuf,
    pub engine_log: PathBuf,
    /// In the app data folder; a marker left by a crash unmutes at the next start.
    pub mute_marker: PathBuf,
    pub secrets: PathBuf,
    /// Set while a meeting records.
    pub meeting: Arc<AtomicBool>,
    /// The speaker models, for files told apart by voice.
    pub speakers: SpeakerModels,
}

impl RealServices {
    pub fn new(
        models: ModelStore,
        paths: Paths,
        history: Arc<SharedHistory>,
        stats: Arc<SharedStats>,
        transcripts: Arc<SharedMeetings>,
        replies: Sender<Message>,
    ) -> Self {
        let mute = MuteGuard::new(DefaultOutput, paths.mute_marker);
        if let Err(error) = mute.recover() {
            log_line!("cannot restore sound muted before a crash: {error}");
        }
        let quit = Arc::new(AtomicBool::new(false));
        let (engine, worker) = engine_worker::spawn(
            paths.executable.clone(),
            paths.llama_executable.clone(),
            paths.engine_log,
            replies.clone(),
            Arc::clone(&quit),
        );
        Self {
            models,
            executable: paths.executable,
            llama_executable: paths.llama_executable,
            engine: Some(engine),
            worker: Some(worker),
            quit,
            replies,
            mute,
            secrets: SecretStore::new(paths.secrets),
            history,
            stats,
            transcripts,
            files: HashMap::new(),
            diarizing: Arc::new(AtomicBool::new(false)),
            meeting: paths.meeting,
            speakers: paths.speakers,
        }
    }

    fn send(&self, request: EngineRequest) {
        if let Some(engine) = &self.engine {
            let _ = engine.send(request);
        }
    }
}

impl Services for RealServices {
    fn ready_model(&self, model_id: &str) -> Option<ReadyModel> {
        catalog::find(model_id).and_then(|spec| self.models.ready(&spec))
    }

    fn engine_available(&self, model: &ReadyModel) -> bool {
        engine_installed(
            model.sherpa.is_some(),
            model.llama.is_some(),
            &self.executable,
            &self.llama_executable,
        )
    }

    fn focus_target(&self) -> Option<FocusTarget> {
        platform::focus_target()
    }

    fn app_identity(&self, target: &FocusTarget) -> Option<AppId> {
        platform::app_identity(target)
    }

    fn start_recording(
        &mut self,
        microphone: Option<String>,
    ) -> Result<Box<dyn Recording>, String> {
        Recorder::start(microphone)
            .map(|recorder| Box::new(recorder) as Box<dyn Recording>)
            .map_err(|e| e.to_string())
    }

    fn mute(&mut self) {
        if self.meeting.load(Ordering::Relaxed) {
            return;
        }
        if let Err(error) = self.mute.mute() {
            log_line!("cannot mute sound: {error}");
        }
    }

    fn unmute(&mut self) {
        if let Err(error) = self.mute.restore() {
            log_line!("cannot restore sound: {error}");
        }
    }

    fn output_format(&self) -> Option<OutputFormat> {
        output::current()
    }

    fn start_engine(&mut self, engine: SpeechEngine) {
        match engine {
            SpeechEngine::Model(model) => self.send(EngineRequest::Start { model }),
            SpeechEngine::Cloud { spec, connection } => self.send(EngineRequest::StartCloud {
                spec,
                connection_id: connection.id,
                secrets: self.secrets.clone(),
            }),
        }
    }

    fn stop_engine(&mut self) {
        self.send(EngineRequest::Stop);
    }

    fn transcribe(&mut self, session: SessionId, audio: Captured, options: TranscribeOptions) {
        self.send(EngineRequest::Transcribe { session, audio, options });
    }

    fn start_live(&mut self, session: SessionId, language: Option<String>, rate: u32) {
        self.send(EngineRequest::StreamStart { session, language, rate });
    }

    fn feed_live(&mut self, session: SessionId, samples: Vec<f32>) {
        self.send(EngineRequest::StreamFeed { session, samples });
    }

    fn finish_live(&mut self, session: SessionId, audio: Captured, options: TranscribeOptions) {
        self.send(EngineRequest::StreamFinish { session, audio, options });
    }

    fn cancel_live(&mut self, session: SessionId) {
        self.send(EngineRequest::StreamCancel { session });
    }

    fn type_live(&mut self, session: SessionId, text: String, target: Option<FocusTarget>) {
        let replies = self.replies.clone();
        thread::spawn(move || {
            let _typing = typing();
            // A held Shift (Shift+Z held to dictate) does not change Unicode input.
            let result = wait_until_released(
                platform::shortcut_modifiers_down,
                &HOTKEYS_HELD,
                MODIFIER_WAIT,
            )
            .and_then(|()| platform::insert_text(&text, target.as_ref()));
            let _ = replies.send(Message::LiveTyped { session, result });
        });
    }

    fn begin_insert(&mut self, session: SessionId, text: String, target: Option<FocusTarget>) {
        let replies = self.replies.clone();
        thread::spawn(move || {
            let _typing = typing();
            let result =
                wait_for_modifiers().and_then(|()| platform::insert_text(&text, target.as_ref()));
            let _ = replies.send(Message::Inserted { session, result });
        });
    }

    fn process(&mut self, session: SessionId, text: String, job: LlmJob, context: PendingContext) {
        let replies = self.replies.clone();
        let secrets = self.secrets.clone();
        thread::spawn(move || {
            let context = context.wait();
            // Which sources went and their sizes, never their texts.
            if !context.is_empty() {
                log_line!("language model context: {context:?}");
            }
            let result = run_llm(&job, &secrets, &text, &context);
            let _ = replies.send(Message::Processed { session, result });
        });
    }

    fn read_context(
        &mut self,
        id: u64,
        target: Option<FocusTarget>,
        sources: ContextSources,
    ) -> Receiver<Context> {
        let (send, read) = mpsc::channel();
        let replies = self.replies.clone();
        thread::spawn(move || {
            // The clipboard first: reading the selection may copy it through the clipboard.
            let clipboard = if sources.clipboard { platform::clipboard_text() } else { None };
            let selection = if sources.selection {
                target.and_then(|target| platform::selected_text(&target))
            } else {
                None
            };
            let context = Context {
                app: None,
                selection: selection.as_deref().and_then(context::clip),
                clipboard: clipboard.as_deref().and_then(context::clip),
            };
            let found = !context.is_empty();
            let _ = send.send(context);
            let _ = replies.send(Message::ContextRead { id, found });
        });
        read
    }

    fn rewrite_selection(&mut self, request: u64, target: FocusTarget, job: LlmJob) {
        let replies = self.replies.clone();
        let secrets = self.secrets.clone();
        thread::spawn(move || {
            // Taken once the answer is there, held until the new text is in: the model's wait
            // holds up no dictation.
            let mut typing_lock = None;
            let mut rewrite = |selected: &str| {
                let text = run_llm(&job, &secrets, selected, &NO_CONTEXT)
                    .map_err(|error| SelectionError::Processing(error.detail))?;
                typing_lock = Some(typing());
                wait_for_modifiers().map_err(SelectionError::Insert)?;
                Ok(text)
            };
            let result = platform::rewrite_selection(&target, &mut rewrite);
            drop(typing_lock);
            let _ = replies.send(Message::Rewritten { request, result });
        });
    }

    fn save_history(&mut self, result: &DictationResult, limit: Option<u32>) -> Result<(), String> {
        let entry = NewEntry {
            created_at_ms: now_ms(),
            mode_id: result.mode_id.clone(),
            language: result.language.clone(),
            transcript: result.transcript.clone(),
            processed: result.processed.clone(),
            inserted: result.inserted,
        };
        self.history
            .add(entry, Retention::from_limit(limit))
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn count_dictation(&mut self, words: u64, recording: Duration) -> Result<(), String> {
        self.stats.add(words, recording).map_err(|error| error.to_string())
    }

    fn open_file(&mut self, file: FileTaskId, path: PathBuf) {
        let reader = file_reader::spawn(file, path, self.replies.clone());
        self.files.insert(file, reader);
    }

    fn next_file_window(&mut self, file: FileTaskId) {
        if let Some(reader) = self.files.get(&file) {
            let _ = reader.send(());
        }
    }

    fn close_file(&mut self, file: FileTaskId) {
        // Dropping the sender stops the reader.
        self.files.remove(&file);
        // A cancelled file does not go on telling voices apart (a finished one has ended that).
        self.diarizing.store(true, Ordering::Relaxed);
    }

    fn transcribe_chunk(
        &mut self,
        file: FileTaskId,
        chunk: u32,
        audio: Captured,
        options: TranscribeOptions,
    ) {
        // Digital silence goes to no engine: Whisper invents words on it, and a cloud service
        // is paid for every second of it.
        if speech::digital_silence(&audio.samples) {
            let result = Ok(NO_WORDS.to_string());
            let _ = self.replies.send(Message::ChunkTranscribed { file, chunk, result });
            return;
        }
        self.send(EngineRequest::TranscribeChunk { file, chunk, audio, options });
    }

    fn diarize_file(&mut self, file: FileTaskId, path: PathBuf, words: Vec<Word>) {
        let replies = self.replies.clone();
        let speakers = self.speakers.clone();
        // One file at a time: a flag of its own, so an older one that ended stays stopped.
        self.diarizing = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.diarizing);
        thread::spawn(move || {
            let mut progress = |share: f64| {
                let _ = replies.send(Message::FileDiarizing { file, share });
            };
            // The worker reads the file itself: it is not decoded into this process too.
            let result = speakers.diarize(&path, &words, &cancel, &mut progress);
            let _ = replies.send(Message::FileDiarized { file, result });
        });
    }

    fn process_file(&mut self, file: FileTaskId, text: String, job: LlmJob) {
        let replies = self.replies.clone();
        let secrets = self.secrets.clone();
        thread::spawn(move || {
            let result = run_llm(&job, &secrets, &text, &NO_CONTEXT).map_err(|error| error.detail);
            let _ = replies.send(Message::FileProcessed { file, result });
        });
    }

    fn write_file(&mut self, path: &Path, text: &str) -> Result<(), String> {
        crate::storage::write_whole(path, text).map_err(|error| error.to_string())
    }

    fn keep_transcript(&mut self, transcript: FileTranscript) -> Result<i64, String> {
        self.transcripts
            .with(|store| store.add_file(now_ms(), &transcript))
            .map_err(|error| error.to_string())
    }

    fn shutdown(&mut self) {
        // The sound comes back first: the wait below may be cut short.
        self.unmute();
        self.quit.store(true, Ordering::Relaxed);
        self.files.clear();
        self.diarizing.store(true, Ordering::Relaxed);
        self.send(EngineRequest::Stop);
        // Dropping the sender ends the worker after the stop.
        self.engine = None;
        // A request at the engine (a long file window, a server that hangs) is not waited out:
        // the engine's processes end with the app (`local_server`, `sherpa_process`).
        if let Some(worker) = self.worker.take() {
            join_for(worker, SHUTDOWN_WAIT);
        }
    }
}

/// How long quitting waits for the engine's thread.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

/// Waits for `thread` to end, at most `wait`; whether it ended.
pub fn join_for(thread: JoinHandle<()>, wait: Duration) -> bool {
    let (ended, finished) = mpsc::channel();
    thread::spawn(move || {
        let _ = thread.join();
        let _ = ended.send(());
    });
    finished.recv_timeout(wait).is_ok()
}

/// One typing at a time: a dictation's text, its live text and a rewritten selection never go
/// into a field at once, mixed.
static TYPING: Mutex<()> = Mutex::new(());

fn typing() -> MutexGuard<'static, ()> {
    TYPING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Typing while Ctrl or Alt is held would trigger shortcuts in the target, so insertion waits
/// for the user to release the hotkey.
fn wait_for_modifiers() -> Result<(), InsertError> {
    wait_until_released(platform::modifiers_down, &HOTKEYS_HELD, MODIFIER_WAIT)
}

/// Waits until `down` says no: at most `wait`, or for as long as a dictation hotkey is `held`.
fn wait_until_released(
    down: impl Fn() -> bool,
    held: &AtomicU32,
    wait: Duration,
) -> Result<(), InsertError> {
    let deadline = Instant::now() + wait;
    while down() && (Instant::now() < deadline || held.load(Ordering::Relaxed) > 0) {
        thread::sleep(Duration::from_millis(20));
    }
    if down() {
        return Err(InsertError::Failed("modifier keys are still held".into()));
    }
    Ok(())
}

/// The key of a language-model connection, read on the calling thread: the stored one, or for a
/// ChatGPT account an access token, renewed first when it is about to end.
fn credential(secrets: &SecretStore, connection: &Connection) -> Result<Option<String>, String> {
    match connection.provider {
        ProviderKind::ChatGptAccount => {
            chatgpt::access_token(&http::agent(), secrets, &connection.id)
                .map(Some)
                .map_err(|error| error.to_string())
        }
        _ => secrets.get(&connection.id).map_err(|error| format!("cannot read the key: {error}")),
    }
}

/// The language-model call; its errors never contain the key or the text.
fn run_llm(
    job: &LlmJob,
    secrets: &SecretStore,
    text: &str,
    context: &Context,
) -> Result<String, ProcessingError> {
    let failed = |detail: String| ProcessingError { detail, kind: ProcessingFailure::Failed };
    let provider = llm::Provider::of(job.connection.provider)
        .ok_or_else(|| failed("the service has no language models".into()))?;
    let connection = llm::Connection {
        provider,
        base_url: job.connection.base_url.clone(),
        api_key: credential(secrets, &job.connection).map_err(failed)?.unwrap_or_default(),
        model: job.model.clone(),
    };
    let request = llm::Request {
        instruction: &job.instruction,
        text,
        output_language: job.output_language.as_deref(),
        context,
    };
    llm::process(&http::agent(), &connection, &request).map_err(|error| match error {
        LlmError::PlanLimit => {
            ProcessingError { detail: error.to_string(), kind: ProcessingFailure::PlanLimit }
        }
        LlmError::Empty => {
            ProcessingError { detail: error.to_string(), kind: ProcessingFailure::Empty }
        }
        // The plan's sign-in, not a key, was refused.
        LlmError::Unauthorized if provider == llm::Provider::ChatGptPlan => {
            failed(chatgpt::AuthError::SignInAgain.to_string())
        }
        error => failed(error.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quitting does not wait out a request at the engine.
    #[test]
    fn quitting_waits_for_the_engine_only_a_moment() {
        let quick = thread::spawn(|| {});
        assert!(join_for(quick, Duration::from_secs(1)));
        let busy = thread::spawn(|| thread::sleep(Duration::from_secs(5)));
        let started = Instant::now();
        assert!(!join_for(busy, Duration::from_millis(100)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// A silent window's answer is read as no words, as an engine's empty one.
    #[test]
    fn a_silent_window_has_no_words() {
        use crate::files::{Window, parse_words};
        let window =
            Window { index: 0, start: 0, end: 16_000, keep_from: 0.0, keep_to: 1.0, last: true };
        assert_eq!(parse_words(NO_WORDS, &window).unwrap(), Vec::new());
    }

    /// The keys of the next dictation's hotkey are held: the insertion before waits them out.
    #[test]
    fn insertion_waits_for_a_held_hotkey_past_the_usual_wait() {
        let released_at = Instant::now() + Duration::from_millis(200);
        let down = || Instant::now() < released_at;
        let held = AtomicU32::new(1);
        let wait = Duration::from_millis(50);
        let watcher = thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(200));
                held.store(0, Ordering::Relaxed);
            });
            wait_until_released(down, &held, wait)
        });
        assert!(watcher.is_ok());
        // Other modifiers held longer than the wait: the insertion gives up.
        let never_released = AtomicU32::new(0);
        assert!(wait_until_released(|| true, &never_released, wait).is_err());
    }
}
