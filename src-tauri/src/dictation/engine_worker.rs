//! The thread that owns the speech engine — the `whisper-server` or `llama-server` process,
//! the sherpa-onnx worker process or a cloud service: start, transcribe and stop run
//! here one at a time, so the orchestrator never waits for the engine.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use super::Message;
use crate::audio::capture::Captured;
use crate::audio::resample;
use crate::domain::scheduler::FileTaskId;
use crate::domain::session::SessionId;
use crate::engines::cloud::{CloudClient, CloudError, CloudSpec};
use crate::engines::llama_server::{LlamaConfig, LlamaServer};
use crate::engines::sherpa_process::SherpaProcess;
use crate::engines::whisper_server::{
    EngineError, ServerConfig, TranscribeOptions, Transcript, WhisperServer,
};
use crate::runtimes::http;
use crate::storage::models::ReadyModel;
use crate::storage::secrets::SecretStore;

pub enum EngineRequest {
    Start {
        model: ReadyModel,
    },
    /// A cloud service recognizes speech with the user's key; there is nothing to load. The key
    /// is read here, on the engine's thread: the first read after an update may wait on the
    /// system's password prompt, and the orchestrator must not wait with it.
    StartCloud {
        spec: CloudSpec,
        connection_id: String,
        secrets: SecretStore,
    },
    Transcribe {
        session: SessionId,
        audio: Captured,
        options: TranscribeOptions,
    },
    /// A window of a file; the reply carries the engine's JSON with word times.
    TranscribeChunk {
        file: FileTaskId,
        chunk: u32,
        audio: Captured,
        options: TranscribeOptions,
    },
    /// A dictation starts and the loaded model streams: it gets the audio while recording.
    /// Ignored by a model that does not stream.
    StreamStart {
        session: SessionId,
        language: Option<String>,
        rate: u32,
    },
    /// More of the dictation's audio; replies `LiveText` when the text changed.
    StreamFeed {
        session: SessionId,
        samples: Vec<f32>,
    },
    /// The recording ended (its last samples were fed): replies `Transcribed`. Without the live
    /// stream (the engine changed meanwhile) the whole audio is transcribed instead.
    StreamFinish {
        session: SessionId,
        audio: Captured,
        options: TranscribeOptions,
    },
    /// The dictation ended without a transcript.
    StreamCancel {
        session: SessionId,
    },
    Stop,
}

