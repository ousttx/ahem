//! Commands of meetings and kept transcripts, the Transcripts page. Those
//! that wait on the disk or on the tracks' threads (opening, stopping, deleting, exporting)
//! run on a thread for blocking work (`blocking`), so the windows and the capsule never freeze.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::commands::{CORE_EVENT, Dictation, Downloads, chosen_engine_available, throttled};
use crate::domain::events::{CoreEvent, ModelEvent};
use crate::files::ExportFormat;
use crate::meetings::service::{MeetingError, Meetings};
use crate::meetings::speakers::{MODELS_ID, ModelInfo, model_list};
use crate::meetings::store::{CALL_FILE, Kind, ME_FILE, Meeting, MeetingSummary};
use crate::meetings::transcript::{Labels, export, export_file};
use crate::runtimes;
use crate::storage::settings::SettingsStore;

/// Why a meeting command did nothing; never carries a transcript.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum MeetingCommandError {
    Recording,
    NotRecording,
    InUse,
    NotAMeeting,
    UnknownFormat,
    NoOutputFolder,
    /// No speech model is chosen.
    NoModel,
    /// The chosen model's program is not on this computer.
    NoEngine,
    /// The chosen cloud engine gives no word times, which meetings are joined by.
    NoWordTimes,
    Store(String),
    Save(String),
}

