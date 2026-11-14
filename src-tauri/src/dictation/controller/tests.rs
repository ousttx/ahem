use super::*;
use crate::domain::app_rules::{AppId, AppRule};
use crate::domain::context::{Context, ContextSources};
use crate::domain::engine::UnloadPolicy;
use crate::domain::gesture::RecordingMode;
use crate::domain::modes::{Connection, ProviderKind};
use crate::domain::replacements::Replacement;
use crate::domain::services::GROQ_BASE_URL;
use crate::domain::session::{DiscardReason, FailureKind};
use crate::storage::catalog::{Recognizer, SherpaSpec};
use crate::storage::models::ReadyModel;

#[derive(Debug, Clone, PartialEq)]
enum Call {
    StartRecording,
    Mute,
    Unmute,
    StartEngine,
    /// A cloud engine and its connection.
    StartCloud(&'static str, String),
    StopEngine,
    Transcribe(SessionId),
    Insert(SessionId, String, Option<FocusTarget>),
    Process(SessionId, String),
    Rewrite(u64),
    ReadContext(ContextSources),
    History(String, Option<u32>),
    OpenFile(FileTaskId),
    NextWindow(FileTaskId),
    CloseFile(FileTaskId),
    /// Chunk number and whether Whisper translates.
    TranscribeChunk(u32, bool),
    ProcessFile(String),
    Diarize(FileTaskId),
    WriteFile(PathBuf, String),
    KeepTranscript(FileTranscript),
    Shutdown,
    StartLive(SessionId),
    /// Session and the number of samples fed.
    FeedLive(SessionId, usize),
    FinishLive(SessionId),
    CancelLive(SessionId),
    TypeLive(SessionId, String, Option<FocusTarget>),
}

#[derive(Clone)]
struct Fake {
    calls: Arc<Mutex<Vec<Call>>>,
    options: Arc<Mutex<Vec<TranscribeOptions>>>,
    model: bool,
    /// The selected model streams (Nemotron); a test may switch models midway.
    streaming: Arc<AtomicBool>,
    engine: bool,
    microphone: Result<(), String>,
    seconds: f32,
    /// Recordings are silent; a test may turn it on midway.
    silent: Arc<AtomicBool>,
    focus: Option<FocusTarget>,
    /// The focus of the next presses, before `focus`.
    focuses: Arc<Mutex<std::collections::VecDeque<Option<FocusTarget>>>>,
    /// The program in focus.
    app: Option<AppId>,
    /// What the OS gives when asked for the selected text and the clipboard.
    context: Context,
    /// The context each language-model call got.
    contexts: Arc<Mutex<Vec<Context>>>,
    history: Result<(), String>,
    /// The dictations counted in the statistics: words and recording time.
    counted: Arc<Mutex<Vec<(u64, Duration)>>>,
    /// The default output device, as a test sets it.
    output: Arc<Mutex<Option<OutputFormat>>>,
    /// What keeping a file's transcript gives: its entry, or why not.
    keep: Result<i64, String>,
    /// What writing a file's result gives.
    write: Result<(), String>,
    /// Recordings lose their device midway.
    interrupted: Option<String>,
}

struct FakeRecording {
    seconds: f32,
    silent: bool,
    /// The device went away midway.
    interrupted: Option<String>,
}

impl FakeRecording {
    /// A loud tone counts as speech; silence does not.
    fn samples(&self) -> Vec<f32> {
        let amplitude = if self.silent { 0.0 } else { 0.1 };
        (0..(16_000.0 * self.seconds) as usize)
            .map(|i| amplitude * (i as f32 * 0.09).sin())
            .collect()
    }
}

impl Recording for FakeRecording {
    fn level(&self) -> f32 {
        0.25
    }

    fn rate(&self) -> u32 {
        16_000
    }

    /// The whole recording is there from the start.
    fn samples_since(&self, from: usize) -> Vec<f32> {
        self.samples().get(from..).unwrap_or_default().to_vec()
    }

    fn stop(self: Box<Self>) -> Result<Captured, String> {
        Ok(Captured { samples: self.samples(), rate: 16_000 })
    }

    fn interrupted(&self) -> Option<String> {
        self.interrupted.clone()
    }
}

impl Fake {
    fn log(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Services for Fake {
    fn ready_model(&self, _model_id: &str) -> Option<ReadyModel> {
        let sherpa = self.streaming.load(Ordering::Relaxed).then(|| SherpaSpec {
            recognizer: Recognizer::OnlineTransducer,
            model_type: None,
            encoder: "encoder.onnx".into(),
            decoder: "decoder.onnx".into(),
            joiner: "joiner.onnx".into(),
            tokens: "tokens.txt".into(),
        });
        self.model.then(|| ReadyModel { path: PathBuf::from("model"), sherpa, llama: None })
    }
    fn engine_available(&self, _model: &ReadyModel) -> bool {
        self.engine
    }
    fn focus_target(&self) -> Option<FocusTarget> {
        self.focuses.lock().unwrap().pop_front().unwrap_or(self.focus)
    }
    fn app_identity(&self, target: &FocusTarget) -> Option<AppId> {
        assert_eq!(*target, TARGET);
        self.app.clone()
    }
    fn start_recording(
        &mut self,
        _microphone: Option<String>,
    ) -> Result<Box<dyn Recording>, String> {
        self.microphone.clone()?;
        self.log(Call::StartRecording);
        Ok(Box::new(FakeRecording {
            seconds: self.seconds,
            silent: self.silent.load(Ordering::Relaxed),
            interrupted: self.interrupted.clone(),
        }))
    }
    fn mute(&mut self) {
        self.log(Call::Mute);
    }
    fn unmute(&mut self) {
        self.log(Call::Unmute);
    }
    fn output_format(&self) -> Option<OutputFormat> {
        self.output.lock().unwrap().clone()
    }
    fn start_engine(&mut self, engine: SpeechEngine) {
        self.log(match engine {
            SpeechEngine::Model(_) => Call::StartEngine,
            SpeechEngine::Cloud { spec, connection } => Call::StartCloud(spec.id, connection.id),
        });
    }
    fn stop_engine(&mut self) {
        self.log(Call::StopEngine);
    }
    fn transcribe(&mut self, session: SessionId, _audio: Captured, options: TranscribeOptions) {
        self.options.lock().unwrap().push(options);
        self.log(Call::Transcribe(session));
    }
    fn start_live(&mut self, session: SessionId, _language: Option<String>, _rate: u32) {
        self.log(Call::StartLive(session));
    }
    fn feed_live(&mut self, session: SessionId, samples: Vec<f32>) {
        self.log(Call::FeedLive(session, samples.len()));
    }
    fn finish_live(&mut self, session: SessionId, _audio: Captured, options: TranscribeOptions) {
        self.options.lock().unwrap().push(options);
        self.log(Call::FinishLive(session));
    }
    fn cancel_live(&mut self, session: SessionId) {
        self.log(Call::CancelLive(session));
    }
    fn type_live(&mut self, session: SessionId, text: String, target: Option<FocusTarget>) {
        self.log(Call::TypeLive(session, text, target));
    }
    fn begin_insert(&mut self, session: SessionId, text: String, target: Option<FocusTarget>) {
        self.log(Call::Insert(session, text, target));
    }
    fn process(&mut self, session: SessionId, text: String, _job: LlmJob, context: PendingContext) {
        self.contexts.lock().unwrap().push(context.wait());
        self.log(Call::Process(session, text));
    }
    fn read_context(
        &mut self,
        _id: u64,
        _target: Option<FocusTarget>,
        sources: ContextSources,
    ) -> std::sync::mpsc::Receiver<Context> {
        self.log(Call::ReadContext(sources));
        let (send, read) = std::sync::mpsc::channel();
        send.send(self.context.clone()).unwrap();
        read
    }
    fn rewrite_selection(&mut self, request: u64, _target: FocusTarget, _job: LlmJob) {
        self.log(Call::Rewrite(request));
    }
    fn save_history(&mut self, result: &DictationResult, limit: Option<u32>) -> Result<(), String> {
        self.log(Call::History(result.text().to_string(), limit));
        self.history.clone()
    }
    fn count_dictation(&mut self, words: u64, recording: Duration) -> Result<(), String> {
        self.counted.lock().unwrap().push((words, recording));
        Ok(())
    }
    fn open_file(&mut self, file: FileTaskId, _path: PathBuf) {
        self.log(Call::OpenFile(file));
    }
    fn next_file_window(&mut self, file: FileTaskId) {
        self.log(Call::NextWindow(file));
    }
    fn close_file(&mut self, file: FileTaskId) {
        self.log(Call::CloseFile(file));
    }
    fn transcribe_chunk(
        &mut self,
        _file: FileTaskId,
        chunk: u32,
        _audio: Captured,
        options: TranscribeOptions,
    ) {
        assert!(options.words, "file chunks ask for word times");
        self.log(Call::TranscribeChunk(chunk, options.translate));
    }
    fn diarize_file(&mut self, file: FileTaskId, _path: PathBuf, words: Vec<Word>) {
        assert!(!words.is_empty(), "the voices are told apart by sentence of the words");
        self.log(Call::Diarize(file));
    }

    fn process_file(&mut self, _file: FileTaskId, text: String, _job: LlmJob) {
        self.log(Call::ProcessFile(text));
    }
    fn write_file(&mut self, path: &std::path::Path, text: &str) -> Result<(), String> {
        self.log(Call::WriteFile(path.to_path_buf(), text.to_string()));
        self.write.clone()
    }
    fn keep_transcript(&mut self, transcript: FileTranscript) -> Result<i64, String> {
        self.log(Call::KeepTranscript(transcript));
        self.keep.clone()
    }
    fn shutdown(&mut self) {
        self.log(Call::Shutdown);
    }
}

struct Harness {
    controller: Controller<Fake>,
    calls: Arc<Mutex<Vec<Call>>>,
    events: Arc<Mutex<Vec<CoreEvent>>>,
    last: Arc<Mutex<Option<DictationResult>>>,
    recent: Arc<Mutex<RecentApps>>,
}

const S1: SessionId = SessionId(1);
const S2: SessionId = SessionId(2);
const S3: SessionId = SessionId(3);
const TARGET: FocusTarget = FocusTarget { window: 7, process_id: 9 };
const SPEAKERS: OutputFormat = OutputFormat { device: String::new(), rate: 48_000, channels: 2 };

fn fake() -> Fake {
    Fake {
        calls: Arc::default(),
        options: Arc::default(),
        model: true,
        streaming: Arc::default(),
        engine: true,
        microphone: Ok(()),
        seconds: 2.0,
        silent: Arc::default(),
        focus: Some(TARGET),
        focuses: Arc::default(),
        app: None,
        context: Context::default(),
        contexts: Arc::default(),
        history: Ok(()),
        counted: Arc::default(),
        output: Arc::new(Mutex::new(Some(SPEAKERS))),
        keep: Ok(KEPT),
        write: Ok(()),
        interrupted: None,
    }
}

fn harness_with(fake: Fake, configure: impl FnOnce(&mut Settings)) -> Harness {
    // Without muting unless a test is about it.
    let mut settings = Settings {
        model_id: Some("model".into()),
        mute_while_recording: false,
        ..Settings::default()
    };
    settings.modes.push(modes::english_mode());
    configure(&mut settings);
    let calls = Arc::clone(&fake.calls);
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let last = Arc::new(Mutex::new(None));
    let emit = Box::new(move |event| sink.lock().unwrap().push(event));
    let recent = Arc::new(Mutex::new(RecentApps::default()));
    let controller = Controller::new(fake, settings, Arc::clone(&last), Arc::clone(&recent), emit);
    Harness { controller, calls, events, last, recent }
}

fn harness() -> Harness {
    harness_with(fake(), |_| {})
}

impl Harness {
    fn send(&mut self, message: Message) -> &mut Self {
        self.controller.handle(message);
        self
    }

    /// Presses and releases the main hotkey (plain dictation unless a test changes it).
    fn press(&mut self) -> &mut Self {
        self.press_action(HotkeyAction::Main)
    }

    /// Presses and releases the "by application" hotkey.
    fn press_by_app(&mut self) -> &mut Self {
        self.press_action(HotkeyAction::ByApp)
    }

    fn press_action(&mut self, action: HotkeyAction) -> &mut Self {
        self.send(Message::HotkeyPressed(action.clone())).send(Message::HotkeyReleased(action))
    }

    fn press_mode(&mut self, mode_id: &str) -> &mut Self {
        let action = HotkeyAction::Dictate { mode_id: mode_id.into() };
        self.send(Message::HotkeyPressed(action.clone())).send(Message::HotkeyReleased(action))
    }

    fn events(&self) -> Vec<CoreEvent> {
        self.events.lock().unwrap().clone()
    }

    fn transcribed(&mut self, session: SessionId, text: &str) -> &mut Self {
        let transcript = Transcript { text: text.into(), language: Some("english".into()) };
        self.send(Message::Transcribed { session, result: Ok(transcript) })
    }

    fn calls(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }

    fn blocked(&self) -> Vec<BlockReason> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                CoreEvent::Dictation(DictationEvent::Blocked { reason, .. }) => Some(*reason),
                _ => None,
            })
            .collect()
    }

    fn state_of(&self, session: SessionId) -> Option<SessionState> {
        self.events.lock().unwrap().iter().rev().find_map(|event| match event {
            CoreEvent::Session(SessionEvent::StateChanged { session: id, state })
                if *id == session =>
            {
                Some(state.clone())
            }
            _ => None,
        })
    }
}

#[test]
fn plain_dictation_end_to_end() {
    let mut h = harness();
    h.press();
    assert_eq!(h.calls(), vec![Call::StartRecording, Call::StartEngine]);
    h.send(Message::EngineReady);
    h.press();
    assert_eq!(h.calls(), vec![Call::Transcribe(S1), Call::Unmute]);
    h.transcribed(S1, "hello");
    // Immediate unload right after the transcript, before the text is inserted.
    assert_eq!(h.calls(), vec![Call::StopEngine, Call::Insert(S1, "hello".into(), Some(TARGET))]);
    h.send(Message::Inserted { session: S1, result: Ok(()) });
    assert_eq!(h.state_of(S1), Some(SessionState::Inserted));
    let last = h.last.lock().unwrap().clone().expect("last transcription");
    assert_eq!(last.text(), "hello");
    assert_eq!(last.language.as_deref(), Some("en"));
    assert!(last.inserted);
    h.send(Message::EngineStopped);
    assert!(h.calls().is_empty());
}

fn headset_mode(fake: &Fake) {
    *fake.output.lock().unwrap() =
        Some(OutputFormat { device: String::new(), rate: 24_000, channels: 1 });
}

