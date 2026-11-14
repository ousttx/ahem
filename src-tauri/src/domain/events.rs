//! Typed events for the UI. The UI only displays them and never parses message text.

use serde::Serialize;

use super::engine::EngineEvent;
use super::session::{SessionEvent, SessionId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "source", content = "event", rename_all = "snake_case")]
pub enum CoreEvent {
    Session(SessionEvent),
    Engine(EngineEvent),
    Model(ModelEvent),
    Dictation(DictationEvent),
    File(FileEvent),
    Meeting(MeetingEvent),
    Update(UpdateEvent),
}

/// A new version of ahem: shown in the window and the quick panel, never as a system
/// notification. `urgent`: a fix of a serious defect, which the release says so in its manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UpdateEvent {
    /// A newer version: `ready` once it is downloaded and only waits to be installed.
    Available { version: String, urgent: bool, ready: bool },
    /// Downloading and installing after the user asked for it; bytes, `total` when known.
    Progress { version: String, done: u64, total: Option<u64> },
    /// The app runs a version it has just been updated to. `notes`: its changes, a line each
    /// (`- ` starts an item), in the interface's language; `page`: all changes on the web.
    Installed { version: String, notes: Option<String>, page: Option<String> },
    /// The update did not install; the version that runs stays.
    Failed { version: String, detail: String },
}

/// A meeting: recording, then transcription. Times in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MeetingEvent {
    /// A recording started, here or in another window.
    Started {
        meeting: i64,
    },
    /// About five times a second while recording. Levels 0–1000; `*_problem`: why that track
    /// records nothing of what is heard.
    Recording {
        meeting: i64,
        elapsed_ms: u64,
        me_level: u16,
        others_level: u16,
        me_problem: Option<TrackProblem>,
        others_problem: Option<TrackProblem>,
    },
    /// The recording ended and was deleted.
    Discarded {
        meeting: i64,
    },
    /// The recording ended and is saved.
    Recorded {
        meeting: i64,
    },
    Progress {
        meeting: i64,
        step: MeetingStep,
        done_ms: u64,
        total_ms: Option<u64>,
    },
    Done {
        meeting: i64,
    },
    Failed {
        meeting: i64,
        problem: MeetingProblem,
        detail: String,
    },
    /// The transcription was stopped; the recording stays.
    Cancelled {
        meeting: i64,
    },
    /// The list changed: a meeting was renamed or deleted.
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStep {
    /// The microphone track.
    Me,
    /// The computer's sound.
    Others,
    /// Telling the voices apart, first model: where voices speak, with no measure of how far.
    Segments,
    /// Telling the voices apart, second model: whose voices they are.
    Voices,
}

/// The two models of telling the voices of a file apart, as [`MeetingStep::Segments`] and
/// [`MeetingStep::Voices`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakersStep {
    Segments,
    Voices,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingProblem {
    NoModel,
    NoEngine,
    /// The recording could not be read.
    Read,
    Transcription,
    /// The model chosen while it was transcribed gives no word times.
    NoWordTimes,
    NoSpeech,
    Save,
}

impl MeetingProblem {
    pub fn code(self) -> &'static str {
        match self {
            Self::NoModel => "no_model",
            Self::NoEngine => "no_engine",
            Self::Read => "read",
            Self::Transcription => "transcription",
            Self::NoWordTimes => "no_word_times",
            Self::NoSpeech => "no_speech",
            Self::Save => "save",
        }
    }
}

impl From<FileProblem> for MeetingProblem {
    fn from(problem: FileProblem) -> Self {
        match problem {
            FileProblem::NoModel => Self::NoModel,
            FileProblem::NoEngine => Self::NoEngine,
            FileProblem::Read => Self::Read,
            FileProblem::NoWordTimes => Self::NoWordTimes,
            FileProblem::NoSpeech => Self::NoSpeech,
            FileProblem::Save => Self::Save,
            // A meeting's track waits while the slot is busy (`Job::track`), and is never
            // translated.
            FileProblem::Busy
            | FileProblem::Transcription
            | FileProblem::NoTranslation
            | FileProblem::Processing => Self::Transcription,
        }
    }
}

