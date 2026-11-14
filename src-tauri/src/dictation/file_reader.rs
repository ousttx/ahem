//! The thread that decodes a file block by block and cuts it into engine windows, one window per
//! request, so only about one window of audio (under 32 s) is held at a time.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use super::Message;
use crate::audio::capture::Captured;
use crate::audio::resample::TARGET_RATE;
use crate::domain::scheduler::FileTaskId;
use crate::files::{FileDecoder, Planner, Window};
use crate::platform;

/// Starts reading and sends the first window at once; every `()` on the returned sender asks for
/// the next one. Replies with `FileWindow` (the last one has `window.last`), `FileEnded` when
/// the audio ends without a last window, or `FileReadFailed`. Dropping the sender stops it.
pub fn spawn(file: FileTaskId, path: PathBuf, replies: Sender<Message>) -> Sender<()> {
    let (requests, inbox) = mpsc::channel::<()>();
    thread::spawn(move || {
        guarded(file, &replies, || read(file, &path, &replies, &inbox));
        // The decoder and its audio are gone.
        platform::release_memory();
    });
    requests
}

/// Runs `work`; a panic in it (a decoder on a damaged file) fails the file instead of leaving
/// it waiting for windows that never come.
fn guarded(file: FileTaskId, replies: &Sender<Message>, work: impl FnOnce()) {
    if panic::catch_unwind(AssertUnwindSafe(work)).is_err() {
        let detail = "the file could not be decoded".to_string();
        let _ = replies.send(Message::FileReadFailed { file, detail });
    }
}

fn read(file: FileTaskId, path: &Path, replies: &Sender<Message>, inbox: &Receiver<()>) {
    let fail = |detail: String| {
        let _ = replies.send(Message::FileReadFailed { file, detail });
    };
    let mut decoder = match FileDecoder::open(path) {
        Ok(decoder) => decoder,
        Err(error) => return fail(error.to_string()),
    };
    let duration = decoder.duration_hint();
    let mut planner = Planner::new();
    let mut buffer: Vec<f32> = Vec::new();
    let mut buffer_start = 0;
    let mut complete = false;
    let mut first = true;
    loop {
        if !first && inbox.recv().is_err() {
            return;
        }
        first = false;
        let window = loop {
            if let Some(window) = planner.next(&buffer[planner.start() - buffer_start..], complete)
            {
                break Some(window);
            }
            if complete {
                break None;
            }
            match decoder.next_block() {
                Ok(Some(block)) => buffer.extend_from_slice(&block),
                Ok(None) => complete = true,
                Err(error) => return fail(error.to_string()),
            }
        };
        let Some(window) = window else {
            let _ = replies.send(Message::FileEnded { file });
            return;
        };
        // The window's audio, indexed in the buffer instead of the whole file.
        let local =
            Window { start: window.start - buffer_start, end: window.end - buffer_start, ..window };
        let samples = local.audio(&buffer).into_owned();
        buffer.drain(..planner.start() - buffer_start);
        buffer_start = planner.start();
        let audio = Captured { samples, rate: TARGET_RATE };
        let _ = replies.send(Message::FileWindow { file, window, audio, duration });
        if window.last {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use super::*;
    use crate::audio::wav;

    fn test_file(name: &str, seconds: f32) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts/file-reader");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}-{}.wav", std::process::id()));
        let samples: Vec<f32> =
            (0..(16_000.0 * seconds) as usize).map(|i| 0.2 * (i as f32 * 0.05).sin()).collect();
        fs::write(&path, wav::encode(&samples, 16_000)).unwrap();
        path
    }

    fn next(inbox: &mpsc::Receiver<Message>) -> Message {
        inbox.recv_timeout(Duration::from_secs(10)).expect("a reply")
    }

    #[test]
    fn windows_come_one_per_request() {
        let path = test_file("long", 70.0);
        let (replies, inbox) = mpsc::channel();
        let requests = spawn(FileTaskId(1), path, replies);
        let mut windows = Vec::new();
        loop {
            match next(&inbox) {
                Message::FileWindow { window, audio, duration, .. } => {
                    assert_eq!(audio.samples.len(), window.end - window.start);
                    assert!(duration.is_some());
                    windows.push(window);
                    assert!(inbox.try_recv().is_err(), "nothing until the next request");
                    if window.last {
                        break;
                    }
                    requests.send(()).unwrap();
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(windows.len(), 3);
        assert_eq!(windows.last().unwrap().keep_to, 70.0);
    }

    /// The app's own path for files on the real engine: this reader, `transcribe_raw` with word
    /// times, `parse_words`, `merge`. 67 s of synthesized speech, three windows.
    #[test]
    #[ignore = "needs the built engine, the Turbo model and test audio: cargo test -- --ignored file_through_the_engine --nocapture"]
    fn file_through_the_engine() {
        use std::sync::atomic::AtomicBool;

        use crate::engines::whisper_server::{
            ServerConfig, TranscribeOptions, WhisperServer, dev_engine_dir, executable_in,
        };
        use crate::files::{ExportFormat, decode, export, join_text, merge, parse_words};

        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let runtimes = manifest.join("..").join("runtimes");
        let speech = decode(&runtimes.join("test-audio/en-long.wav")).unwrap().samples;
        let fox = decode(&runtimes.join("test-audio/en-short.wav")).unwrap().samples;
        let audio = [&speech[..], &fox[..], &speech[..]].concat();
        let dir = manifest.join("target/test-artifacts/file-reader");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("long-speech.wav");
        fs::write(&path, wav::encode(&audio, 16_000)).unwrap();

        let config = ServerConfig {
            executable: executable_in(&dev_engine_dir()),
            model: runtimes.join("models/whisper-large-v3-turbo/ggml-large-v3-turbo.bin"),
            log: manifest.join("target/test-artifacts/engine-files.log"),
            threads: ServerConfig::default_threads(),
        };
        let server = WhisperServer::start(&config, &AtomicBool::new(false)).unwrap();
        let options = TranscribeOptions { words: true, ..Default::default() };
        let (replies, inbox) = mpsc::channel();
        let requests = spawn(FileTaskId(1), path, replies);
        let mut parts = Vec::new();
        loop {
            let Message::FileWindow { window, audio, .. } = next(&inbox) else {
                panic!("expected a window")
            };
            let json = server.transcribe_raw(&audio.to_wav(), &options).unwrap();
            parts.push((window, parse_words(&json, &window).unwrap()));
            if window.last {
                break;
            }
            requests.send(()).unwrap();
        }
        server.stop().unwrap();
        assert_eq!(parts.len(), 3);
        let words = merge(parts);
        print!("{}", export(&words, &ExportFormat::Srt));
        let text = join_text(&words).to_lowercase();
        for word in ["voice", "hotkey", "cursor", "dictation"] {
            assert_eq!(text.matches(word).count(), 2, "{word} in {text}");
        }
        assert_eq!(text.matches("fox").count(), 1, "{text}");
    }

    #[test]
    fn unreadable_files_fail() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts/file-reader");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-audio.wav");
        fs::write(&path, "plain text").unwrap();
        let (replies, inbox) = mpsc::channel();
        let _requests = spawn(FileTaskId(2), path, replies);
        assert!(matches!(next(&inbox), Message::FileReadFailed { .. }));
    }

    /// A decoder that panics on a damaged file fails the file: the job does not wait for ever.
    #[test]
    fn a_panic_while_reading_fails_the_file() {
        let (replies, messages) = mpsc::channel();
        guarded(FileTaskId(7), &replies, || panic!("damaged file"));
        let failed = messages.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(failed, Message::FileReadFailed { file: FileTaskId(7), .. }));
    }
}