#[test]
fn sound_returns_once_the_headset_is_back() {
    let fake = fake();
    let output = fake.clone();
    let mut h = harness_with(fake, |s| s.mute_while_recording = true);
    h.press().send(Message::EngineReady);
    h.calls();
    // The microphone switched the headset into its headset mode.
    headset_mode(&output);
    h.press();
    assert_eq!(h.calls(), vec![Call::Transcribe(S1)], "the sound waits for the headset");
    assert!(h.controller.next_wake().is_some(), "watches the output");
    h.controller.tick(Instant::now());
    assert!(h.calls().is_empty(), "still in its headset mode");
    // The format is back at the start of the switch back: the sound waits it out.
    *output.output.lock().unwrap() = Some(SPEAKERS);
    let back = Instant::now();
    h.controller.tick(back);
    assert!(h.calls().is_empty(), "the switch back is under way");
    h.controller.tick(back + SWITCH_SETTLE);
    assert_eq!(h.calls(), vec![Call::Unmute]);
    assert_eq!(h.controller.next_wake(), None);
}

#[test]
fn sound_returns_after_a_while_if_the_headset_stays() {
    let fake = fake();
    let output = fake.clone();
    let mut h = harness_with(fake, |s| s.mute_while_recording = true);
    h.press().send(Message::EngineReady);
    headset_mode(&output);
    h.calls();
    h.press();
    assert!(!h.calls().contains(&Call::Unmute));
    h.controller.tick(Instant::now() + RESTORE_LIMIT);
    assert_eq!(h.calls(), vec![Call::Unmute]);
    assert_eq!(h.controller.next_wake(), None);
}

#[test]
fn muted_sound_stays_muted_into_the_next_dictation() {
    let fake = fake();
    let output = fake.clone();
    let mut h = harness_with(fake, |s| s.mute_while_recording = true);
    h.press().send(Message::EngineReady);
    headset_mode(&output);
    h.press();
    h.calls();
    h.press();
    assert_eq!(h.calls(), vec![Call::Mute, Call::StartRecording], "no unmute in between");
    // The headset is still in its headset mode: the format from before is waited for.
    h.press();
    assert!(!h.calls().contains(&Call::Unmute), "waits for the headset again");
}

/// A streaming model (Nemotron), the hotkey window kept for insertion.
fn streaming_harness(fake: Fake, configure: impl FnOnce(&mut Settings)) -> Harness {
    harness_with(Fake { streaming: Arc::new(AtomicBool::new(true)), ..fake }, configure)
}

/// Starts a toggle-mode dictation with the engine ready and forgets the calls so far.
fn streaming_started() -> Harness {
    let mut h = streaming_harness(fake(), |_| {});
    h.press().send(Message::EngineReady);
    h.calls();
    h
}

fn live_text(h: &mut Harness, text: &str) {
    h.send(Message::LiveText { session: S1, text: text.into() });
}

fn typed(h: &mut Harness) {
    h.send(Message::LiveTyped { session: S1, result: Ok(()) });
}

#[test]
fn streaming_model_types_while_speaking() {
    let mut h = streaming_harness(fake(), |_| {});
    h.press();
    assert_eq!(h.calls(), vec![Call::StartRecording, Call::StartEngine]);
    h.send(Message::EngineReady);
    assert_eq!(h.calls(), vec![Call::StartLive(S1)], "the stream opens at the ready engine");
    h.controller.tick(Instant::now() + FEED_INTERVAL);
    assert_eq!(h.calls(), vec![Call::FeedLive(S1, 32_000)], "2 s at 16 kHz so far");
    live_text(&mut h, "Hello  wor");
    assert_eq!(h.calls(), vec![Call::TypeLive(S1, "Hello wor".into(), Some(TARGET))]);
    // One piece at a time: the next waits until the first is typed.
    live_text(&mut h, "Hello  world.");
    assert!(h.calls().is_empty());
    typed(&mut h);
    // The full stop waits for the next words: a removed sound after it could take it along.
    assert_eq!(h.calls(), vec![Call::TypeLive(S1, "ld".into(), Some(TARGET))]);
    typed(&mut h);
    h.press();
    // Everything was fed while recording; the engine finishes the stream.
    assert_eq!(h.calls(), vec![Call::FinishLive(S1), Call::Unmute]);
    h.transcribed(S1, "Hello world. Bye.");
    // Only the rest goes in.
    let insert = Call::Insert(S1, ". Bye.".into(), Some(TARGET));
    assert_eq!(h.calls(), vec![Call::StopEngine, insert]);
    h.send(Message::Inserted { session: S1, result: Ok(()) });
    assert_eq!(h.state_of(S1), Some(SessionState::Inserted));
    let last = h.last.lock().unwrap().clone().expect("last transcription");
    assert_eq!(last.text(), "Hello world. Bye.", "history and last result keep all of it");
}

/// Another model chosen while the streaming one still loads: no stream opened at the model
/// being stopped, so nothing is typed that the new model's text would not continue, and no
/// engine is kept for a stream it does not have; the new one (here one that does not stream)
/// recognizes the whole recording.
#[test]
fn a_stream_opens_only_at_a_ready_streaming_engine() {
    let streaming = Arc::new(AtomicBool::new(true));
    let mut h = harness_with(Fake { streaming: Arc::clone(&streaming), ..fake() }, |_| {});
    h.press();
    assert_eq!(h.calls(), vec![Call::StartRecording, Call::StartEngine], "none while it loads");
    streaming.store(false, Ordering::Relaxed);
    let mut other = h.controller.settings.clone();
    other.model_id = Some("another".into());
    h.send(Message::Settings(Box::new(other)));
    h.send(Message::EngineStopped).send(Message::EngineReady);
    h.controller.tick(Instant::now() + 2 * FEED_INTERVAL);
    let calls = h.calls();
    assert!(calls.contains(&Call::StartEngine), "{calls:?}");
    assert!(!calls.iter().any(|call| matches!(call, Call::StartLive(_) | Call::FeedLive(..))));
    // Another model again: nothing keeps the running one.
    let mut third = h.controller.settings.clone();
    third.model_id = Some("third".into());
    h.send(Message::Settings(Box::new(third)));
    assert!(h.calls().contains(&Call::StopEngine), "not kept for a stream");
    h.send(Message::EngineStopped).send(Message::EngineReady).press();
    assert!(h.calls().contains(&Call::Transcribe(S1)), "the whole recording");
}

/// The engine crashes while a dictation is recognized and the next one streams: started again,
/// it gets that stream anew from the recording's start (the engine's thread dropped the old one),
/// and the stream no longer counts as open meanwhile.
#[test]
fn a_stream_opens_again_at_an_engine_started_after_a_crash() {
    let mut h = streaming_harness(fake(), |s| s.unload_policy = UnloadPolicy::Never);
    h.press().send(Message::EngineReady).press(); // S1 is recognized.
    h.press(); // S2 records, its stream open.
    assert!(h.calls().contains(&Call::StartLive(S2)));
    h.send(Message::EngineCrashed("gone".into()));
    assert!(h.calls().contains(&Call::StartEngine), "S2 still holds it");
    assert!(h.controller.open_stream().is_none(), "the old stream is gone");
    h.send(Message::EngineReady);
    assert!(h.calls().contains(&Call::StartLive(S2)), "opened anew");
    h.controller.tick(Instant::now() + 2 * FEED_INTERVAL);
    let fed: usize = h
        .calls()
        .iter()
        .filter_map(|call| match call {
            Call::FeedLive(S2, samples) => Some(*samples),
            _ => None,
        })
        .sum();
    assert!(fed >= 32_000, "from the recording's start: {fed}");
}

/// Another model chosen while a stream is open: the old engine keeps it to the end of the
/// recording, so what is typed as it is spoken goes on, then stops; what waits meanwhile goes
/// to the new engine.
#[test]
fn a_model_change_lets_the_open_stream_finish_at_the_old_engine() {
    let mut h = streaming_started();
    let mut other = h.controller.settings.clone();
    other.model_id = Some("another".into());
    h.send(Message::Settings(Box::new(other)));
    assert_eq!(h.calls(), vec![], "the stream keeps the old engine");
    h.controller.tick(Instant::now() + 2 * FEED_INTERVAL);
    assert!(h.calls().iter().any(|call| matches!(call, Call::FeedLive(S1, _))));
    h.press();
    let calls = h.calls();
    assert!(calls.contains(&Call::FinishLive(S1)), "{calls:?}");
    assert!(!calls.contains(&Call::StopEngine), "not before the stream's answer");
    h.transcribed(S1, "hello there");
    let calls = h.calls();
    assert!(calls.contains(&Call::StopEngine), "{calls:?}");
    h.send(Message::EngineStopped).press();
    assert!(h.calls().contains(&Call::StartEngine), "the next dictation gets the new engine");
}

/// A stream that kept the old engine and ends without an answer (a recording too short)
/// lets it stop all the same.
#[test]
fn a_stream_that_kept_the_old_engine_and_is_dropped_lets_it_stop() {
    let mut h = streaming_harness(Fake { seconds: 0.05, ..fake() }, |_| {});
    h.press().send(Message::EngineReady);
    assert!(h.calls().contains(&Call::StartLive(S1)));
    let mut other = h.controller.settings.clone();
    other.model_id = Some("another".into());
    h.send(Message::Settings(Box::new(other)));
    assert_eq!(h.calls(), vec![], "the stream keeps the old engine");
    h.press();
    assert_eq!(h.state_of(S1), Some(SessionState::Discarded { reason: DiscardReason::TooShort }));
    let calls = h.calls();
    assert!(calls.contains(&Call::StopEngine), "{calls:?}");
}

#[test]
fn a_recording_begun_while_the_engine_stops_streams_once_it_is_ready() {
    let mut h = streaming_harness(fake(), |_| {});
    // A first dictation ends; with "unload at once" the engine is being stopped.
    h.press().send(Message::EngineReady);
    h.press().transcribed(S1, "first");
    h.send(Message::Inserted { session: S1, result: Ok(()) });
    assert_eq!(h.controller.engine.state(), EngineState::Unloading);
    h.calls();
    // The next recording asks for no stream yet: the engine would not be there for it.
    h.press();
    assert!(!h.calls().contains(&Call::StartLive(S2)));
    h.controller.tick(Instant::now() + FEED_INTERVAL);
    assert!(!h.calls().iter().any(|call| matches!(call, Call::FeedLive(..))));
    // Stopped, started again and ready: the stream opens and gets all that was recorded.
    h.send(Message::EngineStopped);
    assert!(h.calls().contains(&Call::StartEngine));
    h.send(Message::EngineReady);
    assert!(h.calls().contains(&Call::StartLive(S2)));
    h.controller.tick(Instant::now() + 2 * FEED_INTERVAL);
    assert!(h.calls().iter().any(|call| matches!(call, Call::FeedLive(S2, _))));
}

/// The engine has one stream: the next recording's opens when the one before is finished.
#[test]
fn a_stream_waits_for_the_one_before_to_finish() {
    let mut h = streaming_harness(fake(), |_| {});
    h.press().send(Message::EngineReady).press();
    assert!(h.calls().contains(&Call::FinishLive(S1)));
    // S1 is transcribed; S2 records and stops, and waits in line with its stream open.
    h.press();
    assert!(h.calls().contains(&Call::StartLive(S2)), "S1's stream is finished");
    h.press();
    assert!(!h.calls().contains(&Call::FinishLive(S2)), "S1 is still transcribed");
    // A third recording: S2's stream is still open at the engine.
    h.press();
    assert!(!h.calls().contains(&Call::StartLive(S3)), "would replace S2's stream");
    h.transcribed(S1, "one");
    let calls = h.calls();
    let finish = calls.iter().position(|call| *call == Call::FinishLive(S2));
    let start = calls.iter().position(|call| *call == Call::StartLive(S3));
    assert!(finish.is_some() && start > finish, "{calls:?}");
}

/// Auto mode with the default Alt hotkey: nothing is typed while the key may still be held;
/// once a tap leaves the recording going, the text so far goes in.
#[test]
fn auto_tap_types_live_text_once_the_hotkey_is_up() {
    let mut h = streaming_harness(fake(), |_| {});
    let action = HotkeyAction::Main;
    h.send(Message::HotkeyPressed(action.clone())).send(Message::EngineReady);
    h.calls();
    live_text(&mut h, "Hello");
    assert!(h.calls().is_empty(), "the hotkey may still be held");
    h.send(Message::HotkeyReleased(action));
    assert_eq!(h.calls(), vec![Call::TypeLive(S1, "Hello".into(), Some(TARGET))]);
}

#[test]
fn hold_mode_with_a_shift_hotkey_types_while_speaking() {
    let mut h = streaming_harness(fake(), |s| {
        s.recording_mode = RecordingMode::Hold;
        s.hotkey = "shift+KeyZ".into();
    });
    let action = HotkeyAction::Main;
    h.send(Message::HotkeyPressed(action)).send(Message::EngineReady);
    h.calls();
    live_text(&mut h, "Hello");
    assert_eq!(h.calls(), vec![Call::TypeLive(S1, "Hello".into(), Some(TARGET))]);
}

/// The default hotkey, Ctrl+Alt+Space, held: typed letters would act as shortcuts.
#[test]
fn hold_mode_with_ctrl_alt_streams_without_typing() {
    let mut h = streaming_harness(fake(), |s| s.recording_mode = RecordingMode::Hold);
    let action = HotkeyAction::Main;
    h.send(Message::HotkeyPressed(action.clone())).send(Message::EngineReady);
    h.calls();
    live_text(&mut h, "Hello");
    assert!(h.calls().is_empty(), "held modifiers would turn typing into shortcuts");
    h.send(Message::HotkeyReleased(action));
    // Not fed yet (no tick): the whole recording goes in before the stream ends.
    let feed = Call::FeedLive(S1, 32_000);
    assert_eq!(h.calls(), vec![feed, Call::FinishLive(S1), Call::Unmute]);
    h.transcribed(S1, "Hello there");
    let insert = Call::Insert(S1, "Hello there".into(), Some(TARGET));
    assert_eq!(h.calls(), vec![Call::StopEngine, insert]);
}

#[test]
fn rewritten_live_text_stops_typing_without_doubling() {
    let mut h = streaming_started();
    live_text(&mut h, "Hello");
    typed(&mut h);
    h.calls();
    live_text(&mut h, "Help me");
    assert!(h.calls().is_empty());
    h.press().transcribed(S1, "Help me");
    let calls = h.calls();
    // Nothing is inserted: that would double what is already typed.
    assert_eq!(calls.last(), Some(&Call::StopEngine), "{calls:?}");
    assert!(!calls.iter().any(|call| matches!(call, Call::Insert(..))));
    assert_eq!(h.state_of(S1), Some(SessionState::Inserted));
}

#[test]
fn final_insertion_waits_for_the_piece_being_typed() {
    let mut h = streaming_started();
    live_text(&mut h, "Hi");
    h.press().transcribed(S1, "Hi there");
    let calls = h.calls();
    assert_eq!(calls.last(), Some(&Call::StopEngine), "nothing inserted yet: {calls:?}");
    typed(&mut h);
    assert_eq!(h.calls(), vec![Call::Insert(S1, " there".into(), Some(TARGET))]);
}

#[test]
fn failed_live_typing_leaves_the_rest_for_the_end() {
    let mut h = streaming_started();
    live_text(&mut h, "One");
    typed(&mut h);
    live_text(&mut h, "One two");
    h.send(Message::LiveTyped { session: S1, result: Err(InsertError::FocusChanged) });
    live_text(&mut h, "One two three");
    h.calls();
    h.press().transcribed(S1, "One two three");
    let calls = h.calls();
    let insert = Call::Insert(S1, " two three".into(), Some(TARGET));
    assert_eq!(calls.last(), Some(&insert), "{calls:?}");
}