/// `executable` is whisper-server, `llama` is llama-server. Setting `quit` ends a server that
/// is still loading its model, so the app does not wait for it to finish when it quits.
pub fn spawn(
    executable: PathBuf,
    llama: PathBuf,
    log: PathBuf,
    replies: Sender<Message>,
    quit: Arc<AtomicBool>,
) -> (Sender<EngineRequest>, JoinHandle<()>) {
    let (requests, inbox) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut engine: Option<Running> = None;
        // The dictation whose live stream runs in the sherpa-onnx worker.
        let mut live: Option<SessionId> = None;
        // Ends when the orchestrator drops its sender; dropping the server kills the process.
        for request in inbox {
            let reply = match request {
                EngineRequest::Start { model } => {
                    live = None;
                    let threads = ServerConfig::default_threads();
                    let started = match (&model.sherpa, &model.llama) {
                        (Some(spec), _) => {
                            SherpaProcess::start(spec, &model.path, threads, &log, &quit)
                                .map(Running::Sherpa)
                        }
                        (None, Some(spec)) => {
                            let config = LlamaConfig {
                                executable: llama.clone(),
                                model: model.path.clone(),
                                mmproj: model.path.with_file_name(&spec.mmproj.file),
                                log: log.clone(),
                                threads,
                            };
                            LlamaServer::start(&config, spec, &quit)
                                .map(Running::Llama)
                                .map_err(|error| error.to_string())
                        }
                        (None, None) => {
                            let config = ServerConfig {
                                executable: executable.clone(),
                                model: model.path,
                                log: log.clone(),
                                threads,
                            };
                            WhisperServer::start(&config, &quit)
                                .map(Running::Whisper)
                                .map_err(|error| error.to_string())
                        }
                    };
                    Some(match started {
                        Ok(started) => {
                            engine = Some(started);
                            Message::EngineReady
                        }
                        Err(error) => Message::EngineFailed(error),
                    })
                }
                EngineRequest::StartCloud { spec, connection_id, secrets } => {
                    live = None;
                    Some(match cloud_key(&secrets, &connection_id) {
                        Ok(key) => {
                            let client = CloudClient::new(http::agent(), spec, key);
                            engine = Some(Running::Cloud(client));
                            Message::EngineReady
                        }
                        Err(detail) => Message::EngineFailed(detail),
                    })
                }
                EngineRequest::Transcribe { session, audio, options } => {
                    let result = transcribe(&mut engine, &replies, &audio, &options);
                    if result.is_err() {
                        let _ = replies.send(unrecognized(session, &audio));
                    }
                    Some(Message::Transcribed { session, result })
                }
                EngineRequest::TranscribeChunk { file, chunk, audio, options } => {
                    let result = run(
                        &mut engine,
                        &replies,
                        |server| server.transcribe_raw(&audio.to_wav(), &options),
                        |llama| {
                            let window = llama.transcribe_words(&audio.samples, audio.rate)?;
                            if window.cut {
                                log_line!(
                                    "file {}, window {chunk}: the answer was cut off at its \
                                     length and the window is left empty",
                                    file.0
                                );
                            }
                            Ok(window.json)
                        },
                        |sherpa| sherpa.transcribe_words(&audio.samples, audio.rate, &options),
                        |cloud| cloud.transcribe_words(&audio.to_wav(), &options),
                    );
                    Some(Message::ChunkTranscribed { file, chunk, result })
                }
                EngineRequest::StreamStart { session, language, rate } => {
                    if let Some(Running::Sherpa(sherpa)) = &mut engine {
                        live = sherpa.start_live(language.as_deref(), rate).then_some(session);
                    }
                    None
                }
                EngineRequest::StreamFeed { session, samples } => match (&mut engine, live) {
                    (Some(Running::Sherpa(sherpa)), Some(id)) if id == session => {
                        sherpa.feed_live(&samples).map(|text| Message::LiveText { session, text })
                    }
                    _ => None,
                },
                EngineRequest::StreamFinish { session, audio, options } => {
                    let streaming = live.take_if(|id| *id == session).is_some();
                    let finished = match &mut engine {
                        Some(Running::Sherpa(sherpa)) if streaming => {
                            let result = sherpa.finish_live();
                            // A worker that died here is reported like one that died in `run`.
                            if result.is_err() && sherpa.died() {
                                engine = None;
                                let crashed =
                                    Message::EngineCrashed("engine process exited".into());
                                let _ = replies.send(crashed);
                            }
                            Some(result)
                        }
                        _ => None,
                    };
                    let result = finished
                        .unwrap_or_else(|| transcribe(&mut engine, &replies, &audio, &options));
                    if result.is_err() {
                        let _ = replies.send(unrecognized(session, &audio));
                    }
                    Some(Message::Transcribed { session, result })
                }
                EngineRequest::StreamCancel { session } => {
                    if live.take_if(|id| *id == session).is_some()
                        && let Some(Running::Sherpa(sherpa)) = &mut engine
                    {
                        sherpa.cancel_live();
                    }
                    None
                }
                EngineRequest::Stop => {
                    live = None;
                    match engine.take() {
                        Some(Running::Whisper(server)) => {
                            let _ = server.stop();
                        }
                        Some(Running::Llama(server)) => {
                            let _ = server.stop();
                        }
                        // Ending the worker returns all of the model's memory to the system.
                        Some(Running::Sherpa(worker)) => worker.stop(),
                        _ => {}
                    }
                    Some(Message::EngineStopped)
                }
            };
            if let Some(reply) = reply {
                let _ = replies.send(reply);
            }
        }
    });
    (requests, worker)
}

/// The loaded engine. Dropping a server or the sherpa-onnx worker stops its process; dropping a
/// cloud engine forgets the key.
enum Running {
    Whisper(WhisperServer),
    Llama(LlamaServer),
    Sherpa(SherpaProcess),
    Cloud(CloudClient),
}