impl From<MeetingError> for MeetingCommandError {
    fn from(error: MeetingError) -> Self {
        match error {
            MeetingError::Recording => Self::Recording,
            MeetingError::NotRecording => Self::NotRecording,
            MeetingError::InUse => Self::InUse,
            MeetingError::NotAMeeting => Self::NotAMeeting,
            MeetingError::Store(detail) => Self::Store(detail),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MeetingsOverview {
    pub meetings: Vec<MeetingSummary>,
    pub recording: Option<i64>,
    pub transcribing: Option<i64>,
    /// The voices of the computer's sound can be told apart.
    pub speakers_ready: bool,
    /// This system can record the computer's sound.
    pub system_audio: bool,
}

/// Runs a command's work on a thread for blocking work: off the main thread, where the windows
/// and the capsule would freeze, and off the async runtime's threads.
async fn blocking<T: Send + 'static>(
    app: AppHandle,
    work: impl FnOnce(&AppHandle) -> Result<T, MeetingCommandError> + Send + 'static,
) -> Result<T, MeetingCommandError> {
    tauri::async_runtime::spawn_blocking(move || work(&app))
        .await
        .unwrap_or_else(|error| Err(MeetingCommandError::Store(error.to_string())))
}

#[tauri::command]
pub async fn get_meetings(app: AppHandle) -> Result<MeetingsOverview, MeetingCommandError> {
    blocking(app, |app| overview(&app.state::<Meetings>())).await
}

fn overview(meetings: &Meetings) -> Result<MeetingsOverview, MeetingCommandError> {
    let list = meetings
        .store()
        .with(|store| store.list())
        .map_err(|e| MeetingCommandError::Store(e.to_string()))?;
    Ok(MeetingsOverview {
        meetings: list,
        recording: meetings.recording_id(),
        transcribing: meetings.transcribing_id(),
        speakers_ready: meetings.speakers().ready(),
        system_audio: crate::platform::system_audio_supported(),
    })
}

/// A meeting with where its audio is.
#[derive(Debug, Serialize)]
pub struct MeetingView {
    #[serde(flatten)]
    pub meeting: Meeting,
    /// Both tracks in one file once a transcription has started, else the microphone track;
    /// the tracks are next to it.
    pub audio: String,
}

#[tauri::command]
pub async fn get_meeting(id: i64, app: AppHandle) -> Result<MeetingView, MeetingCommandError> {
    blocking(app, move |app| meeting_view(id, &app.state::<Meetings>())).await
}

fn meeting_view(id: i64, meetings: &Meetings) -> Result<MeetingView, MeetingCommandError> {
    meetings
        .store()
        .with(|store| {
            let folder = store.folder(id)?;
            let call = folder.join(CALL_FILE);
            let audio = if call.exists() { call } else { folder.join(ME_FILE) };
            let audio = audio.display().to_string();
            Ok(MeetingView { meeting: store.get(id)?, audio })
        })
        .map_err(|e| MeetingCommandError::Store(e.to_string()))
}

/// The speaker models, for the Models page.
#[derive(Debug, Serialize)]
pub struct SpeakerModelsView {
    pub ready: bool,
    /// A stopped download left files: they can be deleted, or the download goes on.
    pub partial: bool,
    pub models: Vec<ModelInfo>,
    /// A meeting is being transcribed or a file's voices are being told apart: the models cannot
    /// be deleted now.
    pub busy: bool,
}

#[tauri::command]
pub fn get_speaker_models(meetings: State<'_, Meetings>) -> SpeakerModelsView {
    SpeakerModelsView {
        ready: meetings.speakers().ready(),
        partial: meetings.speakers().partial(),
        models: model_list(),
        busy: meetings.transcribing_id().is_some() || meetings.speakers().in_use(),
    }
}

/// Records with the microphone of the settings.
#[tauri::command]
pub async fn start_meeting(app: AppHandle) -> Result<i64, MeetingCommandError> {
    blocking(app, |app| {
        let settings = app.state::<Mutex<SettingsStore>>();
        let microphone = settings.lock().expect("settings lock").settings().microphone_id.clone();
        Ok(app.state::<Meetings>().start(microphone)?)
    })
    .await
}

/// Kept, and transcribed when the chosen engine can: else it stays recorded, and "Transcribe"
/// says why.
#[tauri::command]
pub async fn stop_meeting(app: AppHandle) -> Result<i64, MeetingCommandError> {
    blocking(app, |app| {
        let transcribe =
            chosen_can_transcribe(&app.state::<Mutex<SettingsStore>>(), &app.state::<Dictation>())
                .is_ok();
        Ok(app.state::<Meetings>().stop(transcribe)?)
    })
    .await
}

#[tauri::command]
pub async fn discard_meeting(app: AppHandle) -> Result<(), MeetingCommandError> {
    blocking(app, |app| Ok(app.state::<Meetings>().discard()?)).await
}

/// Refused at once when the chosen engine cannot transcribe it, not after the echo is found and
/// the tracks are mixed.
#[tauri::command]
pub async fn transcribe_meeting(id: i64, app: AppHandle) -> Result<(), MeetingCommandError> {
    blocking(app, move |app| {
        chosen_can_transcribe(&app.state::<Mutex<SettingsStore>>(), &app.state::<Dictation>())?;
        Ok(app.state::<Meetings>().transcribe(id)?)
    })
    .await
}

fn chosen_can_transcribe(
    store: &Mutex<SettingsStore>,
    dictation: &Dictation,
) -> Result<(), MeetingCommandError> {
    let model_id = store.lock().expect("settings lock").settings().model_id.clone();
    can_transcribe(model_id.as_deref(), &dictation.whisper_executable, &dictation.llama_executable)
}

fn can_transcribe(
    model_id: Option<&str>,
    whisper_server: &Path,
    llama_server: &Path,
) -> Result<(), MeetingCommandError> {
    if model_id.is_none() {
        return Err(MeetingCommandError::NoModel);
    }
    if !chosen_engine_available(model_id, whisper_server, llama_server) {
        return Err(MeetingCommandError::NoEngine);
    }
    if !crate::engines::transcribes_media(model_id) {
        return Err(MeetingCommandError::NoWordTimes);
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_meeting_transcription(id: i64, meetings: State<'_, Meetings>) {
    meetings.cancel_transcription(id);
}

#[tauri::command]
pub async fn delete_meeting(id: i64, app: AppHandle) -> Result<(), MeetingCommandError> {
    blocking(app, move |app| Ok(app.state::<Meetings>().delete(id)?)).await
}

#[tauri::command]
pub async fn rename_meeting(
    id: i64,
    title: Option<String>,
    app: AppHandle,
) -> Result<(), MeetingCommandError> {
    blocking(app, move |app| Ok(app.state::<Meetings>().rename(id, title.as_deref())?)).await
}

/// Saves the transcript to `output` in `format`, the speakers named by `labels`.
#[tauri::command]
pub async fn export_meeting(
    id: i64,
    output: String,
    format: String,
    title: String,
    labels: Labels,
    app: AppHandle,
) -> Result<(), MeetingCommandError> {
    blocking(app, move |app| {
        export_to(id, output, format, title, &labels, &app.state::<Meetings>())
    })
    .await
}

fn export_to(
    id: i64,
    output: String,
    format: String,
    title: String,
    labels: &Labels,
    meetings: &Meetings,
) -> Result<(), MeetingCommandError> {
    let format =
        ExportFormat::from_extension(&format, title).ok_or(MeetingCommandError::UnknownFormat)?;
    let output = PathBuf::from(output);
    if !output.parent().is_some_and(Path::is_dir) {
        return Err(MeetingCommandError::NoOutputFolder);
    }
    let meeting = meetings
        .store()
        .with(|store| store.get(id))
        .map_err(|e| MeetingCommandError::Store(e.to_string()))?;
    let text = match meeting.summary.kind {
        Kind::Meeting => export(&meeting.turns, &format, labels),
        // A file processed by a language model: its text in Markdown and plain text, as the
        // Files page saved it; the transcript in formats with word times.
        Kind::File => meeting
            .processed
            .as_deref()
            .and_then(|processed| crate::files::export::processed_export(processed, &format))
            .unwrap_or_else(|| export_file(&meeting.turns, &format, labels)),
    };
    crate::storage::write_whole(&output, &text)
        .map_err(|e| MeetingCommandError::Save(e.to_string()))?;
    // A file whose result could not be written has it now.
    if meeting.summary.kind == Kind::File && meeting.summary.output.is_none() {
        meetings.file_saved(id, &output)?;
    }
    Ok(())
}

/// Downloads the speaker models with the same events as a speech model, as `speakers`;
/// `cancel_model_download("speakers")` stops it.
#[tauri::command]
pub fn download_speaker_models(
    app: AppHandle,
    meetings: State<'_, Meetings>,
    downloads: State<'_, Downloads>,
) {
    let cancel = Arc::new(AtomicBool::new(false));
    if !downloads.start(MODELS_ID, Arc::clone(&cancel)) {
        return;
    }
    let models = meetings.speakers().clone();
    thread::spawn(move || {
        let emit = |event| {
            let _ = app.emit(CORE_EVENT, CoreEvent::Model(event));
        };
        let mut progress = throttled(|downloaded, total| {
            emit(ModelEvent::DownloadProgress { model: MODELS_ID.into(), downloaded, total });
        });
        let result = models.download(&runtimes::http::agent(), &cancel, &mut progress);
        app.state::<Downloads>().finish(MODELS_ID);
        match result {
            Ok(()) => emit(ModelEvent::DownloadFinished { model: MODELS_ID.into() }),
            Err(error) => emit(ModelEvent::DownloadFailed {
                model: MODELS_ID.into(),
                failure: error.failure(),
                detail: error.to_string(),
            }),
        }
    });
}

/// Refused while the models tell voices apart (a meeting or a file), and while they are being
/// downloaded (the download would fail half way).
#[tauri::command]
pub async fn delete_speaker_models(app: AppHandle) -> Result<(), MeetingCommandError> {
    blocking(app, |app| delete_speakers(&app.state::<Meetings>(), &app.state::<Downloads>())).await
}

/// Deletes the speaker models (some 80 MB), when nothing uses them.
fn delete_speakers(meetings: &Meetings, downloads: &Downloads) -> Result<(), MeetingCommandError> {
    if meetings.transcribing_id().is_some()
        || meetings.speakers().in_use()
        || downloads.running(MODELS_ID)
    {
        return Err(MeetingCommandError::InUse);
    }
    let deleted = meetings.speakers().delete();
    meetings.changed();
    deleted.map_err(|e| MeetingCommandError::Store(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{MeetingCommandError, can_transcribe};

    /// A meeting is refused before the echo is found and the tracks are mixed when the chosen
    /// engine cannot transcribe it.
    #[test]
    fn a_meeting_is_refused_at_once_when_the_engine_cannot_transcribe_it() {
        let present = std::env::current_exe().unwrap();
        let missing = present.with_file_name("no-such-engine");
        let check = |model| can_transcribe(model, &missing, &present);
        assert!(matches!(check(None), Err(MeetingCommandError::NoModel)));
        assert!(matches!(check(Some("whisper-medium")), Err(MeetingCommandError::NoEngine)));
        let cloud = check(Some("openai-gpt-transcribe"));
        assert!(matches!(cloud, Err(MeetingCommandError::NoWordTimes)));
        assert!(check(Some("qwen3-asr-0_6b")).is_ok());
        assert!(check(Some("deepgram-nova-3")).is_ok());
    }
}