#[test]
fn live_typing_cut_short_does_not_type_its_beginning_twice() {
    let mut h = streaming_started();
    live_text(&mut h, "One");
    typed(&mut h);
    live_text(&mut h, "One two");
    // Focus moved after " t" of " two" went in.
    let cut = Err(InsertError::Interrupted { typed: 2 });
    h.send(Message::LiveTyped { session: S1, result: cut });
    h.calls();
    h.press().transcribed(S1, "One two three");
    let insert = Call::Insert(S1, "wo three".into(), Some(TARGET));
    assert_eq!(h.calls().last(), Some(&insert));
}

#[test]
fn processing_modes_stream_without_typing() {
    let mut h = streaming_harness(fake(), |_| {});
    h.press_mode(modes::ENGLISH).send(Message::EngineReady);
    h.calls();
    live_text(&mut h, "Hello");
    assert!(h.calls().is_empty());
}

fn with_ai(settings: &mut Settings) {
    settings.replacements = vec![Replacement { from: "эй ай".into(), to: "AI".into() }];
}

#[test]
fn replacements_change_the_dictated_text() {
    let mut h = harness_with(fake(), with_ai);
    h.press().send(Message::EngineReady).press().transcribed(S1, "Спросим эй ай.");
    assert!(h.calls().contains(&Call::Insert(S1, "Спросим AI.".into(), Some(TARGET))));
    h.send(Message::Inserted { session: S1, result: Ok(()) });
    let last = h.last.lock().unwrap().clone().expect("last transcription");
    assert_eq!(last.transcript, "Спросим AI.", "the history and the last result too");
}

#[test]
fn the_language_model_gets_the_replaced_text() {
    let mut h = harness_with(fake(), |s| {
        with_email(s);
        with_ai(s);
    });
    h.press_mode("email").send(Message::EngineReady).press_mode("email");
    h.transcribed(S1, "эй ай");
    assert!(h.calls().contains(&Call::Process(S1, "AI".into())));
}

fn cleaned_by_model(settings: &mut Settings) {
    settings.connections = vec![connection()];
    settings.clean_text = CleanText::Model { connection_id: "c1".into(), model: "claude".into() };
}

#[test]
fn plain_dictation_drops_fillers_by_default() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press().transcribed(S1, "Э-э, привет, эм, мир.");
    assert!(h.calls().contains(&Call::Insert(S1, "Привет мир.".into(), Some(TARGET))));
}

#[test]
fn plain_dictation_drops_the_users_own_fillers() {
    let own = |s: &mut Settings| s.fillers = "ну, типа".into();
    let mut h = harness_with(fake(), own);
    h.press().send(Message::EngineReady).press().transcribed(S1, "Ну, э-э, типа привет");
    assert!(h.calls().contains(&Call::Insert(S1, "Э-э привет".into(), Some(TARGET))));
}

#[test]
fn fillers_stay_when_clean_text_is_off_or_in_other_modes() {
    let off = |s: &mut Settings| s.clean_text = CleanText::Off;
    let mut h = harness_with(fake(), off);
    h.press().send(Message::EngineReady).press().transcribed(S1, "Э-э, привет");
    assert!(h.calls().contains(&Call::Insert(S1, "Э-э, привет".into(), Some(TARGET))));

    let mut h = harness();
    h.press_mode(modes::ENGLISH).send(Message::EngineReady).press_mode(modes::ENGLISH);
    h.transcribed(S1, "Uh, hello");
    assert!(h.calls().contains(&Call::Insert(S1, "Uh, hello".into(), Some(TARGET))));
}

#[test]
fn clean_text_with_a_model_sends_the_text_without_fillers() {
    let mut h = harness_with(fake(), cleaned_by_model);
    h.press().send(Message::EngineReady).press().transcribed(S1, "Э-э, в два, нет, в три");
    assert!(h.calls().contains(&Call::Process(S1, "В два, нет, в три".into())));
    h.send(Message::Processed { session: S1, result: Ok("В три.".into()) });
    assert!(h.calls().contains(&Call::Insert(S1, "В три.".into(), Some(TARGET))));
}

#[test]
fn clean_text_without_its_service_inserts_the_text_without_fillers() {
    let gone = |s: &mut Settings| {
        s.clean_text = CleanText::Model { connection_id: "gone".into(), model: "x".into() };
    };
    let mut h = harness_with(fake(), gone);
    h.press().send(Message::EngineReady).press().transcribed(S1, "Эм, привет");
    let calls = h.calls();
    assert!(!calls.iter().any(|call| matches!(call, Call::Process(..))));
    assert!(calls.contains(&Call::Insert(S1, "Привет".into(), Some(TARGET))));
}

#[test]
fn clean_text_with_a_model_does_not_type_while_speaking() {
    let mut h = streaming_harness(fake(), cleaned_by_model);
    h.press().send(Message::EngineReady);
    h.calls();
    live_text(&mut h, "Hello world.");
    assert!(h.calls().is_empty(), "the model gets the text first");
}

#[test]
fn typing_while_speaking_drops_fillers_and_agrees_with_the_end() {
    let mut h = streaming_started();
    live_text(&mut h, "Hello uh world. And");
    let calls = h.calls();
    assert_eq!(calls, vec![Call::TypeLive(S1, "Hello world. And".into(), Some(TARGET))]);
    typed(&mut h);
    h.press();
    h.calls();
    h.transcribed(S1, "Hello uh world. And done.");
    assert!(h.calls().contains(&Call::Insert(S1, " done.".into(), Some(TARGET))));
}

/// Fillers go with their commas whether or not text was typed while speaking.
#[test]
fn a_streamed_dictation_drops_fillers_with_their_commas() {
    let mut h = streaming_started();
    h.press();
    h.calls();
    h.transcribed(S1, "Hello, uh, world. Э-э, я думаю.");
    let calls = h.calls();
    let insert = Call::Insert(S1, "Hello world. Я думаю.".into(), Some(TARGET));
    assert!(calls.contains(&insert), "{calls:?}");
    let mut h = streaming_started();
    live_text(&mut h, "Hello, uh, world. And");
    assert_eq!(h.calls(), vec![Call::TypeLive(S1, "Hello world. And".into(), Some(TARGET))]);
}

#[test]
fn live_typing_waits_for_a_phrase_being_said() {
    let mut h = streaming_harness(fake(), with_ai);
    h.press().send(Message::EngineReady);
    h.calls();
    live_text(&mut h, "Привет эй");
    assert_eq!(h.calls(), vec![Call::TypeLive(S1, "Привет".into(), Some(TARGET))]);
    typed(&mut h);
    live_text(&mut h, "Привет эй ай");
    assert!(h.calls().is_empty(), "the last word may still grow");
    live_text(&mut h, "Привет эй ай, как");
    assert_eq!(h.calls(), vec![Call::TypeLive(S1, " AI, как".into(), Some(TARGET))]);
    typed(&mut h);
    h.press();
    h.calls();
    h.transcribed(S1, "Привет эй ай, как дела?");
    assert_eq!(h.calls(), vec![Call::StopEngine, Call::Insert(S1, " дела?".into(), Some(TARGET))]);
}

#[test]
fn discarded_streaming_dictation_closes_the_stream() {
    let mut h =
        streaming_harness(Fake { silent: Arc::new(AtomicBool::new(true)), ..fake() }, |_| {});
    h.press().send(Message::EngineReady);
    h.calls();
    h.press();
    assert!(h.calls().contains(&Call::CancelLive(S1)));
}

const F1: FileTaskId = FileTaskId(1);
/// The entry the fake store gives a kept transcript.
const KEPT: i64 = 7;

fn file_request(mode_id: &str, format: ExportFormat) -> Message {
    Message::StartFile(Box::new(FileRequest {
        source: FileSource::Media(PathBuf::from("talk.mp3")),
        name: "talk.mp3".into(),
        output: PathBuf::from("talk.md"),
        format,
        mode_id: mode_id.into(),
        replacements: true,
        speakers: None,
        reply: None,
    }))
}

/// A window of `seconds` with the engine's reply: one word per segment.
fn window(index: u32, from: f64, to: f64, last: bool) -> Window {
    let start = (from * 16_000.0) as usize;
    Window { index, start, end: (to * 16_000.0) as usize, keep_from: from, keep_to: to, last }
}

fn words_json(words: &[(&str, f64, f64)]) -> String {
    let segments: Vec<_> = words
        .iter()
        .map(|(text, start, end)| {
            serde_json::json!({ "text": text, "start": start, "end": end, "tokens": [] })
        })
        .collect();
    serde_json::json!({ "text": "", "language": "english", "segments": segments }).to_string()
}

fn file_events(h: &Harness) -> Vec<FileEvent> {
    h.events()
        .into_iter()
        .filter_map(|event| match event {
            CoreEvent::File(event) => Some(event),
            _ => None,
        })
        .collect()
}

fn send_window(h: &mut Harness, window: Window) {
    let audio = Captured { samples: vec![0.0; window.end - window.start], rate: 16_000 };
    h.send(Message::FileWindow {
        file: F1,
        window,
        audio,
        duration: Some(Duration::from_secs(40)),
    });
}

#[test]
fn a_file_is_transcribed_window_by_window_and_saved() {
    let mut h = harness();
    h.send(file_request(modes::PLAIN, ExportFormat::Txt));
    assert_eq!(h.calls(), vec![Call::StartEngine, Call::OpenFile(F1)]);
    send_window(&mut h, window(0, 0.0, 27.0, false));
    assert!(h.calls().is_empty(), "waits for the engine");
    h.send(Message::EngineReady);
    assert_eq!(h.calls(), vec![Call::TranscribeChunk(0, false), Call::NextWindow(F1)]);
    send_window(&mut h, window(1, 27.0, 40.0, true));
    h.send(Message::ChunkTranscribed {
        file: F1,
        chunk: 0,
        result: Ok(words_json(&[(" Hello", 1.0, 1.5), (" world.", 1.5, 2.0)])),
    });
    // The last window: no request for another one.
    assert_eq!(h.calls(), vec![Call::TranscribeChunk(1, false)]);
    h.send(Message::ChunkTranscribed {
        file: F1,
        chunk: 1,
        result: Ok(words_json(&[(" Bye.", 5.0, 5.5)])),
    });
    let calls = h.calls();
    assert!(calls.contains(&Call::StopEngine), "the engine is released");
    // "Bye." comes 30 s later (27 s + 5 s in its window): a new paragraph.
    assert!(
        calls.contains(&Call::WriteFile("talk.md".into(), "Hello world.\n\nBye.\n".into())),
        "{calls:?}"
    );
    let events = file_events(&h);
    assert_eq!(events[0], FileEvent::Started { file: 1, name: "talk.mp3".into() });
    assert!(events.contains(&FileEvent::Progress {
        file: 1,
        done_ms: 27_000,
        total_ms: Some(40_000)
    }));
    assert_eq!(
        events.last(),
        Some(&FileEvent::Finished {
            file: 1,
            path: "talk.md".into(),
            processing_error: None,
            transcript: Some(KEPT)
        })
    );
}

fn kept(calls: Vec<Call>) -> Vec<FileTranscript> {
    calls
        .into_iter()
        .filter_map(|call| match call {
            Call::KeepTranscript(transcript) => Some(transcript),
            _ => None,
        })
        .collect()
}

fn turn_texts(transcript: &FileTranscript) -> Vec<(Speaker, String)> {
    transcript.turns.iter().map(|turn| (turn.speaker, turn.text())).collect()
}

/// A result that cannot be written is held until the next file and written where the user
/// chooses then; the next file lets it go.
#[test]
fn a_result_that_cannot_be_written_is_held_to_be_saved_elsewhere() {
    let mut h = harness_with(Fake { write: Err("disk full".into()), ..fake() }, |_| {});
    h.send(file_request(modes::PLAIN, ExportFormat::Txt));
    two_voices(&mut h);
    assert!(matches!(file_events(&h).last(), Some(FileEvent::Failed { held: true, .. })));
    let take = |h: &mut Harness| {
        let (reply, answer) = std::sync::mpsc::channel();
        h.send(Message::TakeHeld { reply });
        answer.recv().unwrap()
    };
    let held = take(&mut h).expect("held");
    assert!(held.text.contains("Hello there."), "{}", held.text);
    assert_eq!(held.transcript, Some(KEPT));
    // A meeting's track does not let it go; the user's next file does.
    h.send(Message::StartFile(Box::new(FileRequest {
        source: FileSource::Media(PathBuf::from("me.opus")),
        name: "me.opus".into(),
        output: PathBuf::new(),
        format: ExportFormat::Txt,
        mode_id: modes::PLAIN.into(),
        replacements: false,
        speakers: None,
        reply: Some(std::sync::mpsc::channel().0),
    })));
    assert!(take(&mut h).is_some(), "kept past a meeting's track");
    h.send(Message::CancelTrack);
    h.send(file_request(modes::PLAIN, ExportFormat::Txt));
    assert!(take(&mut h).is_none());
}

/// The result cannot be written (a drive gone, a full disk): the transcript is kept in the
/// list all the same, with no file, and the failure names its entry, to be saved elsewhere.
#[test]
fn a_transcript_whose_file_cannot_be_written_is_kept() {
    let mut h = harness_with(Fake { write: Err("no such folder".into()), ..fake() }, |_| {});
    h.send(file_request(modes::PLAIN, ExportFormat::Txt));
    two_voices(&mut h);
    let kept = kept(h.calls());
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].output, None);
    assert_eq!(
        file_events(&h).last(),
        Some(&FileEvent::Failed {
            file: 1,
            problem: FileProblem::Save,
            detail: "no such folder".into(),
            transcript: Some(KEPT),
            held: true,
        })
    );
}

#[test]
fn a_transcribed_file_is_kept_in_the_list_after_it_is_saved() {
    let mut h = harness();
    h.send(file_request(modes::PLAIN, ExportFormat::Txt));
    two_voices(&mut h);
    let calls = h.calls();
    let write = calls.iter().position(|call| matches!(call, Call::WriteFile(..)));
    let keep = calls.iter().position(|call| matches!(call, Call::KeepTranscript(..)));
    assert!(write.is_some() && keep > write, "kept after the file is saved: {calls:?}");
    let kept = kept(calls);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].source, PathBuf::from("talk.mp3"));
    assert_eq!(kept[0].output, Some(PathBuf::from("talk.md")));
    assert_eq!(kept[0].duration_ms, 40_000);
    assert_eq!(kept[0].processed, None);
    // Not told apart: one unnamed speaker, turns split by the pause.
    assert_eq!(
        turn_texts(&kept[0]),
        [(Speaker::Others, "Hello there.".to_string()), (Speaker::Others, "Hi.".to_string())]
    );
    assert!(matches!(
        file_events(&h).last(),
        Some(FileEvent::Finished { transcript: Some(KEPT), .. })
    ));
}