/// A whole recording to the loaded engine.
fn transcribe(
    engine: &mut Option<Running>,
    replies: &Sender<Message>,
    audio: &Captured,
    options: &TranscribeOptions,
) -> Result<Transcript, String> {
    run(
        engine,
        replies,
        |server| server.transcribe(&audio.to_wav(), options),
        |llama| llama.transcribe(&audio.samples, audio.rate),
        |sherpa| sherpa.transcribe(&audio.samples, audio.rate, options),
        |cloud| cloud.transcribe(&audio.to_wav(), options),
    )
}

/// One request to the loaded engine. A failed request whose process (whisper-server,
/// llama-server or the sherpa-onnx worker) has died also reports `EngineCrashed` first and
/// forgets the process. A cloud error names the service.
fn run<T>(
    engine: &mut Option<Running>,
    replies: &Sender<Message>,
    whisper: impl FnOnce(&WhisperServer) -> Result<T, EngineError>,
    llama: impl FnOnce(&LlamaServer) -> Result<T, EngineError>,
    sherpa: impl FnOnce(&mut SherpaProcess) -> Result<T, String>,
    cloud: impl FnOnce(&CloudClient) -> Result<T, CloudError>,
) -> Result<T, String> {
    let (result, exited) = match engine.as_mut() {
        None => return Err("engine is not running".to_string()),
        Some(Running::Sherpa(running)) => {
            let result = sherpa(running);
            let exited = result.is_err() && running.died();
            (result, exited)
        }
        Some(Running::Cloud(client)) => {
            return cloud(client).map_err(|error| format!("{}: {error}", client.service_name()));
        }
        Some(Running::Whisper(running)) => {
            let result = whisper(running).map_err(|error| error.to_string());
            let exited = result.is_err() && running.died();
            (result, exited)
        }
        Some(Running::Llama(running)) => {
            let result = llama(running).map_err(|error| error.to_string());
            let exited = result.is_err() && running.died();
            (result, exited)
        }
    };
    if exited {
        *engine = None;
        let _ = replies.send(Message::EngineCrashed("engine process exited".into()));
    }
    result
}

/// A failed dictation's audio for the controller to keep, at 16 kHz: converted here, not on the
/// controller's thread (seconds for a long recording).
fn unrecognized(session: SessionId, audio: &Captured) -> Message {
    let samples = resample::to_16k(&audio.samples, audio.rate);
    Message::Unrecognized { session, audio: Captured { samples, rate: resample::TARGET_RATE } }
}

/// The key of a cloud engine's connection; why there is none, as the engine's failure.
fn cloud_key(secrets: &SecretStore, connection_id: &str) -> Result<String, String> {
    match secrets.get(connection_id) {
        Ok(Some(key)) => Ok(key),
        Ok(None) => Err("the service has no key".into()),
        Err(error) => Err(format!("cannot read the key: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection without a stored key fails the engine's start with why.
    #[test]
    fn a_cloud_engine_without_its_key_fails_to_start() {
        let secrets = SecretStore::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/test-artifacts/no-such-secrets.json"),
        );
        assert_eq!(cloud_key(&secrets, "groq-1"), Err("the service has no key".into()));
    }

    /// Started on the engine's thread, a cloud engine without its key answers that it failed.
    #[test]
    fn the_engine_thread_reads_the_key_and_reports_a_missing_one() {
        let (replies, answers) = mpsc::channel();
        let nowhere = std::path::PathBuf::from("no-such-program");
        let (requests, worker) = spawn(
            nowhere.clone(),
            nowhere.clone(),
            nowhere,
            replies,
            Arc::new(AtomicBool::new(false)),
        );
        let spec = crate::engines::cloud::find("groq-whisper-large-v3").expect("a cloud engine");
        let secrets = SecretStore::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/test-artifacts/no-such-secrets.json"),
        );
        requests
            .send(EngineRequest::StartCloud { spec, connection_id: "groq-1".into(), secrets })
            .unwrap();
        let answer = answers.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(
            matches!(answer, Message::EngineFailed(detail) if detail == "the service has no key")
        );
        drop(requests);
        worker.join().unwrap();
    }
}