/// A file transcription; one at a time. Times are in milliseconds of the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileEvent {
    Started {
        file: u64,
        name: String,
    },
    Progress {
        file: u64,
        done_ms: u64,
        total_ms: Option<u64>,
    },
    /// The voices of the file are being told apart; for the second model, how far, as the
    /// share of the file's length.
    Speakers {
        file: u64,
        step: SpeakersStep,
        done_ms: u64,
        total_ms: Option<u64>,
    },
    /// The transcript (or the text file) is with the language model.
    Processing {
        file: u64,
    },
    /// `processing_error`: the language model failed, so the transcript itself was saved.
    /// `transcript`: the entry kept in the list of transcripts; none for a text
    /// file, or when it could not be kept.
    Finished {
        file: u64,
        path: String,
        processing_error: Option<String>,
        transcript: Option<i64>,
    },
    /// `transcript`: the entry the transcript was kept in although the file could not be
    /// written (`FileProblem::Save`); `held`: the result is held until the next file, to be
    /// written elsewhere (`save_held_result`).
    Failed {
        file: u64,
        problem: FileProblem,
        detail: String,
        transcript: Option<i64>,
        held: bool,
    },
    Cancelled {
        file: u64,
    },
}

/// Why a meeting's track records nothing of what is heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackProblem {
    /// Its device is gone or cannot be opened: it records silence until one opens.
    NoDevice,
    /// Its file cannot be written (a full disk): it stopped writing for the rest of the meeting.
    CannotWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileProblem {
    /// Another file is being transcribed.
    Busy,
    NoModel,
    NoEngine,
    /// The file could not be read or decoded.
    Read,
    Transcription,
    /// The model chosen while it was transcribed gives no word times.
    NoWordTimes,
    /// The model chosen while it was translated does not translate.
    NoTranslation,
    NoSpeech,
    Processing,
    Save,
}

/// Why a hotkey press (or "Recognize again") could not start a dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    NoModel,
    NoEngine,
    Microphone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DictationEvent {
    Blocked {
        reason: BlockReason,
        detail: String,
    },
    /// Whether the recording of the last dictation that could not be recognized is kept, to be
    /// recognized again (`retry_dictation`).
    RetryAvailable {
        available: bool,
    },
    /// The microphone stopped delivering during the recording (a headset went away); what came
    /// before is recognized.
    MicrophoneLost {
        session: SessionId,
        detail: String,
    },
    /// Input level while recording, 0–1000 (RMS × 1000).
    Level {
        permille: u16,
    },
    /// The dictation takes context: some ticked source of its mode had text.
    ContextTaken {
        session: SessionId,
    },
    /// The "by application" hotkey took the mode the user chose for the program in focus.
    ModeByApp {
        session: SessionId,
        mode_id: String,
    },
    /// The selected text is being rewritten by a language-model mode.
    SelectionStarted {
        mode_id: String,
    },
    SelectionDone,
    SelectionFailed {
        problem: SelectionProblem,
        detail: String,
    },
    /// A dictation went to the history.
    HistorySaved,
    /// Entries were deleted, or the whole history file: every window reads the history again.
    HistoryChanged,
    /// A dictation could not be added to the history; the detail never quotes its text.
    HistoryFailed {
        detail: String,
    },
    /// A dictation was counted, or the statistics were reset: every window reads them again.
    StatsChanged,
}