#[test]
fn a_file_told_apart_by_voice_is_kept_by_speaker() {
    let mut h = harness();
    h.send(with_speakers(modes::PLAIN, ExportFormat::Txt));
    two_voices(&mut h);
    h.send(Message::FileDiarized { file: F1, result: Ok(segments()) });
    let kept = kept(h.calls());
    assert_eq!(
        turn_texts(&kept[0]),
        [(Speaker::Person(1), "Hello there.".to_string()), (Speaker::Person(2), "Hi.".to_string())]
    );
}

#[test]
fn the_processed_text_is_kept_with_the_transcript() {
    let mut h = harness_with(fake(), with_email);
    h.send(file_request("email", ExportFormat::Md { title: "talk".into() }));
    h.send(Message::EngineReady);
    send_window(&mut h, window(0, 0.0, 10.0, true));
    let words = words_json(&[(" Hello.", 1.0, 1.5)]);
    h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(words) });
    h.send(Message::FileProcessed { file: F1, result: Ok("  Dear team.\n".into()) });
    let kept = kept(h.calls());
    assert_eq!(kept[0].processed.as_deref(), Some("Dear team."));
    assert_eq!(turn_texts(&kept[0]), [(Speaker::Others, "Hello.".to_string())]);
}

#[test]
fn a_text_file_is_not_kept() {
    let mut h = harness_with(fake(), with_email);
    h.send(Message::StartFile(Box::new(FileRequest {
        source: FileSource::Text("Notes from the call".into()),
        name: "notes.txt".into(),
        output: PathBuf::from("notes.md"),
        format: ExportFormat::Md { title: "notes".into() },
        mode_id: "email".into(),
        replacements: false,
        speakers: None,
        reply: None,
    })));
    h.send(Message::FileProcessed { file: F1, result: Ok("Dear team.".into()) });
    let calls = h.calls();
    assert!(calls.iter().any(|call| matches!(call, Call::WriteFile(..))), "{calls:?}");
    assert!(kept(calls).is_empty());
    assert!(matches!(file_events(&h).last(), Some(FileEvent::Finished { transcript: None, .. })));
}

#[test]
fn a_file_is_saved_even_when_its_transcript_cannot_be_kept() {
    let fake = Fake { keep: Err("meetings database error: DatabaseBusy".into()), ..fake() };
    let mut h = harness_with(fake, |_| {});
    h.send(file_request(modes::PLAIN, ExportFormat::Txt));
    two_voices(&mut h);
    assert!(h.calls().iter().any(|call| matches!(call, Call::WriteFile(..))));
    assert_eq!(
        file_events(&h).last(),
        Some(&FileEvent::Finished {
            file: 1,
            path: "talk.md".into(),
            processing_error: None,
            transcript: None
        })
    );
}

fn track_request(reply: Sender<TrackUpdate>) -> Message {
    let Message::StartFile(mut request) = file_request(modes::PLAIN, ExportFormat::Txt) else {
        unreachable!()
    };
    request.reply = Some(reply);
    Message::StartFile(request)
}

#[test]
fn a_meeting_track_returns_its_words_without_file_events() {
    let (reply, updates) = std::sync::mpsc::channel();
    let mut h = harness_with(fake(), with_ai);
    h.send(track_request(reply));
    send_window(&mut h, window(0, 0.0, 10.0, true));
    h.send(Message::EngineReady);
    let words = [(" Спросим", 1.0, 1.5), (" эй", 1.6, 1.8), (" ай.", 1.8, 2.0)];
    h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(words_json(&words)) });
    let calls = h.calls();
    assert!(calls.contains(&Call::StopEngine), "the engine is released");
    assert!(!calls.iter().any(|call| matches!(call, Call::WriteFile(..))), "nothing is saved");
    assert!(file_events(&h).is_empty(), "the file view hears nothing");
    let updates: Vec<TrackUpdate> = updates.try_iter().collect();
    assert_eq!(updates[0], TrackUpdate::Progress { done_ms: 10_000, total_ms: Some(40_000) });
    let TrackUpdate::Done(words) = &updates[1] else { panic!("{updates:?}") };
    // The replacements apply, as to a file.
    assert_eq!(crate::files::join_text(words), "Спросим AI.");
    assert_eq!(updates.len(), 2);
}

#[test]
fn a_silent_meeting_track_is_done_with_no_words() {
    let (reply, updates) = std::sync::mpsc::channel();
    let mut h = harness();
    h.send(track_request(reply));
    send_window(&mut h, window(0, 0.0, 10.0, true));
    h.send(Message::EngineReady);
    h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(words_json(&[])) });
    assert_eq!(updates.try_iter().last(), Some(TrackUpdate::Done(Vec::new())));
    assert!(file_events(&h).is_empty());
}

/// Credits Whisper invents on a silent window are no words, in a meeting track as in a file;
/// an engine that does not invent them keeps such a phrase.
#[test]
fn invented_credits_in_a_window_are_no_words() {
    let credits = words_json(&[(" Продолжение", 1.0, 1.5), (" следует...", 1.5, 2.0)]);
    let (reply, updates) = std::sync::mpsc::channel();
    let mut h = harness();
    h.send(track_request(reply));
    send_window(&mut h, window(0, 0.0, 10.0, true));
    h.send(Message::EngineReady);
    h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(credits.clone()) });
    assert_eq!(updates.try_iter().last(), Some(TrackUpdate::Done(Vec::new())));

    let (reply, updates) = std::sync::mpsc::channel();
    let mut h = streaming_harness(fake(), |_| {});
    h.send(track_request(reply));
    send_window(&mut h, window(0, 0.0, 10.0, true));
    h.send(Message::EngineReady);
    h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(credits) });
    let Some(TrackUpdate::Done(words)) = updates.try_iter().last() else { panic!("not done") };
    assert_eq!(crate::files::join_text(&words), "Продолжение следует...");
}

/// Only a window's own part is compared: the words it shares with the window before do not hide
/// credits after them, and they stay for the seam.
#[test]
fn invented_credits_after_shared_words_go_too() {
    let mut h = harness();
    h.send(file_request(modes::PLAIN, ExportFormat::Txt));
    send_window(&mut h, window(0, 0.0, 27.0, false));
    h.send(Message::EngineReady);
    // It starts 2 s before its own part: the end of the first window is heard again.
    let second = Window {
        index: 1,
        start: 25 * 16_000,
        end: 40 * 16_000,
        keep_from: 27.0,
        keep_to: 40.0,
        last: true,
    };
    send_window(&mut h, second);
    let first = words_json(&[(" Hello", 24.0, 25.0), (" world.", 25.5, 26.5)]);
    h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(first) });
    let credits =
        words_json(&[(" world.", 0.5, 1.5), (" Продолжение", 5.0, 5.5), (" следует...", 5.5, 6.0)]);
    h.send(Message::ChunkTranscribed { file: F1, chunk: 1, result: Ok(credits) });
    let calls = h.calls();
    assert!(
        calls.contains(&Call::WriteFile("talk.md".into(), "Hello world.\n".into())),
        "{calls:?}"
    );
}

#[test]
fn a_meeting_track_hears_of_cancelling_and_of_a_busy_engine() {
    let (reply, updates) = std::sync::mpsc::channel();
    let mut h = harness();
    h.send(file_request(modes::PLAIN, ExportFormat::Txt));
    h.send(track_request(reply.clone()));
    assert!(matches!(
        updates.try_recv(),
        Ok(TrackUpdate::Failed { problem: FileProblem::Busy, .. })
    ));
    // Cancelling a meeting track leaves the user's file alone.
    h.send(Message::CancelTrack);
    assert!(!file_events(&h).iter().any(|event| matches!(event, FileEvent::Cancelled { .. })));
    h.send(Message::CancelFile);
    h.send(track_request(reply));
    // And the file view's cancel leaves the meeting track alone.
    h.send(Message::CancelFile);
    assert!(updates.try_iter().all(|update| update != TrackUpdate::Cancelled));
    h.send(Message::CancelTrack);
    assert_eq!(updates.try_iter().last(), Some(TrackUpdate::Cancelled));
    // Only the file's own events reached the interface.
    assert!(file_events(&h).iter().all(|event| match event {
        FileEvent::Started { file, .. } | FileEvent::Cancelled { file } => *file == 1,
        _ => false,
    }));
}

#[test]
fn file_words_get_the_replacements_unless_turned_off() {
    for (on, expected) in [(true, "Спросим AI.\n"), (false, "Спросим эй ай.\n")] {
        let mut h = harness_with(fake(), with_ai);
        let Message::StartFile(mut request) = file_request(modes::PLAIN, ExportFormat::Txt) else {
            unreachable!()
        };
        request.replacements = on;
        h.send(Message::StartFile(request));
        send_window(&mut h, window(0, 0.0, 10.0, true));
        h.send(Message::EngineReady);
        let words = [(" Спросим", 1.0, 1.5), (" эй", 1.6, 1.8), (" ай.", 1.8, 2.0)];
        h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(words_json(&words)) });
        let write = Call::WriteFile("talk.md".into(), expected.into());
        assert!(h.calls().contains(&write), "replacements {on}");
    }
}

#[test]
fn dictation_goes_before_the_next_window() {
    let mut h = harness();
    h.send(file_request(modes::PLAIN, ExportFormat::Txt)).send(Message::EngineReady);
    send_window(&mut h, window(0, 0.0, 27.0, false));
    h.calls();
    h.press();
    send_window(&mut h, window(1, 27.0, 40.0, true));
    h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(words_json(&[])) });
    assert!(!h.calls().iter().any(|call| matches!(call, Call::TranscribeChunk(1, _))));
    h.press();
    let calls = h.calls();
    assert!(calls.contains(&Call::Transcribe(S1)), "the finished recording goes first");
    assert!(!calls.iter().any(|call| matches!(call, Call::TranscribeChunk(1, _))));
    h.transcribed(S1, "hi");
    assert!(h.calls().contains(&Call::TranscribeChunk(1, false)), "then the file goes on");
}

#[test]
fn a_second_file_waits_its_turn_and_failures_free_the_engine() {
    let mut h = harness();
    h.send(file_request(modes::PLAIN, ExportFormat::Srt)).send(Message::EngineReady);
    h.send(file_request(modes::PLAIN, ExportFormat::Srt));
    assert!(file_events(&h).iter().any(|event| matches!(
        event,
        FileEvent::Failed { file: 2, problem: FileProblem::Busy, .. }
    )));
    h.calls();
    h.send(Message::FileReadFailed { file: F1, detail: "unsupported audio codec opus".into() });
    let calls = h.calls();
    assert!(calls.contains(&Call::CloseFile(F1)) && calls.contains(&Call::StopEngine));
    assert!(matches!(
        file_events(&h).last(),
        Some(FileEvent::Failed { file: 1, problem: FileProblem::Read, .. })
    ));
}

fn with_speakers(mode_id: &str, format: ExportFormat) -> Message {
    let Message::StartFile(mut request) = file_request(mode_id, format) else { unreachable!() };
    request.speakers =
        Some(Labels { me: "Me".into(), others: "Others".into(), person: "Speaker {n}".into() });
    Message::StartFile(request)
}

/// Two voices: "Hello there." then "Hi.".
fn two_voices(h: &mut Harness) {
    send_window(h, window(0, 0.0, 10.0, true));
    h.send(Message::EngineReady);
    let words = [(" Hello", 1.0, 1.4), (" there.", 1.4, 1.8), (" Hi.", 4.0, 4.4)];
    h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(words_json(&words)) });
}

fn segments() -> Vec<SpeakerSegment> {
    vec![
        SpeakerSegment { start: 0.5, end: 2.0, speaker: 4 },
        SpeakerSegment { start: 3.5, end: 5.0, speaker: 1 },
    ]
}

#[test]
fn a_file_is_saved_by_speaker() {
    let mut h = harness();
    h.send(with_speakers(modes::PLAIN, ExportFormat::Txt));
    two_voices(&mut h);
    let calls = h.calls();
    assert!(calls.contains(&Call::Diarize(F1)), "{calls:?}");
    assert!(calls.contains(&Call::StopEngine), "the engine is free while voices are told apart");
    assert!(!calls.iter().any(|call| matches!(call, Call::WriteFile(..))));
    let speakers =
        |step, done_ms| FileEvent::Speakers { file: 1, step, done_ms, total_ms: Some(40_000) };
    // First where voices speak, with no measure of how far.
    assert!(file_events(&h).contains(&speakers(SpeakersStep::Segments, 0)));
    // Then whose they are: how far, as the share of the file's length.
    h.send(Message::FileDiarizing { file: F1, share: 0.25 });
    assert_eq!(file_events(&h).last(), Some(&speakers(SpeakersStep::Voices, 10_000)));
    h.send(Message::FileDiarized { file: F1, result: Ok(segments()) });
    let expected = "[00:00:01] Speaker 1: Hello there.\n\n[00:00:04] Speaker 2: Hi.\n\n";
    assert!(h.calls().contains(&Call::WriteFile("talk.md".into(), expected.into())));
}

#[test]
fn a_language_model_failure_keeps_the_speakers_in_the_file() {
    let mut h = harness_with(fake(), with_email);
    h.send(with_speakers("email", ExportFormat::Md { title: "talk".into() }));
    two_voices(&mut h);
    h.send(Message::FileDiarized { file: F1, result: Ok(segments()) });
    h.send(Message::FileProcessed { file: F1, result: Err("HTTP 500".into()) });
    let written = h.calls().into_iter().find_map(|call| match call {
        Call::WriteFile(_, text) => Some(text),
        _ => None,
    });
    let written = written.expect("the transcript is saved");
    assert!(written.contains("**Speaker 1**"), "{written}");
    assert!(written.contains("**Speaker 2**"), "{written}");
}

#[test]
fn a_failed_or_single_voice_file_is_saved_as_before() {
    for result in [Err("cannot load the speaker models".to_string()), Ok(Vec::new())] {
        let mut h = harness();
        h.send(with_speakers(modes::PLAIN, ExportFormat::Txt));
        two_voices(&mut h);
        h.calls();
        h.send(Message::FileDiarized { file: F1, result });
        let write = Call::WriteFile("talk.md".into(), "Hello there.\n\nHi.\n".into());
        assert!(h.calls().contains(&write));
    }
}

#[test]
fn the_language_model_gets_the_speakers() {
    let mut h = harness_with(fake(), with_email);
    h.send(with_speakers("email", ExportFormat::Md { title: "talk".into() }));
    two_voices(&mut h);
    h.send(Message::FileDiarized { file: F1, result: Ok(segments()) });
    let text = "Speaker 1: Hello there.\n\nSpeaker 2: Hi.";
    assert!(h.calls().contains(&Call::ProcessFile(text.into())));
}

#[test]
fn a_file_without_the_option_is_not_told_apart() {
    let mut h = harness();
    h.send(file_request(modes::PLAIN, ExportFormat::Txt));
    two_voices(&mut h);
    assert!(!h.calls().contains(&Call::Diarize(F1)));
}

