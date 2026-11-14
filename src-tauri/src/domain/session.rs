//! One dictation from the microphone's opening to text insertion, or a failed one recognized again
//! (its text kept, not inserted).

use std::time::Duration;

use serde::Serialize;

use super::InvalidTransition;

const MACHINE: &str = "dictation session";

/// Recordings shorter than this are discarded without transcription.
pub const MIN_RECORDING: Duration = Duration::from_millis(150);
/// Less speech than this (see `audio::speech`) is not sent to the engine: Whisper invents
/// text on silence. Low on purpose, so a short "yes" survives.
pub const MIN_SPEECH: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct SessionId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMode {
    pub id: String,
    /// The transcript goes through a language model before insertion.
    pub post_process: bool,
    /// The text is typed into the window of the hotkey press; `false` keeps it in the history
    /// and the last result only (a recording recognized again after an error).
    pub insert: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Recording,
    Transcription,
    Processing,
    /// The language model returned no text.
    EmptyProcessing,
    /// The share of the ChatGPT plan the user gave ahem is used up; the transcript is used.
    PlanLimit,
    /// No window had keyboard focus.
    NoFocus,
    /// Focus moved to another window before the text was ready.
    FocusChanged,
    /// The window refuses typed input, e.g. it runs as administrator.
    InsertionBlocked,
    /// The OS has not allowed the app to type into other programs (macOS Accessibility).
    NoPermission,
    Insertion,
}

/// Why the platform could not insert the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionProblem {
    NoFocus,
    FocusChanged,
    Blocked,
    NoPermission,
    Other,
}

impl InsertionProblem {
    fn kind(self) -> FailureKind {
        match self {
            Self::NoFocus => FailureKind::NoFocus,
            Self::FocusChanged => FailureKind::FocusChanged,
            Self::Blocked => FailureKind::InsertionBlocked,
            Self::NoPermission => FailureKind::NoPermission,
            Self::Other => FailureKind::Insertion,
        }
    }
}

/// `detail` is technical text shown next to the translated message; the UI never parses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscardReason {
    TooShort,
    NoSpeech,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionState {
    Recording,
    Transcribing,
    PostProcessing,
    Inserting,
    Inserted,
    /// The text is kept (history, last result) without typing it.
    Kept,
    Discarded {
        reason: DiscardReason,
    },
    Failed {
        failure: Failure,
    },
}

impl SessionState {
    pub fn is_finished(&self) -> bool {
        matches!(self, Self::Inserted | Self::Kept | Self::Discarded { .. } | Self::Failed { .. })
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Transcribing => "transcribing",
            Self::PostProcessing => "post_processing",
            Self::Inserting => "inserting",
            Self::Inserted => "inserted",
            Self::Kept => "kept",
            Self::Discarded { .. } => "discarded",
            Self::Failed { .. } => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionInput {
    /// `speech` is how much of the recording sounds like speech.
    RecordingStopped {
        duration: Duration,
        speech: Duration,
    },
    RecordingFailed {
        detail: String,
    },
    TranscriptReady {
        text: String,
        language: Option<String>,
    },
    TranscriptionFailed {
        detail: String,
    },
    ProcessingDone {
        text: String,
    },
    ProcessingFailed {
        detail: String,
    },
    /// The ChatGPT plan's limit is reached: like a failure, with its own kind.
    PlanLimitReached,
    InsertionDone,
    InsertionFailed {
        problem: InsertionProblem,
        detail: String,
    },
}

impl SessionInput {
    fn name(&self) -> &'static str {
        match self {
            Self::RecordingStopped { .. } => "recording_stopped",
            Self::RecordingFailed { .. } => "recording_failed",
            Self::TranscriptReady { .. } => "transcript_ready",
            Self::TranscriptionFailed { .. } => "transcription_failed",
            Self::ProcessingDone { .. } => "processing_done",
            Self::ProcessingFailed { .. } => "processing_failed",
            Self::PlanLimitReached => "plan_limit_reached",
            Self::InsertionDone => "insertion_done",
            Self::InsertionFailed { .. } => "insertion_failed",
        }
    }
}

/// What a dictation produced: kept as the last transcription and, when enabled, in history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DictationResult {
    pub mode_id: String,
    pub transcript: String,
    pub language: Option<String>,
    /// Language-model output; `None` without processing or when processing failed.
    pub processed: Option<String>,
    pub inserted: bool,
}