/// Why the selected text was not rewritten; the selection is left as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionProblem {
    NoFocus,
    FocusChanged,
    Password,
    NotSupported,
    NoSelection,
    SeveralRanges,
    ReadOnly,
    Changed,
    /// The language model failed or is not set up.
    Processing,
    InsertionBlocked,
    Insertion,
    /// The OS has not allowed the app to read or type into other programs (macOS Accessibility).
    NoPermission,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadFailure {
    Cancelled,
    NoSpace,
    Network,
    /// Size or SHA-256 did not match; the data was discarded.
    Verification,
    Disk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelEvent {
    DownloadProgress {
        model: String,
        downloaded: u64,
        total: u64,
    },
    /// The archive is downloaded and verified; its files are being unpacked (sherpa-onnx models).
    Unpacking {
        model: String,
    },
    DownloadFinished {
        model: String,
    },
    DownloadFailed {
        model: String,
        failure: DownloadFailure,
        detail: String,
    },
}

#[cfg(test)]
impl From<SessionEvent> for CoreEvent {
    fn from(event: SessionEvent) -> Self {
        Self::Session(event)
    }
}

#[cfg(test)]
impl From<EngineEvent> for CoreEvent {
    fn from(event: EngineEvent) -> Self {
        Self::Engine(event)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::engine::EngineState;
    use crate::domain::session::{DiscardReason, SessionId, SessionState};

    #[test]
    fn update_events_have_their_own_source() {
        let event = CoreEvent::Update(UpdateEvent::Available {
            version: "0.3.0".into(),
            urgent: false,
            ready: true,
        });
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "source": "update",
                "event": { "type": "available", "version": "0.3.0", "urgent": false, "ready": true },
            })
        );
        let progress =
            UpdateEvent::Progress { version: "0.3.0".into(), done: 10, total: Some(100) };
        assert_eq!(
            serde_json::to_value(progress).unwrap(),
            json!({ "type": "progress", "version": "0.3.0", "done": 10, "total": 100 })
        );
    }

    #[test]
    fn meeting_events_keep_their_shape() {
        let event = CoreEvent::Meeting(MeetingEvent::Progress {
            meeting: 3,
            step: MeetingStep::Segments,
            done_ms: 0,
            total_ms: None,
        });
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "source": "meeting",
                "event": { "type": "progress", "meeting": 3, "step": "segments", "done_ms": 0, "total_ms": null }
            })
        );
        // A file's voices name the same two steps.
        let event = CoreEvent::File(FileEvent::Speakers {
            file: 2,
            step: SpeakersStep::Voices,
            done_ms: 1_000,
            total_ms: Some(4_000),
        });
        assert_eq!(
            serde_json::to_value(&event).unwrap()["event"],
            json!({ "type": "speakers", "file": 2, "step": "voices", "done_ms": 1_000, "total_ms": 4_000 })
        );
        let event = CoreEvent::Meeting(MeetingEvent::Failed {
            meeting: 3,
            problem: MeetingProblem::from(FileProblem::Processing),
            detail: String::new(),
        });
        assert_eq!(serde_json::to_value(&event).unwrap()["event"]["problem"], "transcription");
        for problem in [MeetingProblem::NoSpeech, MeetingProblem::NoEngine, MeetingProblem::Save] {
            assert_eq!(serde_json::to_value(problem).unwrap(), problem.code());
        }
    }

    #[test]
    fn json_shape_is_stable() {
        let event = CoreEvent::from(SessionEvent::StateChanged {
            session: SessionId(7),
            state: SessionState::Discarded { reason: DiscardReason::NoSpeech },
        });
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "source": "session",
                "event": {
                    "type": "state_changed",
                    "session": 7,
                    "state": { "kind": "discarded", "reason": "no_speech" }
                }
            })
        );
        assert_eq!(
            serde_json::to_value(CoreEvent::Dictation(DictationEvent::HistoryChanged)).unwrap(),
            json!({ "source": "dictation", "event": { "type": "history_changed" } })
        );
        let event = CoreEvent::from(EngineEvent::StateChanged { state: EngineState::Loading });
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "source": "engine",
                "event": { "type": "state_changed", "state": "loading" }
            })
        );
        let event = CoreEvent::Model(ModelEvent::DownloadFailed {
            model: "whisper-large-v3".into(),
            failure: DownloadFailure::NoSpace,
            detail: "need 1 byte".into(),
        });
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "source": "model",
                "event": {
                    "type": "download_failed",
                    "model": "whisper-large-v3",
                    "failure": "no_space",
                    "detail": "need 1 byte"
                }
            })
        );
        let event = CoreEvent::Model(ModelEvent::Unpacking { model: "parakeet".into() });
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({ "source": "model", "event": { "type": "unpacking", "model": "parakeet" } })
        );
    }
}