#[test]
fn a_language_model_failure_saves_the_transcript() {
    let mut h = harness_with(fake(), with_email);
    h.send(file_request("email", ExportFormat::Md { title: "talk".into() }));
    h.send(Message::EngineReady);
    send_window(&mut h, window(0, 0.0, 10.0, true));
    h.send(Message::ChunkTranscribed {
        file: F1,
        chunk: 0,
        result: Ok(words_json(&[(" Hello.", 1.0, 1.5)])),
    });
    assert!(h.calls().contains(&Call::ProcessFile("Hello.".into())));
    h.send(Message::FileProcessed { file: F1, result: Err("HTTP 500".into()) });
    assert!(h.calls().contains(&Call::WriteFile("talk.md".into(), "# talk\n\nHello.\n".into())));
    assert_eq!(
        file_events(&h).last(),
        Some(&FileEvent::Finished {
            file: 1,
            path: "talk.md".into(),
            processing_error: Some("HTTP 500".into()),
            transcript: Some(KEPT)
        })
    );
}

#[test]
fn a_plain_file_is_not_cleaned_by_the_model_of_clean_text() {
    let mut h = harness_with(fake(), cleaned_by_model);
    h.send(file_request(modes::PLAIN, ExportFormat::Srt));
    h.send(Message::EngineReady);
    send_window(&mut h, window(0, 0.0, 10.0, true));
    h.send(Message::ChunkTranscribed {
        file: F1,
        chunk: 0,
        result: Ok(words_json(&[(" Hello.", 1.0, 1.5)])),
    });
    let calls = h.calls();
    assert!(!calls.iter().any(|call| matches!(call, Call::ProcessFile(..))), "{calls:?}");
    assert!(calls.iter().any(|call| matches!(call, Call::WriteFile(..))), "{calls:?}");
}

#[test]
fn cancelling_a_file_stops_it() {
    let mut h = harness();
    h.send(file_request(modes::ENGLISH, ExportFormat::Txt)).send(Message::EngineReady);
    send_window(&mut h, window(0, 0.0, 27.0, false));
    assert!(h.calls().contains(&Call::TranscribeChunk(0, true)), "English mode translates");
    h.send(Message::CancelFile);
    let calls = h.calls();
    assert!(calls.contains(&Call::CloseFile(F1)));
    assert_eq!(file_events(&h).last(), Some(&FileEvent::Cancelled { file: 1 }));
    assert!(!calls.contains(&Call::StopEngine), "the running chunk still holds the engine");
    // The answer for the running chunk frees the engine and its slot.
    h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(words_json(&[])) });
    assert!(h.calls().contains(&Call::StopEngine), "nothing holds the engine any more");
    h.send(Message::EngineStopped).press().send(Message::EngineReady).press();
    assert!(h.calls().contains(&Call::Transcribe(S1)), "the engine serves dictation again");
}

/// Inserted dictations count with their words and recording time, the history off as by
/// default; a dictation that was not inserted does not.
#[test]
fn inserted_dictations_count_in_the_statistics() {
    let fake = fake();
    let counted = Arc::clone(&fake.counted);
    let mut h = harness_with(fake, |_| {});
    h.press().send(Message::EngineReady).press();
    h.transcribed(S1, "Встреча в 15:30, отлично");
    h.send(Message::Inserted { session: S1, result: Ok(()) });
    assert_eq!(*counted.lock().unwrap(), vec![(4, Duration::from_secs(2))]);
    assert!(h.events().contains(&CoreEvent::Dictation(DictationEvent::StatsChanged)));

    h.press().press();
    h.transcribed(S2, "не вставилось");
    let blocked = Err(InsertError::Blocked);
    h.send(Message::Inserted { session: S2, result: blocked });
    assert_eq!(counted.lock().unwrap().len(), 1, "a failed insertion does not count");
}

#[test]
fn history_gets_finished_dictations_only_when_on() {
    let mut off = harness();
    off.press().send(Message::EngineReady).press().transcribed(S1, "hello");
    off.send(Message::Inserted { session: S1, result: Ok(()) });
    assert!(!off.calls().iter().any(|call| matches!(call, Call::History(..))));

    let failing = Fake { history: Err("disk full".into()), ..fake() };
    let mut on = harness_with(failing, |s| {
        s.history_enabled = true;
        s.history_limit = Some(5);
    });
    on.press().send(Message::EngineReady).press().transcribed(S1, "hello");
    // A failed insertion still goes to the history.
    on.send(Message::Inserted { session: S1, result: Err(InsertError::Blocked) });
    assert!(on.calls().contains(&Call::History("hello".into(), Some(5))));
    assert!(on.events().contains(&CoreEvent::Dictation(DictationEvent::HistoryFailed {
        detail: "disk full".into()
    })));
}

#[test]
fn recording_waits_for_the_engine() {
    let mut h = harness();
    h.press().press();
    assert!(!h.calls().contains(&Call::Transcribe(S1)), "engine still loading");
    h.send(Message::EngineReady);
    assert_eq!(h.calls(), vec![Call::Transcribe(S1)]);
}

#[test]
fn short_recording_is_discarded_and_stops_the_load() {
    let mut h = harness_with(Fake { seconds: 0.05, ..fake() }, |_| {});
    h.press().press();
    assert_eq!(h.state_of(S1), Some(SessionState::Discarded { reason: DiscardReason::TooShort }));
    let calls = h.calls();
    assert!(calls.contains(&Call::StopEngine));
    assert!(!calls.iter().any(|call| matches!(call, Call::Transcribe(_))));
}

#[test]
fn idle_policy_unloads_after_the_timer() {
    let mut h = harness_with(fake(), |s| s.unload_policy = UnloadPolicy::AfterIdle { minutes: 20 });
    h.press().send(Message::EngineReady).press().transcribed(S1, "hi");
    assert!(!h.calls().contains(&Call::StopEngine));
    let wake = h.controller.next_wake().expect("idle timer");
    assert!(wake > Instant::now() + Duration::from_secs(19 * 60));
    h.controller.tick(wake);
    assert_eq!(h.calls(), vec![Call::StopEngine]);
}

#[test]
fn never_policy_keeps_the_engine() {
    let mut h = harness_with(fake(), |s| s.unload_policy = UnloadPolicy::Never);
    h.press().send(Message::EngineReady).press().transcribed(S1, "hi");
    assert!(!h.calls().contains(&Call::StopEngine));
    assert_eq!(h.controller.next_wake(), None);
}

#[test]
fn engine_failure_fails_the_dictation() {
    let mut h = harness();
    h.press();
    h.send(Message::EngineFailed("model is broken".into()));
    h.press();
    assert!(matches!(
        h.state_of(S1),
        Some(SessionState::Failed { failure }) if failure.kind == FailureKind::Transcription
    ));
    assert!(!h.calls().iter().any(|call| matches!(call, Call::Transcribe(_))));
    // Its recording is kept, at the microphone's rate, to be recognized again.
    let failed = h.controller.failed.as_ref().expect("a recording is kept");
    assert_eq!(failed.session, S1);
    assert_eq!(failed.audio.rate, 16_000);
    assert!(!failed.audio.samples.is_empty());
    assert_eq!(retry_events(&h), [true]);
    // The next dictation tries to load the engine again.
    h.press();
    assert!(h.calls().contains(&Call::StartEngine));
    // Stopped before that load ends, it waits for the engine: the old error is not its own.
    h.press();
    assert_eq!(h.state_of(S2), Some(SessionState::Transcribing));
    h.send(Message::EngineReady);
    assert!(h.calls().iter().any(|call| matches!(call, Call::Transcribe(_))));
}

/// The engine failed to load while a dictation records; another model is chosen: it loads at
/// once, and the dictation is recognized by it, not failed with the old error.
#[test]
fn another_model_after_a_failed_load_recognizes_the_recording() {
    let mut h = harness();
    h.press();
    h.send(Message::EngineFailed("model is broken".into()));
    h.calls();
    let mut other = h.controller.settings.clone();
    other.model_id = Some("another".into());
    h.send(Message::Settings(Box::new(other)));
    assert!(h.calls().contains(&Call::StartEngine), "the new model loads now");
    h.press().send(Message::EngineReady);
    assert!(h.calls().contains(&Call::Transcribe(S1)));
}

#[test]
fn waiting_audio_fails_when_the_engine_fails() {
    let mut h = harness();
    h.press().press();
    h.send(Message::EngineFailed("no GPU".into()));
    assert!(matches!(h.state_of(S1), Some(SessionState::Failed { .. })));
}

#[test]
fn a_stale_engine_failure_does_not_fail_what_waits_for_the_restart() {
    let mut h = harness();
    // A finished dictation: with "unload at once" the engine is being stopped.
    h.press().send(Message::EngineReady).press().transcribed(S1, "first");
    h.send(Message::Inserted { session: S1, result: Ok(()) });
    assert_eq!(h.controller.engine.state(), EngineState::Unloading);
    // The next one is recorded and waits; a failure of the old process arrives late.
    h.press().press();
    h.send(Message::EngineFailed("of the process being stopped".into()));
    assert_eq!(h.state_of(S2), Some(SessionState::Transcribing));
    // The engine starts again and serves it.
    h.send(Message::EngineStopped).send(Message::EngineReady);
    assert!(matches!(h.calls().last(), Some(Call::Transcribe(_))));
}

#[test]
fn insertion_failure_keeps_the_last_transcription() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press().transcribed(S1, "keep me");
    h.send(Message::Inserted { session: S1, result: Err(InsertError::Blocked) });
    assert!(matches!(
        h.state_of(S1),
        Some(SessionState::Failed { failure }) if failure.kind == FailureKind::InsertionBlocked
    ));
    let last = h.last.lock().unwrap().clone().expect("kept");
    assert_eq!(last.text(), "keep me");
    assert!(!last.inserted);
}

#[test]
fn missing_permission_is_its_own_failure() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press().transcribed(S1, "keep me");
    h.send(Message::Inserted { session: S1, result: Err(InsertError::NoPermission) });
    assert!(matches!(
        h.state_of(S1),
        Some(SessionState::Failed { failure }) if failure.kind == FailureKind::NoPermission
    ));
    assert_eq!(h.last.lock().unwrap().clone().expect("kept").text(), "keep me");
}

#[test]
fn selection_without_permission_is_reported_as_such() {
    assert_eq!(selection_problem(&SelectionError::NoPermission), SelectionProblem::NoPermission);
    assert_eq!(
        selection_problem(&SelectionError::Insert(InsertError::NoPermission)),
        SelectionProblem::NoPermission
    );
}

#[test]
fn second_dictation_while_the_first_is_transcribed() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press();
    h.press(); // second recording starts while S1 is being transcribed
    assert_eq!(
        h.calls(),
        vec![
            Call::StartRecording,
            Call::StartEngine,
            Call::Transcribe(S1),
            Call::Unmute,
            Call::StartRecording
        ]
    );
    h.transcribed(S1, "first");
    assert_eq!(
        h.calls(),
        vec![Call::Insert(S1, "first".into(), Some(TARGET))],
        "engine still held by S2"
    );
    h.press();
    assert_eq!(h.calls(), vec![Call::Transcribe(S2), Call::Unmute]);
    h.transcribed(S2, "second");
    assert_eq!(h.calls(), vec![Call::StopEngine], "S2 goes in after S1 is in");
    h.send(Message::Inserted { session: S1, result: Ok(()) });
    assert_eq!(h.calls(), vec![Call::Insert(S2, "second".into(), Some(TARGET))]);
}

#[test]
fn missing_model_or_engine_blocks_and_allows_retry() {
    let mut h = harness_with(Fake { model: false, ..fake() }, |_| {});
    h.press();
    h.press();
    assert_eq!(h.blocked(), vec![BlockReason::NoModel, BlockReason::NoModel]);
    assert!(h.calls().is_empty());

    let mut h = harness_with(Fake { engine: false, ..fake() }, |_| {});
    h.press();
    assert_eq!(h.blocked(), vec![BlockReason::NoEngine]);
}

fn groq() -> Connection {
    Connection {
        id: "groq-1".into(),
        name: "Groq".into(),
        provider: ProviderKind::OpenAiCompatible,
        base_url: Some(GROQ_BASE_URL.into()),
        key_hint: Some("••••abcd".into()),
        added_at_ms: 0,
        models: vec!["llama".into()],
    }
}

/// The cloud engine's service gets another connection (a new key): the running engine starts
/// again with it instead of going on with the old key.
#[test]
fn a_new_connection_of_the_cloud_engine_starts_it_again() {
    let fake = Fake { model: false, engine: false, ..fake() };
    let mut h = harness_with(fake, |s| {
        s.model_id = Some("groq-whisper-large-v3".into());
        s.connections = vec![groq()];
        s.unload_policy = UnloadPolicy::Never;
    });
    h.press().send(Message::EngineReady).press();
    h.transcribed(S1, "hello");
    h.calls();
    let mut settings = h.controller.settings.clone();
    settings.connections = vec![Connection { id: "groq-2".into(), ..groq() }];
    h.send(Message::Settings(Box::new(settings)));
    assert!(h.calls().contains(&Call::StopEngine), "the old key's engine goes");
}

#[test]
fn cloud_engine_needs_no_model_or_program() {
    let fake = Fake { model: false, engine: false, ..fake() };
    let options = Arc::clone(&fake.options);
    let mut h = harness_with(fake, |s| {
        s.model_id = Some("groq-whisper-large-v3".into());
        s.connections = vec![groq()];
    });
    h.press_mode(modes::ENGLISH);
    assert_eq!(
        h.calls(),
        vec![Call::StartRecording, Call::StartCloud("groq-whisper-large-v3", "groq-1".into())]
    );
    h.send(Message::EngineReady).press_mode(modes::ENGLISH);
    assert_eq!(h.calls(), vec![Call::Transcribe(S1), Call::Unmute]);
    assert!(options.lock().unwrap().pop().expect("transcribed").translate);
}

#[test]
fn cloud_engine_without_its_service_blocks() {
    let mut h = harness_with(fake(), |s| s.model_id = Some("deepgram-nova-3".into()));
    h.press();
    assert_eq!(h.blocked(), vec![BlockReason::NoModel]);
    assert!(h.calls().is_empty());
}

#[test]
fn microphone_error_blocks() {
    let mut h = harness_with(Fake { microphone: Err("unplugged".into()), ..fake() }, |_| {});
    h.press();
    assert_eq!(h.blocked(), vec![BlockReason::Microphone]);
    assert!(h.calls().is_empty(), "engine not started");
}

#[test]
fn mute_while_recording() {
    let mut h = harness_with(fake(), |s| s.mute_while_recording = true);
    h.press();
    // Silent before the microphone opens, so a headset switching modes is not heard.
    assert_eq!(h.calls(), vec![Call::Mute, Call::StartRecording, Call::StartEngine]);
    h.press();
    assert_eq!(h.calls(), vec![Call::Unmute]);
}

/// A key's auto-repeat sends the press again: one release still lets it go. Settings saved
/// while it is held (its release lost to the new registration) let go of it too.
#[test]
fn a_hotkey_held_counts_once() {
    let mut h = harness();
    h.send(Message::HotkeyPressed(HotkeyAction::Main));
    h.send(Message::HotkeyPressed(HotkeyAction::Main));
    h.send(Message::HotkeyPressed(HotkeyAction::ByApp));
    assert_eq!(h.controller.held.len(), 2);
    h.send(Message::HotkeyReleased(HotkeyAction::Main));
    assert_eq!(h.controller.held.len(), 1);
    let settings = h.controller.settings.clone();
    h.send(Message::Settings(Box::new(settings)));
    assert!(h.controller.held.is_empty());
}