impl DictationResult {
    /// The text that was inserted, or would have been.
    pub fn text(&self) -> &str {
        self.processed.as_deref().unwrap_or(&self.transcript)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    StateChanged {
        session: SessionId,
        state: SessionState,
    },
    /// Processing failed; the transcript is used without processing (inserted, or kept).
    ProcessingFailed {
        session: SessionId,
        failure: Failure,
    },
    /// The session finished with a transcript: inserted, kept, or not inserted.
    Finished {
        session: SessionId,
        result: DictationResult,
    },
}

#[derive(Debug)]
pub struct DictationSession {
    id: SessionId,
    mode: SessionMode,
    state: SessionState,
    transcript: Option<String>,
    language: Option<String>,
    processed: Option<String>,
}

impl DictationSession {
    /// A session starts when recording starts (a recording recognized again starts here too,
    /// and is stopped at once).
    pub fn start(id: SessionId, mode: SessionMode) -> (Self, Vec<SessionEvent>) {
        let session = Self {
            id,
            mode,
            state: SessionState::Recording,
            transcript: None,
            language: None,
            processed: None,
        };
        let event = session.state_changed();
        (session, vec![event])
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// Whether its text is typed into a window (not a recording recognized again).
    pub fn inserts(&self) -> bool {
        self.mode.insert
    }

    /// Whether its transcript goes through a language model.
    pub fn post_processes(&self) -> bool {
        self.mode.post_process
    }

    /// The text to insert: the processed text if there is one, else the transcript.
    pub fn text(&self) -> Option<&str> {
        self.processed.as_deref().or(self.transcript.as_deref())
    }

    pub fn handle(&mut self, input: SessionInput) -> Result<Vec<SessionEvent>, InvalidTransition> {
        use SessionInput as I;
        use SessionState as S;

        let mut events = Vec::new();
        let next = match (&self.state, input) {
            (S::Recording, I::RecordingStopped { duration, speech }) => {
                if duration < MIN_RECORDING {
                    S::Discarded { reason: DiscardReason::TooShort }
                } else if speech < MIN_SPEECH {
                    S::Discarded { reason: DiscardReason::NoSpeech }
                } else {
                    S::Transcribing
                }
            }
            (S::Recording, I::RecordingFailed { detail }) => failed(FailureKind::Recording, detail),
            (S::Transcribing, I::TranscriptReady { text, language }) => {
                if text.trim().is_empty() {
                    S::Discarded { reason: DiscardReason::NoSpeech }
                } else {
                    self.transcript = Some(text);
                    self.language = language;
                    if self.mode.post_process { S::PostProcessing } else { self.text_ready() }
                }
            }
            (S::Transcribing, I::TranscriptionFailed { detail }) => {
                failed(FailureKind::Transcription, detail)
            }
            (S::PostProcessing, I::ProcessingDone { text }) => {
                if text.trim().is_empty() {
                    events
                        .push(self.processing_failed(FailureKind::EmptyProcessing, String::new()));
                } else {
                    self.processed = Some(text);
                }
                self.text_ready()
            }
            (S::PostProcessing, I::ProcessingFailed { detail }) => {
                events.push(self.processing_failed(FailureKind::Processing, detail));
                self.text_ready()
            }
            (S::PostProcessing, I::PlanLimitReached) => {
                events.push(self.processing_failed(FailureKind::PlanLimit, String::new()));
                self.text_ready()
            }
            (S::Inserting, I::InsertionDone) => S::Inserted,
            (S::Inserting, I::InsertionFailed { problem, detail }) => {
                failed(problem.kind(), detail)
            }
            (state, input) => {
                return Err(InvalidTransition::new(MACHINE, state.name(), input.name()));
            }
        };
        self.state = next;
        events.push(self.state_changed());
        if let Some(result) = self.result() {
            events.push(SessionEvent::Finished { session: self.id, result });
        }
        Ok(events)
    }

    /// Where the session goes once its text is ready.
    fn text_ready(&self) -> SessionState {
        if self.mode.insert { SessionState::Inserting } else { SessionState::Kept }
    }

    fn state_changed(&self) -> SessionEvent {
        SessionEvent::StateChanged { session: self.id, state: self.state.clone() }
    }

    fn processing_failed(&self, kind: FailureKind, detail: String) -> SessionEvent {
        SessionEvent::ProcessingFailed { session: self.id, failure: Failure { kind, detail } }
    }

    /// Present once the session finished with a transcript: inserted, kept, or insertion failed.
    fn result(&self) -> Option<DictationResult> {
        let inserted = match &self.state {
            SessionState::Inserted => true,
            SessionState::Kept | SessionState::Failed { .. } => false,
            _ => return None,
        };
        Some(DictationResult {
            mode_id: self.mode.id.clone(),
            transcript: self.transcript.clone()?,
            language: self.language.clone(),
            processed: self.processed.clone(),
            inserted,
        })
    }
}

fn failed(kind: FailureKind, detail: String) -> SessionState {
    SessionState::Failed { failure: Failure { kind, detail } }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "secret dictation text";

    fn mode(post_process: bool) -> SessionMode {
        SessionMode { id: "plain".into(), post_process, insert: true }
    }

    fn session(post_process: bool) -> DictationSession {
        DictationSession::start(SessionId(1), mode(post_process)).0
    }

    /// A recording that is speech from start to end.
    fn stopped(seconds: f32) -> SessionInput {
        let duration = Duration::from_secs_f32(seconds);
        SessionInput::RecordingStopped { duration, speech: duration }
    }

    fn transcript(text: &str) -> SessionInput {
        SessionInput::TranscriptReady { text: text.into(), language: Some("ru".into()) }
    }

    #[test]
    fn a_session_that_does_not_insert_keeps_its_text() {
        let keep = SessionMode { id: "plain".into(), post_process: false, insert: false };
        let mut session = DictationSession::start(SessionId(2), keep).0;
        session.handle(stopped(2.0)).unwrap();
        let events = session.handle(transcript("again")).unwrap();
        assert_eq!(*session.state(), SessionState::Kept);
        let finished = events.iter().find_map(|event| match event {
            SessionEvent::Finished { result, .. } => Some(result),
            _ => None,
        });
        let result = finished.expect("a result");
        assert_eq!(result.transcript, "again");
        assert!(!result.inserted);
    }

    fn feed(session: &mut DictationSession, inputs: Vec<SessionInput>) -> Vec<SessionEvent> {
        inputs
            .into_iter()
            .flat_map(|input| session.handle(input).expect("valid transition"))
            .collect()
    }

    fn finished(events: &[SessionEvent]) -> Option<&DictationResult> {
        events.iter().find_map(|event| match event {
            SessionEvent::Finished { result, .. } => Some(result),
            _ => None,
        })
    }

    /// Builds a session in the named state through valid inputs.
    fn in_state(name: &str) -> DictationSession {
        let mut session = session(true);
        let (path, expected) = match name {
            "recording" => (vec![], "recording"),
            "transcribing" => (vec![stopped(1.0)], "transcribing"),
            "post_processing" => (vec![stopped(1.0), transcript("hi")], "post_processing"),
            "inserting" => (
                vec![
                    stopped(1.0),
                    transcript("hi"),
                    SessionInput::ProcessingDone { text: "ok".into() },
                ],
                "inserting",
            ),
            "finished" => (vec![stopped(0.0)], "discarded"),
            _ => unreachable!(),
        };
        feed(&mut session, path);
        assert_eq!(session.state().name(), expected);
        session
    }

    fn all_inputs() -> Vec<SessionInput> {
        vec![
            stopped(1.0),
            SessionInput::RecordingFailed { detail: "mic".into() },
            transcript("hi"),
            SessionInput::TranscriptionFailed { detail: "engine".into() },
            SessionInput::ProcessingDone { text: "ok".into() },
            SessionInput::ProcessingFailed { detail: "api".into() },
            SessionInput::InsertionDone,
            SessionInput::InsertionFailed {
                problem: InsertionProblem::FocusChanged,
                detail: "focus".into(),
            },
        ]
    }

    #[test]
    fn transition_table() {
        let accepted: &[(&str, &[&str])] = &[
            ("recording", &["recording_stopped", "recording_failed"]),
            ("transcribing", &["transcript_ready", "transcription_failed"]),
            ("post_processing", &["processing_done", "processing_failed"]),
            ("inserting", &["insertion_done", "insertion_failed"]),
            ("finished", &[]),
        ];
        for (state, valid) in accepted {
            for input in all_inputs() {
                let mut session = in_state(state);
                let before = session.state().clone();
                let name = input.name();
                let result = session.handle(input);
                assert_eq!(result.is_ok(), valid.contains(&name), "{state} + {name}");
                if result.is_err() {
                    assert_eq!(session.state(), &before, "{state} + {name} changed state");
                }
            }
        }
    }

    #[test]
    fn start_reports_recording() {
        let (_, events) = DictationSession::start(SessionId(5), mode(false));
        assert_eq!(
            events,
            vec![SessionEvent::StateChanged {
                session: SessionId(5),
                state: SessionState::Recording
            }]
        );
    }

    #[test]
    fn plain_dictation_is_inserted() {
        let mut session = session(false);
        let events = feed(
            &mut session,
            vec![stopped(2.0), transcript("hello"), SessionInput::InsertionDone],
        );
        assert_eq!(session.state(), &SessionState::Inserted);
        let result = finished(&events).expect("finished");
        assert_eq!(result.text(), "hello");
        assert_eq!(result.language.as_deref(), Some("ru"));
        assert!(result.inserted);
        assert_eq!(result.processed, None);
    }

    #[test]
    fn processed_text_is_inserted() {
        let mut session = session(true);
        let events = feed(
            &mut session,
            vec![
                stopped(2.0),
                transcript("raw"),
                SessionInput::ProcessingDone { text: "clean".into() },
                SessionInput::InsertionDone,
            ],
        );
        let result = finished(&events).expect("finished");
        assert_eq!(result.transcript, "raw");
        assert_eq!(result.text(), "clean");
    }

    #[test]
    fn processing_failure_inserts_transcript() {
        let mut session = session(true);
        let events = feed(
            &mut session,
            vec![
                stopped(2.0),
                transcript("raw"),
                SessionInput::ProcessingFailed { detail: "401".into() },
            ],
        );
        assert_eq!(session.state(), &SessionState::Inserting);
        assert!(events.contains(&SessionEvent::ProcessingFailed {
            session: SessionId(1),
            failure: Failure { kind: FailureKind::Processing, detail: "401".into() }
        }));
        let events = feed(&mut session, vec![SessionInput::InsertionDone]);
        assert_eq!(finished(&events).expect("finished").text(), "raw");
    }

    #[test]
    fn a_used_up_plan_inserts_the_transcript_with_its_own_kind() {
        let mut session = session(true);
        let events = feed(
            &mut session,
            vec![stopped(2.0), transcript("raw"), SessionInput::PlanLimitReached],
        );
        assert!(events.contains(&SessionEvent::ProcessingFailed {
            session: SessionId(1),
            failure: Failure { kind: FailureKind::PlanLimit, detail: String::new() }
        }));
        let events = feed(&mut session, vec![SessionInput::InsertionDone]);
        assert_eq!(finished(&events).expect("finished").text(), "raw");
    }

    #[test]
    fn empty_processing_result_inserts_transcript() {
        let mut session = session(true);
        let events = feed(
            &mut session,
            vec![
                stopped(2.0),
                transcript("raw"),
                SessionInput::ProcessingDone { text: "  ".into() },
            ],
        );
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::ProcessingFailed { failure, .. } if failure.kind == FailureKind::EmptyProcessing
        )));
        let events = feed(&mut session, vec![SessionInput::InsertionDone]);
        assert_eq!(finished(&events).expect("finished").text(), "raw");
    }

    #[test]
    fn insertion_failure_keeps_the_result() {
        let mut session = session(false);
        let events = feed(
            &mut session,
            vec![
                stopped(2.0),
                transcript("hello"),
                SessionInput::InsertionFailed {
                    problem: InsertionProblem::Blocked,
                    detail: "uipi".into(),
                },
            ],
        );
        assert!(matches!(
            session.state(),
            SessionState::Failed { failure } if failure.kind == FailureKind::InsertionBlocked
        ));
        let result = finished(&events).expect("finished");
        assert_eq!(result.text(), "hello");
        assert!(!result.inserted);
    }

    #[test]
    fn short_recording_is_discarded() {
        let mut session = session(false);
        let events = feed(&mut session, vec![stopped(0.1)]);
        assert_eq!(session.state(), &SessionState::Discarded { reason: DiscardReason::TooShort });
        assert!(finished(&events).is_none());
    }

    #[test]
    fn recording_without_speech_is_not_transcribed() {
        let mut session = session(false);
        let silence = SessionInput::RecordingStopped {
            duration: Duration::from_secs(3),
            speech: Duration::from_millis(100),
        };
        feed(&mut session, vec![silence]);
        assert_eq!(session.state(), &SessionState::Discarded { reason: DiscardReason::NoSpeech });
    }

    #[test]
    fn blank_transcript_is_discarded() {
        let mut session = session(false);
        feed(&mut session, vec![stopped(1.0), transcript(" \n")]);
        assert_eq!(session.state(), &SessionState::Discarded { reason: DiscardReason::NoSpeech });
    }

    #[test]
    fn transcription_failure_has_no_result() {
        let mut session = session(false);
        let events = feed(
            &mut session,
            vec![stopped(1.0), SessionInput::TranscriptionFailed { detail: "engine".into() }],
        );
        assert!(session.state().is_finished());
        assert!(finished(&events).is_none());
    }

    #[test]
    fn invalid_transition_hides_dictation_text() {
        let mut session = session(false);
        let error = session.handle(transcript(SECRET)).unwrap_err();
        assert_eq!(error.state, "recording");
        assert_eq!(error.input, "transcript_ready");
        assert!(!error.to_string().contains(SECRET));
        assert!(!format!("{error:?}").contains(SECRET));
    }
}