/// A recording started by a mode's hotkey stops only at that hotkey: settings that no longer
/// name it end the recording, which goes on to recognition. Settings that keep it leave it be.
#[test]
fn a_recording_whose_hotkey_is_gone_ends() {
    let mut h = harness_with(fake(), with_email);
    h.press_mode("email").send(Message::EngineReady);
    assert!(h.controller.recording.is_some());
    h.calls();
    let same = h.controller.settings.clone();
    h.send(Message::Settings(Box::new(same)));
    assert!(h.controller.recording.is_some(), "its hotkey is still there");
    let mut without = h.controller.settings.clone();
    without.modes.iter_mut().find(|mode| mode.id == "email").unwrap().hotkey = None;
    h.send(Message::Settings(Box::new(without)));
    assert!(h.controller.recording.is_none());
    assert!(h.calls().contains(&Call::Transcribe(S1)));
    // The main hotkey starts a new dictation as usual.
    h.press();
    assert!(h.controller.recording.is_some());
}

/// Held when its hotkey went away (no release comes), the ended recording does not keep the
/// next press of the main hotkey from starting one.
#[test]
fn a_held_hotkey_that_went_away_does_not_swallow_the_next_press() {
    let mut h = harness_with(fake(), |s| {
        with_email(s);
        s.recording_mode = RecordingMode::Hold;
    });
    h.send(Message::HotkeyPressed(HotkeyAction::Dictate { mode_id: "email".into() }));
    assert!(h.controller.recording.is_some());
    let mut without = h.controller.settings.clone();
    without.modes.iter_mut().find(|mode| mode.id == "email").unwrap().hotkey = None;
    h.send(Message::Settings(Box::new(without)));
    assert!(h.controller.recording.is_none());
    h.send(Message::HotkeyPressed(HotkeyAction::Main));
    assert!(h.controller.recording.is_some(), "the main hotkey starts a dictation");
}

/// The "by application" hotkey cleared while it records: the recording ends.
#[test]
fn a_cleared_by_app_hotkey_ends_its_recording() {
    let mut h = harness_with(fake(), |s| s.app_hotkey = Some("control+alt+KeyA".into()));
    h.press_by_app().send(Message::EngineReady);
    assert!(h.controller.recording.is_some());
    let mut without = h.controller.settings.clone();
    without.app_hotkey = None;
    h.send(Message::Settings(Box::new(without)));
    assert!(h.controller.recording.is_none());
}

/// Recording with the translation hotkey, a model that does not translate is chosen: its hotkey
/// goes, the recording ends, the old engine stops and the new one recognizes the recording.
#[test]
fn a_model_without_translation_ends_a_translation_recording() {
    let mut h = harness_with(fake(), |s| {
        s.modes.iter_mut().find(|mode| mode.id == "english").unwrap().hotkey =
            Some("control+alt+KeyT".into());
    });
    h.press_mode("english").send(Message::EngineReady);
    assert!(h.controller.recording.is_some());
    h.calls();
    let mut other = h.controller.settings.clone();
    other.model_id = Some("gigaam-v3-e2e-rnnt".into());
    h.send(Message::Settings(Box::new(other)));
    assert!(h.controller.recording.is_none());
    // Nothing was at the old engine: it stops, and the new one recognizes the recording.
    assert!(h.calls().contains(&Call::StopEngine));
    h.send(Message::EngineStopped);
    assert_eq!(h.calls(), vec![Call::StartEngine]);
    h.send(Message::EngineReady);
    assert!(h.calls().contains(&Call::Transcribe(S1)));
}

/// Another engine chosen while a file holds the old one: the window at the old engine
/// finishes there, and nothing else goes to it — the next dictation and the rest of the file
/// wait for the new engine (audio goes only to the engine the user chose).
#[test]
fn work_after_a_model_change_waits_for_the_new_engine() {
    let mut h = harness_with(fake(), |s| s.unload_policy = UnloadPolicy::Never);
    h.send(file_request(modes::PLAIN, ExportFormat::Txt)).send(Message::EngineReady);
    send_window(&mut h, window(0, 0.0, 27.0, false));
    assert!(h.calls().contains(&Call::TranscribeChunk(0, false)));
    let mut other = h.controller.settings.clone();
    other.model_id = Some("another".into());
    h.send(Message::Settings(Box::new(other)));
    assert!(h.calls().is_empty(), "the window at the engine finishes there");
    h.press().press();
    send_window(&mut h, window(1, 27.0, 40.0, true));
    assert!(!h.calls().iter().any(|call| matches!(call, Call::Transcribe(_))));
    h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(words_json(&[])) });
    let calls = h.calls();
    assert!(calls.contains(&Call::StopEngine), "{calls:?}");
    assert!(
        !calls.iter().any(|call| matches!(call, Call::Transcribe(_) | Call::TranscribeChunk(..)))
    );
    h.send(Message::EngineStopped);
    assert!(h.calls().contains(&Call::StartEngine), "the new engine");
    h.send(Message::EngineReady);
    assert!(h.calls().contains(&Call::Transcribe(S1)), "the dictation first");
    h.transcribed(S1, "hi");
    assert!(h.calls().contains(&Call::TranscribeChunk(1, false)), "then the rest of the file");
}

/// Another engine chosen while a file is transcribed, one that cannot go on with it: the file
/// stops at once and says why — no word times for a media file, no translation for one being
/// translated — and nothing more of it goes to an engine.
#[test]
fn a_model_that_cannot_go_on_with_a_file_stops_it() {
    for (mode, model, problem) in [
        (modes::PLAIN, "openai-gpt-transcribe", FileProblem::NoWordTimes),
        (modes::ENGLISH, "gigaam-v3-e2e-rnnt", FileProblem::NoTranslation),
    ] {
        let mut h = harness_with(fake(), |s| s.unload_policy = UnloadPolicy::Never);
        h.send(file_request(mode, ExportFormat::Txt)).send(Message::EngineReady);
        send_window(&mut h, window(0, 0.0, 27.0, false));
        assert!(h.calls().iter().any(|call| matches!(call, Call::TranscribeChunk(0, _))));
        let mut other = h.controller.settings.clone();
        other.model_id = Some(model.into());
        h.send(Message::Settings(Box::new(other)));
        let failed = file_events(&h).into_iter().find_map(|event| match event {
            FileEvent::Failed { problem, detail, .. } => Some((problem, detail)),
            _ => None,
        });
        assert_eq!(failed, Some((problem, String::new())), "{model}");
        send_window(&mut h, window(1, 27.0, 40.0, true));
        h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: Ok(words_json(&[])) });
        assert!(!h.calls().iter().any(|call| matches!(call, Call::TranscribeChunk(..))));
    }
    // A model that can go on with it takes the rest: one that transcribes media, and one that
    // does not translate a file whose model did not translate either.
    for (mode, before, after) in [
        (modes::PLAIN, "model", "gigaam-v3-e2e-rnnt"),
        (modes::ENGLISH, "whisper-large-v3-turbo", "gigaam-v3-e2e-rnnt"),
    ] {
        let mut h = harness_with(fake(), |s| {
            s.unload_policy = UnloadPolicy::Never;
            s.model_id = Some(before.into());
        });
        h.send(file_request(mode, ExportFormat::Txt)).send(Message::EngineReady);
        send_window(&mut h, window(0, 0.0, 27.0, false));
        let mut other = h.controller.settings.clone();
        other.model_id = Some(after.into());
        h.send(Message::Settings(Box::new(other)));
        assert!(!file_events(&h).iter().any(|event| matches!(event, FileEvent::Failed { .. })));
    }
}

/// A meeting's track asked for after the model was changed to one without word times is
/// refused at once with that reason.
#[test]
fn a_track_for_a_model_without_word_times_is_refused() {
    let mut h = harness_with(fake(), |s| s.model_id = Some("openai-gpt-transcribe".into()));
    h.send(file_request(modes::PLAIN, ExportFormat::Txt));
    let failed = file_events(&h).into_iter().find_map(|event| match event {
        FileEvent::Failed { problem, .. } => Some(problem),
        _ => None,
    });
    assert_eq!(failed, Some(FileProblem::NoWordTimes));
    assert!(!h.calls().iter().any(|call| matches!(call, Call::StartEngine)));
}

#[test]
fn hold_mode_records_while_held() {
    let mut h = harness_with(fake(), |s| s.recording_mode = RecordingMode::Hold);
    let plain = HotkeyAction::Main;
    h.send(Message::EngineReady); // ignored: nothing was loading
    h.send(Message::HotkeyPressed(plain.clone()));
    assert_eq!(h.calls(), vec![Call::StartRecording, Call::StartEngine]);
    h.send(Message::EngineReady).send(Message::HotkeyReleased(plain));
    assert_eq!(h.calls(), vec![Call::Transcribe(S1), Call::Unmute]);
}

fn connection() -> crate::domain::modes::Connection {
    crate::domain::modes::Connection {
        id: "c1".into(),
        name: "Claude".into(),
        provider: crate::domain::modes::ProviderKind::Anthropic,
        base_url: None,
        key_hint: None,
        added_at_ms: 0,
        models: vec!["claude".into()],
    }
}

/// Settings with an "email" language-model mode.
fn with_email(settings: &mut Settings) {
    settings.connections = vec![connection()];
    settings.modes.push(Mode {
        id: "email".into(),
        name: "Email".into(),
        action: ModeAction::Process {
            connection_id: "c1".into(),
            model: "claude".into(),
            instruction: "Make it an email".into(),
            output_language: None,
            context: ContextSources::default(),
        },
        hotkey: Some("control+alt+KeyE".into()),
        selection_hotkey: Some("control+alt+KeyR".into()),
    });
}

#[test]
fn english_mode_asks_whisper_to_translate() {
    let fake = fake();
    let options = Arc::clone(&fake.options);
    let mut h = harness_with(fake, |_| {});
    h.press_mode(modes::ENGLISH).send(Message::EngineReady).press_mode(modes::ENGLISH);
    assert!(options.lock().unwrap().pop().expect("transcribed").translate);
}

/// A dictation with no window to type into puts nothing in, so the next one does not wait for
/// it.
#[test]
fn a_dictation_with_nowhere_to_type_does_not_hold_up_the_next() {
    let fake = fake();
    fake.focuses.lock().unwrap().push_back(None);
    let mut h = harness_with(fake, |s| {
        with_email(s);
        s.unload_policy = UnloadPolicy::Never;
    });
    h.press_mode("email").send(Message::EngineReady).press_mode("email").transcribed(S1, "raw");
    h.press().press().transcribed(S2, "second");
    let calls = h.calls();
    assert!(calls.contains(&Call::Insert(S2, "second".into(), Some(TARGET))), "{calls:?}");
}

/// A dictation waits for the language model while the next one is ready: text goes in in the
/// order of the dictations, the later one after the earlier one is in, never both at once.
#[test]
fn text_goes_in_in_the_order_of_the_dictations() {
    let mut h = harness_with(fake(), |s| {
        with_email(s);
        s.unload_policy = UnloadPolicy::Never;
    });
    h.press_mode("email").send(Message::EngineReady).press_mode("email").transcribed(S1, "raw");
    h.press().press().transcribed(S2, "second");
    let calls = h.calls();
    assert!(calls.contains(&Call::Process(S1, "raw".into())));
    assert!(!calls.iter().any(|call| matches!(call, Call::Insert(..))), "S2 waits for S1");
    h.send(Message::Processed { session: S1, result: Ok("Dear team".into()) });
    assert_eq!(h.calls(), vec![Call::Insert(S1, "Dear team".into(), Some(TARGET))]);
    h.send(Message::Inserted { session: S1, result: Ok(()) });
    assert!(h.calls().contains(&Call::Insert(S2, "second".into(), Some(TARGET))), "then S2");
    h.send(Message::Inserted { session: S2, result: Ok(()) });
    // An earlier dictation whose text cannot go in lets the next one go in too.
    h.press_mode("email").press_mode("email").transcribed(S3, "raw");
    h.press().press();
    h.transcribed(SessionId(4), "fourth");
    assert!(!h.calls().iter().any(|call| matches!(call, Call::Insert(..))));
    h.send(Message::Processed { session: S3, result: Err("401".into()) });
    let calls = h.calls();
    assert!(calls.contains(&Call::Insert(S3, "raw".into(), Some(TARGET))), "{calls:?}");
    h.send(Message::Inserted {
        session: S3,
        result: Err(crate::platform::InsertError::FocusChanged),
    });
    assert!(h.calls().contains(&Call::Insert(SessionId(4), "fourth".into(), Some(TARGET))));
}

/// The next dictation's live text waits too while an earlier one's answer is not in yet, and
/// goes on once it is: the model's answer is never typed into the middle of it.
#[test]
fn live_text_waits_for_an_earlier_dictation() {
    let mut h = streaming_harness(fake(), |s| {
        with_email(s);
        s.unload_policy = UnloadPolicy::Never;
    });
    h.press_mode("email").send(Message::EngineReady).press_mode("email").transcribed(S1, "raw");
    h.press();
    h.send(Message::LiveText { session: S2, text: "Hello".into() });
    assert!(!h.calls().iter().any(|call| matches!(call, Call::TypeLive(..))), "S1 is not in");
    h.send(Message::Processed { session: S1, result: Ok("Dear team".into()) });
    assert!(!h.calls().iter().any(|call| matches!(call, Call::TypeLive(..))), "S1 being typed");
    h.send(Message::Inserted { session: S1, result: Ok(()) });
    assert!(h.calls().contains(&Call::TypeLive(S2, "Hello".into(), Some(TARGET))));
}

#[test]
fn language_model_mode_inserts_the_processed_text() {
    let mut h = harness_with(fake(), with_email);
    h.press_mode("email").send(Message::EngineReady).press_mode("email").transcribed(S1, "raw");
    let calls = h.calls();
    assert!(calls.contains(&Call::Process(S1, "raw".into())));
    assert!(!calls.iter().any(|call| matches!(call, Call::Insert(..))), "waits for the model");
    h.send(Message::Processed { session: S1, result: Ok("Dear team".into()) });
    assert_eq!(h.calls(), vec![Call::Insert(S1, "Dear team".into(), Some(TARGET))]);
    h.send(Message::Inserted { session: S1, result: Ok(()) });
    let last = h.last.lock().unwrap().clone().unwrap();
    assert_eq!((last.transcript.as_str(), last.text()), ("raw", "Dear team"));
}

#[test]
fn language_model_failure_inserts_the_transcript() {
    let mut h = harness_with(fake(), with_email);
    h.press_mode("email").send(Message::EngineReady).press_mode("email").transcribed(S1, "raw");
    h.calls();
    h.send(Message::Processed { session: S1, result: Err("401".into()) });
    assert_eq!(h.calls(), vec![Call::Insert(S1, "raw".into(), Some(TARGET))]);
    assert!(
        h.events().iter().any(|event| matches!(
            event,
            CoreEvent::Session(SessionEvent::ProcessingFailed { .. })
        ))
    );
}

#[test]
fn a_used_up_chatgpt_plan_inserts_the_transcript() {
    let mut h = harness_with(fake(), with_email);
    h.press_mode("email").send(Message::EngineReady).press_mode("email").transcribed(S1, "raw");
    h.calls();
    let limit = ProcessingError { detail: "limit".into(), kind: ProcessingFailure::PlanLimit };
    h.send(Message::Processed { session: S1, result: Err(limit) });
    assert_eq!(h.calls(), vec![Call::Insert(S1, "raw".into(), Some(TARGET))]);
    assert!(h.events().iter().any(|event| matches!(
        event,
        CoreEvent::Session(SessionEvent::ProcessingFailed { failure, .. })
            if failure.kind == FailureKind::PlanLimit
    )));
}

#[test]
fn an_empty_answer_of_the_model_inserts_the_transcript_and_says_so() {
    let mut h = harness_with(fake(), with_email);
    h.press_mode("email").send(Message::EngineReady).press_mode("email").transcribed(S1, "raw");
    h.calls();
    let empty = ProcessingError { detail: "empty result".into(), kind: ProcessingFailure::Empty };
    h.send(Message::Processed { session: S1, result: Err(empty) });
    assert_eq!(h.calls(), vec![Call::Insert(S1, "raw".into(), Some(TARGET))]);
    assert!(h.events().iter().any(|event| matches!(
        event,
        CoreEvent::Session(SessionEvent::ProcessingFailed { failure, .. })
            if failure.kind == FailureKind::EmptyProcessing
    )));
}

fn telegram() -> AppId {
    AppId { id: "ru.keepcoder.Telegram".into(), name: "Telegram".into() }
}

/// An "email" mode the user chose for Telegram.
fn email_in_telegram(settings: &mut Settings) {
    with_email(settings);
    settings.app_rules = vec![AppRule {
        app_id: "ru.keepcoder.Telegram".into(),
        app_name: "Telegram".into(),
        mode_id: "email".into(),
    }];
}

fn in_app(app: AppId) -> Fake {
    Fake { app: Some(app), ..fake() }
}

fn mode_by_app(h: &Harness) -> Vec<String> {
    h.events()
        .into_iter()
        .filter_map(|event| match event {
            CoreEvent::Dictation(DictationEvent::ModeByApp { mode_id, .. }) => Some(mode_id),
            _ => None,
        })
        .collect()
}

#[test]
fn the_by_app_hotkey_takes_the_mode_chosen_for_the_program() {
    let mut h = harness_with(in_app(telegram()), email_in_telegram);
    h.press_by_app().send(Message::EngineReady).press_by_app().transcribed(S1, "raw");
    assert!(h.calls().contains(&Call::Process(S1, "raw".into())), "the email mode runs");
    assert_eq!(mode_by_app(&h), ["email"], "the capsule is told");
}

/// The main hotkey always gives the main mode, so the plain text is one press away also
/// where a rule is set.
#[test]
fn the_main_hotkey_ignores_the_rules() {
    let mut h = harness_with(in_app(telegram()), email_in_telegram);
    h.press().send(Message::EngineReady).press().transcribed(S1, "raw");
    assert!(h.calls().contains(&Call::Insert(S1, "raw".into(), Some(TARGET))), "plain dictation");
    assert!(mode_by_app(&h).is_empty());
}

#[test]
fn the_by_app_hotkey_keeps_the_main_mode_in_other_programs() {
    let notepad = AppId { id: "Notepad.exe".into(), name: "Notepad".into() };
    for fake in [in_app(notepad), fake()] {
        let mut h = harness_with(fake, email_in_telegram);
        h.press_by_app().send(Message::EngineReady).press_by_app().transcribed(S1, "raw");
        assert!(
            h.calls().contains(&Call::Insert(S1, "raw".into(), Some(TARGET))),
            "plain dictation"
        );
        assert!(mode_by_app(&h).is_empty());
    }
}

#[test]
fn a_mode_hotkey_keeps_its_own_mode_in_a_ruled_program() {
    let fake = in_app(telegram());
    let options = Arc::clone(&fake.options);
    let mut h = harness_with(fake, email_in_telegram);
    h.press_mode(modes::ENGLISH).send(Message::EngineReady).press_mode(modes::ENGLISH);
    assert!(options.lock().unwrap().pop().expect("transcribed").translate);
    h.transcribed(S1, "raw");
    assert!(!h.calls().iter().any(|call| matches!(call, Call::Process(..))));
    assert!(mode_by_app(&h).is_empty());
}

/// The "email" mode with the given context sources, dictated in Mail.
fn email_with_context(sources: ContextSources) -> Harness {
    let fake = Fake {
        app: Some(AppId { id: "com.apple.mail".into(), name: "Mail".into() }),
        context: Context {
            app: None,
            selection: Some("Can we meet on Thursday?".into()),
            clipboard: Some("Agenda".into()),
        },
        ..fake()
    };
    let configure = move |settings: &mut Settings| {
        with_email(settings);
        for mode in &mut settings.modes {
            if let ModeAction::Process { context, .. } = &mut mode.action {
                *context = sources;
            }
        }
    };
    harness_with(fake, configure)
}

fn model_contexts(h: &mut Harness) -> Vec<Context> {
    h.press_mode("email").send(Message::EngineReady).press_mode("email").transcribed(S1, "ok");
    let fake = &h.controller.services;
    std::mem::take(&mut *fake.contexts.lock().unwrap())
}

#[test]
fn without_sources_nothing_is_read_and_the_model_gets_no_context() {
    let mut h = email_with_context(ContextSources::default());
    assert_eq!(model_contexts(&mut h), [Context::default()]);
    assert!(!h.calls().iter().any(|call| matches!(call, Call::ReadContext(_))));
}

#[test]
fn ticked_sources_are_read_at_the_press_and_go_to_the_model() {
    let all = ContextSources { selection: true, clipboard: true, app: true };
    let mut h = email_with_context(all);
    h.send(Message::HotkeyPressed(HotkeyAction::Dictate { mode_id: "email".into() }));
    assert_eq!(h.calls().first(), Some(&Call::ReadContext(all)), "before the microphone");
    h.send(Message::HotkeyReleased(HotkeyAction::Dictate { mode_id: "email".into() }));
    h.send(Message::EngineReady).press_mode("email").transcribed(S1, "agree but friday");
    let contexts = std::mem::take(&mut *h.controller.services.contexts.lock().unwrap());
    assert_eq!(
        contexts,
        [Context {
            app: Some("Mail".into()),
            selection: Some("Can we meet on Thursday?".into()),
            clipboard: Some("Agenda".into()),
        }]
    );
}

fn context_taken(h: &Harness) -> Vec<SessionId> {
    h.events()
        .into_iter()
        .filter_map(|event| match event {
            CoreEvent::Dictation(DictationEvent::ContextTaken { session }) => Some(session),
            _ => None,
        })
        .collect()
}

#[test]
fn the_capsule_learns_when_a_source_had_text() {
    let selection = ContextSources { selection: true, ..ContextSources::default() };
    let press = HotkeyAction::Dictate { mode_id: "email".into() };
    let mut found = email_with_context(selection);
    found.send(Message::HotkeyPressed(press.clone()));
    found.send(Message::ContextRead { id: 1, found: true });
    assert_eq!(context_taken(&found), [S1]);

    let mut empty = email_with_context(selection);
    empty.send(Message::HotkeyPressed(press));
    empty.send(Message::ContextRead { id: 1, found: false });
    assert!(context_taken(&empty).is_empty(), "nothing was selected");
}

#[test]
fn the_program_name_shows_at_once_and_plain_modes_take_nothing() {
    let mut h = email_with_context(ContextSources { app: true, ..ContextSources::default() });
    h.send(Message::HotkeyPressed(HotkeyAction::Dictate { mode_id: "email".into() }));
    assert_eq!(context_taken(&h), [S1]);
    let mut plain = harness();
    plain.press();
    plain.send(Message::ContextRead { id: 0, found: true });
    assert!(context_taken(&plain).is_empty());
}

#[test]
fn the_program_name_alone_needs_no_reading() {
    let mut h = email_with_context(ContextSources { app: true, ..ContextSources::default() });
    let contexts = model_contexts(&mut h);
    assert_eq!(contexts, [Context { app: Some("Mail".into()), ..Context::default() }]);
    assert!(!h.calls().iter().any(|call| matches!(call, Call::ReadContext(_))));
}

#[test]
fn the_context_stays_out_of_the_history_and_the_last_result() {
    let all = ContextSources { selection: true, clipboard: true, app: true };
    let mut h = email_with_context(all);
    let configure_history = |h: &mut Harness| h.controller.settings.history_enabled = true;
    configure_history(&mut h);
    model_contexts(&mut h);
    h.send(Message::Processed { session: S1, result: Ok("Friday works".into()) });
    h.send(Message::Inserted { session: S1, result: Ok(()) });
    let last = h.last.lock().unwrap().clone().unwrap();
    assert_eq!((last.transcript.as_str(), last.text()), ("ok", "Friday works"));
    for call in h.calls() {
        if let Call::History(text, _) = call {
            assert!(!text.contains("Thursday") && !text.contains("Agenda"));
        }
    }
}

#[test]
fn a_rule_for_a_removed_mode_falls_back_to_the_main_mode() {
    let configure = |settings: &mut Settings| {
        email_in_telegram(settings);
        settings.modes.retain(|mode| mode.id != "email");
    };
    let mut h = harness_with(in_app(telegram()), configure);
    h.press_by_app().send(Message::EngineReady).press_by_app().transcribed(S1, "raw");
    assert!(h.calls().contains(&Call::Insert(S1, "raw".into(), Some(TARGET))));
    assert!(mode_by_app(&h).is_empty());
}

#[test]
fn a_ruled_recording_stops_only_with_the_by_app_hotkey() {
    let mut h = harness_with(in_app(telegram()), email_in_telegram);
    h.press_by_app();
    h.calls();
    h.press_mode("email");
    h.press();
    assert!(h.calls().is_empty(), "the email and the main hotkeys did not start it");
    h.press_by_app();
    assert!(h.calls().contains(&Call::Unmute), "the by-app hotkey stops it");
}

/// Held, the "by application" hotkey's own combination decides whether typing is safe.
#[test]
fn hold_mode_by_app_types_unless_its_hotkey_holds_ctrl() {
    for (hotkey, types) in [("shift+KeyZ", true), ("control+alt+KeyA", false)] {
        let mut h = streaming_harness(fake(), |s| {
            s.recording_mode = RecordingMode::Hold;
            s.app_hotkey = Some(hotkey.into());
        });
        h.send(Message::HotkeyPressed(HotkeyAction::ByApp)).send(Message::EngineReady);
        h.calls();
        live_text(&mut h, "Hello");
        assert_eq!(h.calls().contains(&Call::TypeLive(S1, "Hello".into(), Some(TARGET))), types);
    }
}

#[test]
fn programs_dictated_into_become_recent_ones() {
    let mut h = harness_with(in_app(telegram()), with_email);
    assert!(h.recent.lock().unwrap().list().is_empty());
    h.press_mode("email");
    assert_eq!(h.recent.lock().unwrap().list(), [telegram()]);
    let mut h = harness();
    h.press();
    assert!(h.recent.lock().unwrap().list().is_empty(), "this app or an unknown one");
}

#[test]
fn another_hotkey_does_not_stop_the_recording() {
    let mut h = harness_with(fake(), with_email);
    h.press();
    h.calls();
    h.press_mode("email");
    assert!(h.calls().is_empty(), "the email hotkey is ignored while plain records");
    h.press();
    assert!(h.calls().contains(&Call::Unmute), "the plain hotkey stops it");
}

#[test]
fn selected_text_is_rewritten_one_at_a_time() {
    let mut h = harness_with(fake(), with_email);
    let rewrite = HotkeyAction::Rewrite { mode_id: "email".into() };
    h.send(Message::HotkeyPressed(rewrite.clone()));
    h.send(Message::HotkeyPressed(rewrite.clone()));
    assert_eq!(h.calls(), vec![Call::Rewrite(1)], "a second press while one runs is dropped");
    h.send(Message::Rewritten { request: 1, result: Err(SelectionError::ReadOnly) });
    assert!(h.events().contains(&CoreEvent::Dictation(DictationEvent::SelectionFailed {
        problem: SelectionProblem::ReadOnly,
        detail: "the field is read-only".into(),
    })));
    h.send(Message::HotkeyPressed(rewrite));
    assert_eq!(h.calls(), vec![Call::Rewrite(2)]);
    h.send(Message::Rewritten { request: 2, result: Ok(()) });
    assert_eq!(h.events().last(), Some(&CoreEvent::Dictation(DictationEvent::SelectionDone)));
}

fn retry_events(h: &Harness) -> Vec<bool> {
    h.events()
        .into_iter()
        .filter_map(|event| match event {
            CoreEvent::Dictation(DictationEvent::RetryAvailable { available }) => Some(available),
            _ => None,
        })
        .collect()
}

/// The engine crashes while the next dictation records: it starts again, and that dictation is
/// recognized instead of failing with the crash.
#[test]
fn a_crash_while_recording_starts_the_engine_again() {
    let mut h = harness_with(fake(), |s| s.unload_policy = UnloadPolicy::Never);
    // S1 is being recognized when the engine crashes; S2 records meanwhile.
    h.press().send(Message::EngineReady).press().press();
    h.calls();
    h.send(Message::EngineCrashed("engine process exited".into()));
    assert!(h.calls().contains(&Call::StartEngine), "started again for S2");
    h.send(Message::Transcribed { session: S1, result: Err("engine process exited".into()) });
    h.send(Message::EngineReady).press();
    assert!(h.calls().contains(&Call::Transcribe(S2)), "recognized, not failed");
}

/// A cancelled file's chunk still at the engine crashes it while a dictation waits: the engine
/// starts again for the dictation instead of failing it with the crash.
#[test]
fn a_crash_by_a_cancelled_files_chunk_starts_the_engine_again() {
    let mut h = harness_with(fake(), |s| s.unload_policy = UnloadPolicy::Never);
    h.send(file_request(modes::PLAIN, ExportFormat::Txt)).send(Message::EngineReady);
    send_window(&mut h, window(0, 0.0, 27.0, false));
    h.press().send(Message::CancelFile).press();
    h.calls();
    h.send(Message::EngineCrashed("engine process exited".into()));
    assert!(h.calls().contains(&Call::StartEngine), "started again for the dictation");
    let failed = Err("engine process exited".into());
    h.send(Message::ChunkTranscribed { file: F1, chunk: 0, result: failed });
    h.send(Message::EngineReady);
    assert!(h.calls().contains(&Call::Transcribe(S1)), "recognized, not failed");
}

/// The request that crashed held the engine alone: it is not loaded again just to be let go.
#[test]
fn a_crash_of_the_only_work_leaves_the_engine_down() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press();
    h.calls();
    h.send(Message::EngineCrashed("engine process exited".into()));
    assert!(!h.calls().contains(&Call::StartEngine));
}

/// The engine gives the audio back with its error, as `engine_worker` does.
fn not_recognized(h: &mut Harness, session: SessionId, seconds: usize) {
    let audio = Captured { samples: vec![0.1; 16_000 * seconds], rate: 16_000 };
    h.send(Message::Unrecognized { session, audio });
    h.send(Message::Transcribed { session, result: Err("HTTP 503".into()) });
}

/// An engine error keeps the recording; recognized again, the text goes to the history and the
/// last result, not into a window.
#[test]
fn a_dictation_that_was_not_recognized_can_be_recognized_again() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press();
    not_recognized(&mut h, S1, 2);
    assert_eq!(retry_events(&h), [true]);
    assert!(h.controller.retry_available().load(std::sync::atomic::Ordering::Relaxed));
    // The engine is let go after the failure ("unload at once").
    assert_eq!(h.controller.engine.state(), EngineState::Unloading);
    h.calls();
    h.send(Message::RetryDictation);
    assert!(!h.calls().contains(&Call::Transcribe(S2)), "waits for the engine");
    h.send(Message::EngineStopped);
    assert!(h.calls().contains(&Call::StartEngine), "started again for the retry");
    h.send(Message::EngineReady);
    assert!(h.calls().contains(&Call::Transcribe(S2)));
    assert_eq!(retry_events(&h), [true, false]);
    let recording = CoreEvent::Session(SessionEvent::StateChanged {
        session: S2,
        state: SessionState::Recording,
    });
    assert!(!h.events().contains(&recording), "nothing is recorded again");
    h.transcribed(S2, "second try");
    assert_eq!(h.state_of(S2), Some(SessionState::Kept));
    assert!(!h.calls().iter().any(|call| matches!(call, Call::Insert(..))), "nothing typed");
    let last = h.last.lock().unwrap().clone().expect("last result");
    assert_eq!(last.text(), "second try");
    assert!(!last.inserted);
    assert_eq!(h.controller.engine.holds(), 0, "the retry let the engine go");
}

/// A retry without an engine to recognize with says so and leaves the dictation being recorded
/// as it was: its own hotkey still stops it, and the failed recording stays for later.
#[test]
fn a_retry_without_an_engine_leaves_the_recording_alone() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press();
    not_recognized(&mut h, S1, 2);
    h.press();
    assert!(h.controller.recording.is_some());
    let mut without = h.controller.settings.clone();
    without.model_id = None;
    h.send(Message::Settings(Box::new(without)));
    h.send(Message::RetryDictation);
    assert!(
        h.events()
            .iter()
            .any(|event| matches!(event, CoreEvent::Dictation(DictationEvent::Blocked { .. })))
    );
    assert!(h.controller.recording.is_some(), "still recording");
    assert!(h.controller.failed.is_some(), "kept for later");
    h.press();
    assert!(h.controller.recording.is_none(), "stopped by its hotkey");
}

/// A retry that fails again keeps the recording again, for one more try.
#[test]
fn a_retry_that_fails_again_keeps_the_recording() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press();
    not_recognized(&mut h, S1, 2);
    h.send(Message::RetryDictation).send(Message::EngineStopped).send(Message::EngineReady);
    assert!(h.calls().contains(&Call::Transcribe(S2)));
    assert!(h.controller.failed.is_none(), "taken by the retry");
    not_recognized(&mut h, S2, 2);
    let failed = h.controller.failed.as_ref().expect("kept again");
    assert_eq!(failed.session, S1, "the speech is still S1's");
    assert_eq!(failed.audio.samples.len(), 2 * 16_000);
    assert_eq!(retry_events(&h), [true, false, true]);
}

/// A retry of an older recording that fails again does not replace a newer failed one: the
/// freshest speech is kept.
#[test]
fn a_failed_retry_does_not_replace_newer_speech() {
    let mut h = harness_with(fake(), |s| s.unload_policy = UnloadPolicy::Never);
    h.press().send(Message::EngineReady).press();
    not_recognized(&mut h, S1, 1);
    h.press(); // S2 records.
    h.send(Message::RetryDictation); // S3: S1's speech again.
    h.press(); // S2 stops.
    not_recognized(&mut h, S2, 3);
    not_recognized(&mut h, S3, 1);
    let failed = h.controller.failed.as_ref().expect("a recording is kept");
    assert_eq!(failed.session, S2);
    assert_eq!(failed.audio.samples.len(), 3 * 16_000, "S2's speech");
}

/// A dictation recording at the failure does not let the failed recording go: its number was
/// taken at the press, before the failure.
#[test]
fn a_dictation_recording_at_the_failure_keeps_the_recording() {
    let mut h = harness_with(fake(), |s| s.unload_policy = UnloadPolicy::Never);
    h.press().send(Message::EngineReady).press();
    h.press(); // S2 records.
    not_recognized(&mut h, S1, 2);
    assert!(h.controller.recording.is_some(), "S2 records");
    h.press();
    h.transcribed(S2, "second");
    assert!(h.controller.failed.is_some(), "S2 was pressed before S1 failed");
}

/// Two failures: the newer recording is kept, also when the older one fails after it.
#[test]
fn the_newest_failed_recording_is_kept() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press();
    h.press().press();
    // The newer fails first, the older after it.
    not_recognized(&mut h, S2, 3);
    not_recognized(&mut h, S1, 1);
    let failed = h.controller.failed.as_ref().expect("a recording is kept");
    assert_eq!(failed.session, S2);
    assert_eq!(failed.audio.samples.len(), 3 * 16_000);
}

/// A new dictation lets the failed recording go once it gives text, not while it records or is
/// recognized: it may turn out a touch of the hotkey.
#[test]
fn a_new_dictation_lets_the_failed_recording_go() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press();
    not_recognized(&mut h, S1, 2);
    h.press();
    assert_eq!(retry_events(&h), [true], "kept while the next one records");
    h.press().send(Message::EngineStopped).send(Message::EngineReady);
    assert_eq!(retry_events(&h), [true], "kept while it is recognized");
    h.transcribed(S2, "next");
    assert_eq!(retry_events(&h), [true, false]);
    h.calls();
    h.send(Message::RetryDictation);
    assert!(!h.calls().iter().any(|call| matches!(call, Call::Transcribe(_))));
}

/// The recording passed the threshold of speech once: recognized again, it is not measured
/// again (converted to 16 kHz, a short word may measure less).
#[test]
fn a_retry_is_not_held_to_the_threshold_of_speech_again() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press();
    let audio = Captured { samples: vec![0.0; 16_000], rate: 16_000 };
    h.send(Message::Unrecognized { session: S1, audio });
    h.send(Message::Transcribed { session: S1, result: Err("HTTP 503".into()) });
    h.send(Message::RetryDictation).send(Message::EngineStopped).send(Message::EngineReady);
    assert!(h.calls().contains(&Call::Transcribe(S2)), "not discarded");
}

/// A dictation already recording when the one before failed does not let that recording go: the
/// user had no chance to recognize it again. One started after the failure does.
#[test]
fn only_a_dictation_started_after_the_failure_lets_its_recording_go() {
    let mut h = harness_with(fake(), |s| s.unload_policy = UnloadPolicy::Never);
    h.press().send(Message::EngineReady).press();
    h.press(); // S2 records while S1 is recognized.
    not_recognized(&mut h, S1, 2);
    assert_eq!(retry_events(&h), [true]);
    h.press(); // S2 goes to recognition.
    assert_eq!(retry_events(&h), [true], "S2 began before S1 failed");
    h.transcribed(S2, "second");
    assert_eq!(retry_events(&h), [true], "S2 began before S1 failed");
    h.press().press(); // S3, after the failure.
    h.transcribed(S3, "third");
    assert_eq!(retry_events(&h), [true, false]);
}

/// A new dictation whose answer turns out no speech (credits Whisper invents) leaves the failed
/// recording for a retry.
#[test]
fn a_dictation_heard_as_nothing_keeps_the_failed_recording() {
    let mut h = harness_with(fake(), |s| s.unload_policy = UnloadPolicy::Never);
    h.press().send(Message::EngineReady).press();
    not_recognized(&mut h, S1, 2);
    h.press().press();
    h.transcribed(S2, "Thank you for watching!");
    assert_eq!(h.state_of(S2), Some(SessionState::Discarded { reason: DiscardReason::NoSpeech }));
    assert_eq!(retry_events(&h), [true], "still there");
}

/// A touch of the hotkey records no speech: the failed recording stays for a retry.
#[test]
fn a_dictation_without_speech_keeps_the_failed_recording() {
    let fake = fake();
    let silent = Arc::clone(&fake.silent);
    let mut h = harness_with(fake, |_| {});
    h.press().send(Message::EngineReady).press();
    not_recognized(&mut h, S1, 2);
    silent.store(true, Ordering::Relaxed);
    h.press().press();
    assert_eq!(h.state_of(S2), Some(SessionState::Discarded { reason: DiscardReason::NoSpeech }));
    assert_eq!(retry_events(&h), [true], "still there");
}

/// A headset that went away midway: what was recorded before is recognized, and the user hears
/// why the rest is missing.
#[test]
fn a_microphone_lost_midway_keeps_the_start_and_says_so() {
    let interrupted = Some("device disconnected".to_string());
    let mut h = harness_with(Fake { interrupted, ..fake() }, |_| {});
    h.press().send(Message::EngineReady).press();
    assert!(h.calls().contains(&Call::Transcribe(S1)), "the start is recognized");
    assert!(h.events().iter().any(|event| matches!(
        event,
        CoreEvent::Dictation(DictationEvent::MicrophoneLost { session, detail })
            if *session == S1 && detail == "device disconnected"
    )));
}

#[test]
fn plain_mode_cannot_rewrite_selection() {
    let mut h = harness();
    h.send(Message::HotkeyPressed(HotkeyAction::Rewrite { mode_id: modes::PLAIN.into() }));
    assert!(h.calls().is_empty(), "nothing is rewritten");
    assert!(matches!(
        h.events().last(),
        Some(CoreEvent::Dictation(DictationEvent::SelectionFailed {
            problem: SelectionProblem::Processing,
            ..
        }))
    ));
}

#[test]
fn the_text_goes_only_to_the_window_of_the_press() {
    let mut h = harness_with(fake(), |_| {});
    h.press().send(Message::EngineReady).press().transcribed(S1, "here");
    assert!(h.calls().contains(&Call::Insert(S1, "here".into(), Some(TARGET))));

    let mut h = harness_with(Fake { focus: None, ..fake() }, |_| {});
    h.press().send(Message::EngineReady).press().transcribed(S1, "nowhere");
    assert!(matches!(
        h.state_of(S1),
        Some(SessionState::Failed { failure }) if failure.kind == FailureKind::NoFocus
    ));
}

#[test]
fn model_change_restarts_an_idle_engine() {
    let mut h = harness_with(fake(), |s| s.unload_policy = UnloadPolicy::Never);
    h.press().send(Message::EngineReady).press().transcribed(S1, "hi");
    h.calls();
    let settings = Settings {
        model_id: Some("other".into()),
        unload_policy: UnloadPolicy::Never,
        ..Settings::default()
    };
    h.send(Message::Settings(Box::new(settings)));
    assert_eq!(h.calls(), vec![Call::StopEngine]);
}

#[test]
fn level_is_reported_while_recording() {
    let mut h = harness();
    assert_eq!(h.controller.next_wake(), None);
    h.press();
    assert!(h.controller.next_wake().is_some());
    h.controller.tick(Instant::now());
    assert!(
        h.events
            .lock()
            .unwrap()
            .contains(&CoreEvent::Dictation(DictationEvent::Level { permille: 250 }))
    );
}

#[test]
fn silence_is_not_sent_to_the_engine() {
    let mut h = harness_with(Fake { silent: Arc::new(AtomicBool::new(true)), ..fake() }, |_| {});
    h.press().send(Message::EngineReady).press();
    assert_eq!(h.state_of(S1), Some(SessionState::Discarded { reason: DiscardReason::NoSpeech }));
    let calls = h.calls();
    assert!(!calls.iter().any(|call| matches!(call, Call::Transcribe(_))));
    assert!(calls.contains(&Call::StopEngine), "the hold is released");
}

#[test]
fn invented_credits_count_as_no_speech() {
    let mut h = harness();
    h.press().send(Message::EngineReady).press().transcribed(S1, "Thank you for watching!");
    assert_eq!(h.state_of(S1), Some(SessionState::Discarded { reason: DiscardReason::NoSpeech }));
    assert!(!h.calls().iter().any(|call| matches!(call, Call::Insert(..))));
    assert!(h.last.lock().unwrap().is_none());
}

/// The answer comes from the engine started, not from the one chosen since: Whisper's credits go
/// even when a streaming model was chosen meanwhile, and the other way round they stay.
#[test]
fn credits_follow_the_engine_that_answered() {
    let whisper = fake();
    let streaming = Arc::clone(&whisper.streaming);
    let mut h = harness_with(whisper, |s| s.unload_policy = UnloadPolicy::Never);
    h.press().send(Message::EngineReady).press();
    streaming.store(true, Ordering::Relaxed); // Another model is chosen; Whisper still runs.
    h.transcribed(S1, "Thank you for watching!");
    assert_eq!(h.state_of(S1), Some(SessionState::Discarded { reason: DiscardReason::NoSpeech }));

    let nemotron = Fake { streaming: Arc::new(AtomicBool::new(true)), ..fake() };
    let streaming = Arc::clone(&nemotron.streaming);
    let mut h = harness_with(nemotron, |s| s.unload_policy = UnloadPolicy::Never);
    h.press().send(Message::EngineReady).press();
    streaming.store(false, Ordering::Relaxed); // Whisper is chosen; the streaming model runs.
    h.transcribed(S1, "Thank you for watching!");
    assert_eq!(h.state_of(S1), Some(SessionState::Inserting));
}

/// Only Whisper invents credits: with another engine such a phrase was said.
#[test]
fn credits_are_kept_from_engines_that_do_not_invent_them() {
    let mut h = streaming_harness(fake(), |_| {});
    h.press().send(Message::EngineReady).press().transcribed(S1, "Thank you for watching!");
    assert_eq!(h.state_of(S1), Some(SessionState::Inserting));
}

#[test]
fn language_and_dictionary_go_to_the_engine() {
    let fake = fake();
    let options = Arc::clone(&fake.options);
    let mut h = harness_with(fake, |s| {
        s.spoken_language = Some("ru".into());
        s.dictionary = "Kubernetes\nTauri".into();
    });
    h.press().send(Message::EngineReady).press();
    let sent = options.lock().unwrap().pop().expect("transcribed");
    assert_eq!(sent.language.as_deref(), Some("ru"));
    assert_eq!(sent.prompt.as_deref(), Some("Kubernetes, Tauri"));
    assert!(!sent.translate);
}

#[test]
fn shutdown_discards_the_recording_and_stops_the_engine() {
    let mut h = harness();
    h.press();
    h.calls();
    h.send(Message::Shutdown);
    assert_eq!(h.calls(), vec![Call::Shutdown]);
}
