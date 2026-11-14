//! sherpa-onnx in a process of its own: the app starts itself with
//! [`WORKER_ARG`], and that process loads the model (or the speaker models) and answers over
//! its standard input and output. Unloading ends the process, so the system takes back all of
//! its memory whatever the allocator kept; a crash of the model ends that process, not the app.
//!
//! The protocol: every message is one JSON line, followed by `samples` little-endian `f32`
//! values when the header says so. One request at a time, answered by one reply, which progress
//! lines may precede; the end of the input ends the worker.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::sherpa::{LiveStream, SherpaEngine};
use super::whisper_server::{TranscribeOptions, Transcript};
use crate::files::Word;
use crate::meetings::speakers::SpeakerModels;
use crate::meetings::transcript::SpeakerSegment;
use crate::platform;
use crate::storage::catalog::SherpaSpec;

/// The argument that makes the app a sherpa-onnx worker instead of the app.
pub const WORKER_ARG: &str = "--sherpa-worker";
/// How long a worker may take to end after its input closes before it is killed.
const STOP_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Load {
        spec: SherpaSpec,
        dir: PathBuf,
        threads: usize,
    },
    Transcribe {
        rate: u32,
        language: Option<String>,
        samples: usize,
    },
    Words {
        rate: u32,
        language: Option<String>,
        samples: usize,
    },
    LiveStart {
        language: Option<String>,
        rate: u32,
    },
    LiveFeed {
        samples: usize,
    },
    LiveFinish,
    LiveCancel,
    /// One-off: the speaker models in `dir` tell the voices of a recording apart, by sentence
    /// of `words` when there are any. The worker reads the recording from `audio` itself, so a
    /// long one is in memory once, not in both processes; without it the samples follow.
    Diarize {
        dir: PathBuf,
        words: Vec<Word>,
        #[serde(default)]
        audio: Option<PathBuf>,
        samples: usize,
    },
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
enum Reply {
    Done,
    Text {
        text: String,
    },
    /// The live stream's whole text when it changed.
    Live {
        text: Option<String>,
    },
    /// Whether the loaded model streams.
    Streaming {
        live: bool,
    },
    Words {
        json: String,
    },
    Segments {
        segments: Vec<Segment>,
    },
    /// Before the reply: the share of the request done, from 0 to 1.
    Progress {
        share: f64,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Segment {
    start: f64,
    end: f64,
    speaker: i32,
}

impl Request {
    fn samples(&self) -> usize {
        match self {
            Self::Transcribe { samples, .. }
            | Self::Words { samples, .. }
            | Self::LiveFeed { samples }
            | Self::Diarize { samples, .. } => *samples,
            _ => 0,
        }
    }
}

fn write_message(out: &mut impl Write, header: &impl Serialize, samples: &[f32]) -> io::Result<()> {
    let mut line = serde_json::to_vec(header).map_err(io::Error::other)?;
    line.push(b'\n');
    out.write_all(&line)?;
    // In blocks: a long dictation is not copied into a second buffer as a whole.
    let mut bytes = Vec::with_capacity(SAMPLE_BLOCK.min(samples.len()) * 4);
    for block in samples.chunks(SAMPLE_BLOCK) {
        bytes.clear();
        for sample in block {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        out.write_all(&bytes)?;
    }
    out.flush()
}

/// Samples converted at a time on either side of the pipe.
const SAMPLE_BLOCK: usize = 16_384;

/// The next header, or `None` at the end of the input.
fn read_header<T: for<'a> Deserialize<'a>>(input: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line).map(Some).map_err(io::Error::other)
}

fn read_samples(input: &mut impl Read, count: usize) -> io::Result<Vec<f32>> {
    let mut samples = Vec::with_capacity(count);
    let mut bytes = vec![0; SAMPLE_BLOCK.min(count) * 4];
    while samples.len() < count {
        let block = &mut bytes[..(count - samples.len()).min(SAMPLE_BLOCK) * 4];
        input.read_exact(block)?;
        samples.extend(block.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
    }
    Ok(samples)
}

/// What the worker does with a request; sherpa-onnx in the worker, a fake in tests.
trait Backend {
    fn load(&mut self, spec: &SherpaSpec, dir: &Path, threads: usize) -> Result<bool, String>;
    fn transcribe(
        &self,
        samples: &[f32],
        rate: u32,
        language: Option<String>,
    ) -> Result<String, String>;
    fn words(&self, samples: &[f32], rate: u32, language: Option<String>)
    -> Result<String, String>;
    fn live_start(&mut self, language: Option<&str>, rate: u32) -> bool;
    fn live_feed(&mut self, samples: &[f32]) -> Option<String>;
    fn live_finish(&mut self) -> Result<String, String>;
    fn live_cancel(&mut self);
    fn diarize(
        &self,
        dir: &Path,
        samples: &[f32],
        words: &[Word],
        progress: &mut dyn FnMut(f64),
    ) -> Result<Vec<SpeakerSegment>, String>;
}

/// Answers requests until the input ends.
fn serve(backend: &mut impl Backend, input: impl Read, mut output: impl Write) -> io::Result<()> {
    let mut input = BufReader::new(input);
    while let Some(request) = read_header::<Request>(&mut input)? {
        let samples = read_samples(&mut input, request.samples())?;
        let error = |message: String| Reply::Error { message };
        let reply = match request {
            Request::Load { spec, dir, threads } => match backend.load(&spec, &dir, threads) {
                Ok(live) => Reply::Streaming { live },
                Err(message) => error(message),
            },
            Request::Transcribe { rate, language, .. } => backend
                .transcribe(&samples, rate, language)
                .map_or_else(error, |text| Reply::Text { text }),
            Request::Words { rate, language, .. } => backend
                .words(&samples, rate, language)
                .map_or_else(error, |json| Reply::Words { json }),
            Request::LiveStart { language, rate } => {
                Reply::Streaming { live: backend.live_start(language.as_deref(), rate) }
            }
            Request::LiveFeed { .. } => Reply::Live { text: backend.live_feed(&samples) },
            Request::LiveFinish => {
                backend.live_finish().map_or_else(error, |text| Reply::Text { text })
            }
            Request::LiveCancel => {
                backend.live_cancel();
                Reply::Done
            }
            Request::Diarize { dir, words, audio, .. } => {
                let samples = match audio {
                    Some(path) => crate::files::decode(&path)
                        .map(|decoded| decoded.samples)
                        .map_err(|error| error.to_string()),
                    None => Ok(samples),
                };
                let samples = match samples {
                    Ok(samples) => samples,
                    Err(message) => {
                        write_message(&mut output, &error(message), &[])?;
                        continue;
                    }
                };
                // The first line at once (the second model has started), then one for each
                // whole percent, not for each step.
                let mut percent: Option<u32> = None;
                let mut report = |share: f64| {
                    let now = (share.clamp(0.0, 1.0) * 100.0) as u32;
                    if percent.is_none_or(|before| now > before) {
                        percent = Some(now);
                        let written = write_message(&mut output, &Reply::Progress { share }, &[]);
                        // Nobody reads any more (the app quit or crashed): a worker process
                        // must not go on computing for minutes.
                        if written.is_err() && WORKER_PROCESS.load(Ordering::Relaxed) {
                            std::process::exit(1);
                        }
                    }
                };
                match backend.diarize(&dir, &samples, &words, &mut report) {
                    Ok(segments) => Reply::Segments {
                        segments: segments
                            .into_iter()
                            .map(|s| Segment { start: s.start, end: s.end, speaker: s.speaker })
                            .collect(),
                    },
                    Err(message) => error(message),
                }
            }
        };
        write_message(&mut output, &reply, &[])?;
    }
    Ok(())
}

/// The real backend. The live stream is declared first, so it is dropped before the engine.
#[derive(Default)]
struct Sherpa {
    live: Option<LiveStream>,
    engine: Option<SherpaEngine>,
}

impl Sherpa {
    fn engine(&self) -> Result<&SherpaEngine, String> {
        self.engine.as_ref().ok_or_else(|| "no model is loaded".to_string())
    }
}

fn options(language: Option<String>) -> TranscribeOptions {
    TranscribeOptions { language, ..TranscribeOptions::default() }
}

impl Backend for Sherpa {
    fn load(&mut self, spec: &SherpaSpec, dir: &Path, threads: usize) -> Result<bool, String> {
        self.live = None;
        let engine = SherpaEngine::load(spec, dir, threads)?;
        let streams = matches!(engine, SherpaEngine::Online(_));
        self.engine = Some(engine);
        Ok(streams)
    }

    fn transcribe(
        &self,
        samples: &[f32],
        rate: u32,
        language: Option<String>,
    ) -> Result<String, String> {
        self.engine()?.transcribe(samples, rate, &options(language)).map(|t| t.text)
    }

    fn words(
        &self,
        samples: &[f32],
        rate: u32,
        language: Option<String>,
    ) -> Result<String, String> {
        self.engine()?.transcribe_words(samples, rate, &options(language))
    }

    fn live_start(&mut self, language: Option<&str>, rate: u32) -> bool {
        self.live = None;
        self.live = self.engine.as_ref().and_then(|engine| engine.start_live(language, rate));
        self.live.is_some()
    }

    fn live_feed(&mut self, samples: &[f32]) -> Option<String> {
        let (engine, live) = (self.engine.as_ref()?, self.live.as_mut()?);
        engine.feed_live(live, samples)
    }

    fn live_finish(&mut self) -> Result<String, String> {
        let live = self.live.take().ok_or("no live stream")?;
        self.engine()?.finish_live(live).map(|t| t.text)
    }

    fn live_cancel(&mut self) {
        self.live = None;
    }

    fn diarize(
        &self,
        dir: &Path,
        samples: &[f32],
        words: &[Word],
        progress: &mut dyn FnMut(f64),
    ) -> Result<Vec<SpeakerSegment>, String> {
        SpeakerModels::new(dir.to_path_buf()).diarize_here(samples, words, progress)
    }
}

/// Runs the worker when the app was started as one: call first in `main`. `true` means the
/// worker ran and the process should end.
pub fn run_worker_if_asked() -> bool {
    if std::env::args_os().nth(1).is_none_or(|arg| arg != WORKER_ARG) {
        return false;
    }
    WORKER_PROCESS.store(true, Ordering::Relaxed);
    let input: Box<dyn Read> = match input_ending_the_process() {
        Ok(input) => Box::new(input),
        Err(_) => Box::new(io::stdin().lock()),
    };
    let result = serve(&mut Sherpa::default(), input, io::stdout().lock());
    if let Err(error) = result {
        log_line!("sherpa worker: {error}");
    }
    true
}

/// The app's requests, passed on by a thread that reads the worker's input: when the input ends
/// (the app stopped the worker, or the app is gone) the process ends at once, also in the
/// middle of a request, which reads no input for minutes. The app keeps the input open while it
/// waits for an answer.
fn input_ending_the_process() -> io::Result<io::PipeReader> {
    let (reader, mut writer) = io::pipe()?;
    std::thread::spawn(move || {
        let _ = io::copy(&mut io::stdin().lock(), &mut writer);
        std::process::exit(0);
    });
    Ok(reader)
}

/// This process is a worker (not the app, and not a test's thread standing in for one).
static WORKER_PROCESS: AtomicBool = AtomicBool::new(false);

/// A worker and the pipes to it; `child` is its process (a thread stands in for it in tests).
struct Worker {
    child: Option<Child>,
    input: Box<dyn Write + Send>,
    output: BufReader<Box<dyn Read + Send>>,
}

impl Worker {
    /// `log` gets what sherpa-onnx and ONNX Runtime write (why a model does not load); without
    /// one it goes where the app's own output goes.
    fn spawn(log: Option<&Path>) -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| format!("cannot find the app: {e}"))?;
        Self::spawn_from(&exe, log)
    }

    fn spawn_from(exe: &Path, log: Option<&Path>) -> Result<Self, String> {
        let errors = match log {
            Some(log) => Stdio::from(
                super::local_server::open_log(log)
                    .map_err(|e| format!("cannot open the engine log: {e}"))?,
            ),
            None => Stdio::inherit(),
        };
        let mut command = Command::new(exe);
        command.arg(WORKER_ARG).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(errors);
        platform::hide_console(&mut command);
        let mut child =
            command.spawn().map_err(|e| format!("cannot start the speech worker: {e}"))?;
        let input = child.stdin.take().ok_or("the speech worker has no input")?;
        let output = child.stdout.take().ok_or("the speech worker has no output")?;
        Ok(Self {
            child: Some(child),
            input: Box::new(input),
            output: BufReader::new(Box::new(output)),
        })
    }

    fn ask(&mut self, request: &Request, samples: &[f32]) -> Result<Reply, String> {
        self.ask_with_progress(request, samples, &mut |_| {})
    }

    /// [`Self::ask`]; `progress` gets the progress lines before the reply.
    fn ask_with_progress(
        &mut self,
        request: &Request,
        samples: &[f32],
        progress: &mut dyn FnMut(f64),
    ) -> Result<Reply, String> {
        let lost = |e: io::Error| format!("the speech worker stopped: {e}");
        write_message(&mut self.input, request, samples).map_err(lost)?;
        loop {
            match read_header::<Reply>(&mut self.output).map_err(lost)? {
                Some(Reply::Progress { share }) => progress(share),
                Some(Reply::Error { message }) => return Err(message),
                Some(reply) => return Ok(reply),
                None => return Err("the speech worker stopped".into()),
            }
        }
    }

    /// [`Self::ask_with_progress`], stopped by setting `cancel`: a worker that loads a model or
    /// computes for minutes reads no input, so a watcher kills it while this thread waits.
    /// [`STOPPED`] is the error then.
    fn ask_cancellable(
        &mut self,
        request: &Request,
        samples: &[f32],
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(f64),
    ) -> Result<Reply, String> {
        let mut child = self.child.take();
        let done = AtomicBool::new(false);
        let reply = std::thread::scope(|scope| {
            scope.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    if cancel.load(Ordering::Relaxed) {
                        if let Some(child) = child.as_mut() {
                            let _ = child.kill();
                        }
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
            let reply = self.ask_with_progress(request, samples, progress);
            done.store(true, Ordering::Relaxed);
            reply
        });
        self.child = child;
        if cancel.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        reply
    }

    /// Closes the input, waits briefly for the worker to end, else kills it.
    fn stop(self) {
        drop(self.input);
        let Some(mut child) = self.child else { return };
        let until = Instant::now() + STOP_WAIT;
        while Instant::now() < until {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Names the kind of reply only: a reply can carry dictated text, which stays out of errors.
fn unexpected(reply: Reply) -> String {
    let kind = match reply {
        Reply::Done => "done",
        Reply::Text { .. } => "text",
        Reply::Live { .. } => "live text",
        Reply::Streaming { .. } => "streaming",
        Reply::Words { .. } => "words",
        Reply::Segments { .. } => "segments",
        Reply::Progress { .. } => "progress",
        Reply::Error { .. } => "an error",
    };
    format!("the speech worker answered with {kind} where something else was due")
}

/// A loaded sherpa-onnx model in its worker process.
pub struct SherpaProcess {
    worker: Option<Worker>,
    streams: bool,
}

impl SherpaProcess {
    /// Loads the model in a new worker writing to `log`; setting `quit` ends a load that is
    /// still going.
    pub fn start(
        spec: &SherpaSpec,
        dir: &Path,
        threads: usize,
        log: &Path,
        quit: &AtomicBool,
    ) -> Result<Self, String> {
        Self::start_with(Worker::spawn(Some(log))?, spec, dir, threads, quit)
    }

    fn start_with(
        mut worker: Worker,
        spec: &SherpaSpec,
        dir: &Path,
        threads: usize,
        quit: &AtomicBool,
    ) -> Result<Self, String> {
        let request = Request::Load { spec: spec.clone(), dir: dir.to_path_buf(), threads };
        match worker.ask_cancellable(&request, &[], quit, &mut |_| {}) {
            Ok(Reply::Streaming { live }) => Ok(Self { worker: Some(worker), streams: live }),
            Ok(reply) => {
                worker.stop();
                Err(unexpected(reply))
            }
            Err(error) => {
                worker.stop();
                Err(error)
            }
        }
    }

    fn worker(&mut self) -> Result<&mut Worker, String> {
        self.worker.as_mut().ok_or_else(|| "the speech worker stopped".to_string())
    }

    fn text(reply: Reply) -> Result<Transcript, String> {
        match reply {
            Reply::Text { text } => Ok(Transcript { text, language: None }),
            other => Err(unexpected(other)),
        }
    }

    pub fn transcribe(
        &mut self,
        samples: &[f32],
        rate: u32,
        options: &TranscribeOptions,
    ) -> Result<Transcript, String> {
        let request = Request::Transcribe {
            rate,
            language: options.language.clone(),
            samples: samples.len(),
        };
        Self::text(self.worker()?.ask(&request, samples)?)
    }

    pub fn transcribe_words(
        &mut self,
        samples: &[f32],
        rate: u32,
        options: &TranscribeOptions,
    ) -> Result<String, String> {
        let request =
            Request::Words { rate, language: options.language.clone(), samples: samples.len() };
        match self.worker()?.ask(&request, samples)? {
            Reply::Words { json } => Ok(json),
            other => Err(unexpected(other)),
        }
    }

    /// Starts the live stream of a dictation; `false` for a model that does not stream.
    pub fn start_live(&mut self, language: Option<&str>, rate: u32) -> bool {
        if !self.streams {
            return false;
        }
        let request = Request::LiveStart { language: language.map(String::from), rate };
        let Ok(worker) = self.worker() else { return false };
        matches!(worker.ask(&request, &[]), Ok(Reply::Streaming { live: true }))
    }

    pub fn feed_live(&mut self, samples: &[f32]) -> Option<String> {
        let request = Request::LiveFeed { samples: samples.len() };
        match self.worker().ok()?.ask(&request, samples) {
            Ok(Reply::Live { text }) => text,
            _ => None,
        }
    }

    pub fn finish_live(&mut self) -> Result<Transcript, String> {
        Self::text(self.worker()?.ask(&Request::LiveFinish, &[])?)
    }

    pub fn cancel_live(&mut self) {
        if let Ok(worker) = self.worker() {
            let _ = worker.ask(&Request::LiveCancel, &[]);
        }
    }

    /// After a failed request: whether the worker died (a crash of the model).
    pub fn died(&mut self) -> bool {
        match self.worker.as_mut().and_then(|worker| worker.child.as_mut()) {
            Some(child) => super::local_server::died(child),
            None => false,
        }
    }

    /// Ends the worker: the system takes back all of the model's memory.
    pub fn stop(mut self) {
        if let Some(worker) = self.worker.take() {
            worker.stop();
        }
    }
}

impl Drop for SherpaProcess {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.stop();
        }
    }
}

/// Tells the voices of the recording `audio` apart in a one-off worker, which reads the file
/// itself and then ends; by sentence of `words` when there are any. `progress` gets the share
/// done, from 0 to 1. Setting `cancel` kills the worker: [`STOPPED`] is the error then.
pub fn diarize(
    dir: &Path,
    audio: &Path,
    words: &[Word],
    log: Option<&Path>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<Vec<SpeakerSegment>, String> {
    let request = Request::Diarize {
        dir: dir.to_path_buf(),
        words: words.to_vec(),
        audio: Some(audio.to_path_buf()),
        samples: 0,
    };
    diarize_with(Worker::spawn(log)?, &request, &[], cancel, progress)
}

/// The error of a diarization that was stopped.
pub const STOPPED: &str = "stopped";

fn diarize_with(
    mut worker: Worker,
    request: &Request,
    samples: &[f32],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<Vec<SpeakerSegment>, String> {
    let reply = worker.ask_cancellable(request, samples, cancel, progress);
    worker.stop();
    match reply? {
        Reply::Segments { segments } => Ok(segments
            .into_iter()
            .map(|s| SpeakerSegment { start: s.start, end: s.end, speaker: s.speaker })
            .collect()),
        other => Err(unexpected(other)),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::storage::catalog::Recognizer;

    /// Records what it is asked; streams when the spec's model type says "online".
    #[derive(Default)]
    struct Fake {
        /// A request computes for a long time.
        slow: bool,
        loaded: bool,
        streams: bool,
        live: Option<Vec<f32>>,
        seen: RefCell<Vec<String>>,
    }

    impl Backend for Fake {
        fn load(&mut self, spec: &SherpaSpec, _dir: &Path, threads: usize) -> Result<bool, String> {
            if spec.encoder == "missing" {
                return Err("sherpa-onnx cannot load the model".into());
            }
            self.loaded = true;
            self.streams = spec.recognizer == Recognizer::OnlineTransducer;
            self.seen.borrow_mut().push(format!("load {threads}"));
            Ok(self.streams)
        }
        fn transcribe(
            &self,
            samples: &[f32],
            rate: u32,
            language: Option<String>,
        ) -> Result<String, String> {
            if self.slow {
                std::thread::sleep(Duration::from_secs(60));
            }
            Ok(format!("{} samples at {rate} in {}", samples.len(), language.unwrap_or_default()))
        }
        fn words(
            &self,
            samples: &[f32],
            _rate: u32,
            _language: Option<String>,
        ) -> Result<String, String> {
            Ok(format!("{{\"sum\":{}}}", samples.iter().sum::<f32>()))
        }
        fn live_start(&mut self, _language: Option<&str>, _rate: u32) -> bool {
            self.live = self.streams.then(Vec::new);
            self.live.is_some()
        }
        fn live_feed(&mut self, samples: &[f32]) -> Option<String> {
            let live = self.live.as_mut()?;
            live.extend_from_slice(samples);
            Some(format!("{} heard", live.len()))
        }
        fn live_finish(&mut self) -> Result<String, String> {
            let live = self.live.take().ok_or("no live stream")?;
            Ok(format!("{} in all", live.len()))
        }
        fn live_cancel(&mut self) {
            self.live = None;
        }
        /// One segment for the whole audio, or one per word with its index as the voice; the
        /// progress in thousandths, then halfway twice.
        fn diarize(
            &self,
            _dir: &Path,
            samples: &[f32],
            words: &[Word],
            progress: &mut dyn FnMut(f64),
        ) -> Result<Vec<SpeakerSegment>, String> {
            for share in [0.001, 0.004, 0.5, 0.5] {
                progress(share);
            }
            if words.is_empty() {
                return Ok(vec![SpeakerSegment {
                    start: 0.0,
                    end: samples.len() as f64 / 16_000.0,
                    speaker: 3,
                }]);
            }
            Ok(words
                .iter()
                .enumerate()
                .map(|(i, w)| SpeakerSegment { start: w.start, end: w.end, speaker: i as i32 })
                .collect())
        }
    }

    fn spec(recognizer: Recognizer, encoder: &str) -> SherpaSpec {
        SherpaSpec {
            recognizer,
            model_type: None,
            encoder: encoder.into(),
            decoder: "d".into(),
            joiner: "j".into(),
            tokens: "t".into(),
        }
    }

    /// A worker on a thread with pipes, as the app has one in a process.
    fn worker(fake: Fake) -> (Worker, std::thread::JoinHandle<()>) {
        let (to_worker_read, to_worker_write) = io::pipe().unwrap();
        let (from_worker_read, from_worker_write) = io::pipe().unwrap();
        let thread = std::thread::spawn(move || {
            let mut fake = fake;
            serve(&mut fake, to_worker_read, from_worker_write).unwrap();
        });
        let worker = Worker {
            child: None,
            input: Box::new(to_worker_write),
            output: BufReader::new(Box::new(from_worker_read)),
        };
        (worker, thread)
    }

    /// A worker in the middle of a long request ends once its input ends: the app that
    /// stopped it or went away does not leave it computing. In a process of its own.
    #[test]
    fn a_worker_ends_with_its_input_also_mid_request() {
        if std::env::var_os("AHEM_TEST_WORKER_INPUT").is_some() {
            let input = input_ending_the_process().unwrap();
            let _ = serve(&mut Fake { slow: true, ..Fake::default() }, input, io::sink());
            return;
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "engines::sherpa_process::tests::a_worker_ends_with_its_input_also_mid_request",
                "--exact",
                "--nocapture",
            ])
            .env("AHEM_TEST_WORKER_INPUT", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let request = Request::Transcribe { rate: 16_000, language: None, samples: 10 };
        write_message(&mut input, &request, &[0.0; 10]).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(child.try_wait().unwrap().is_none(), "the worker computes, not ended or failed");
        drop(input);
        let until = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() > until {
                let _ = child.kill();
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let status = status.expect("still computing after its input ended");
        assert!(status.success(), "{status}");
    }

    /// A worker that died is noticed by the request that failed with it.
    #[cfg(unix)]
    #[test]
    fn a_worker_that_died_is_noticed() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 0.1; exit 1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let worker = Worker {
            child: Some(child),
            input: Box::new(input),
            output: BufReader::new(Box::new(output)),
        };
        let mut engine = SherpaProcess { worker: Some(worker), streams: false };
        assert!(engine.died());
    }

    #[test]
    fn samples_cross_the_pipe_whole_in_blocks() {
        let samples: Vec<f32> = (0..SAMPLE_BLOCK * 2 + 7).map(|i| i as f32 * 0.5).collect();
        let mut pipe = Vec::new();
        write_message(&mut pipe, &Request::LiveFeed { samples: samples.len() }, &samples).unwrap();
        let mut input = std::io::Cursor::new(pipe);
        let header: Request = read_header(&mut input).unwrap().unwrap();
        assert_eq!(read_samples(&mut input, header.samples()).unwrap(), samples);
        assert!(read_samples(&mut input, 0).unwrap().is_empty());
    }

    #[test]
    fn a_model_transcribes_in_the_worker() {
        let (worker, thread) = worker(Fake::default());
        let mut engine = SherpaProcess::start_with(
            worker,
            &spec(Recognizer::OfflineTransducer, "e"),
            Path::new("/m"),
            4,
            &never(),
        )
        .unwrap();
        let options =
            TranscribeOptions { language: Some("ru".into()), ..TranscribeOptions::default() };
        let text = engine.transcribe(&[0.5; 48_000], 48_000, &options).unwrap();
        assert_eq!(
            text,
            Transcript { text: "48000 samples at 48000 in ru".into(), language: None }
        );
        // The samples arrive as they were sent.
        let json = engine.transcribe_words(&[0.25, -1.0, 2.5], 16_000, &options).unwrap();
        assert_eq!(json, "{\"sum\":1.75}");
        // An offline model does not stream.
        assert!(!engine.start_live(None, 48_000));
        engine.stop();
        thread.join().unwrap();
    }

    #[test]
    fn a_streaming_model_takes_a_dictation_while_it_is_recorded() {
        let (worker, thread) = worker(Fake::default());
        let mut engine = SherpaProcess::start_with(
            worker,
            &spec(Recognizer::OnlineTransducer, "e"),
            Path::new("/m"),
            2,
            &never(),
        )
        .unwrap();
        assert!(engine.start_live(Some("en"), 48_000));
        assert_eq!(engine.feed_live(&[0.0; 100]).as_deref(), Some("100 heard"));
        assert_eq!(engine.feed_live(&[0.0; 50]).as_deref(), Some("150 heard"));
        assert_eq!(engine.finish_live().unwrap().text, "150 in all");
        // A cancelled stream finishes nothing.
        assert!(engine.start_live(None, 48_000));
        engine.cancel_live();
        assert!(engine.finish_live().is_err());
        drop(engine);
        thread.join().unwrap();
    }

    #[test]
    fn a_model_that_does_not_load_reports_why() {
        let (worker, thread) = worker(Fake::default());
        let error = SherpaProcess::start_with(
            worker,
            &spec(Recognizer::OfflineTransducer, "missing"),
            Path::new("/m"),
            1,
            &never(),
        );
        assert_eq!(error.err().as_deref(), Some("sherpa-onnx cannot load the model"));
        thread.join().unwrap();
    }

    #[test]
    fn a_load_stopped_for_quitting_gives_no_engine() {
        let (worker, thread) = worker(Fake::default());
        let quit = AtomicBool::new(true);
        let spec = spec(Recognizer::OfflineTransducer, "e");
        let error = SherpaProcess::start_with(worker, &spec, Path::new("/m"), 1, &quit);
        assert_eq!(error.err().as_deref(), Some(STOPPED));
        thread.join().unwrap();
    }

    fn never() -> AtomicBool {
        AtomicBool::new(false)
    }

    /// The request with the samples sent along, as the fake backend needs them.
    fn of_samples(samples: &[f32], words: &[Word]) -> Request {
        Request::Diarize {
            dir: PathBuf::from("/speakers"),
            words: words.to_vec(),
            audio: None,
            samples: samples.len(),
        }
    }

    #[test]
    fn voices_are_told_apart_in_a_one_off_worker() {
        let (one_off, thread) = worker(Fake::default());
        let mut shares = Vec::new();
        let two_seconds = [0.0; 32_000];
        let segments = diarize_with(
            one_off,
            &of_samples(&two_seconds, &[]),
            &two_seconds,
            &never(),
            &mut |s| shares.push(s),
        )
        .unwrap();
        assert_eq!(segments, [SpeakerSegment { start: 0.0, end: 2.0, speaker: 3 }]);
        // The progress comes before the reply: the first at once, then once a whole percent
        // more is done.
        assert_eq!(shares, [0.001, 0.5]);
        thread.join().unwrap();
        // The words of the transcript reach the worker.
        let (one_off, thread) = worker(Fake::default());
        let words = [
            Word { start: 0.0, end: 0.5, text: " Why?".into() },
            Word { start: 0.6, end: 1.0, text: " Rarely.".into() },
        ];
        let second = [0.0; 16_000];
        let segments =
            diarize_with(one_off, &of_samples(&second, &words), &second, &never(), &mut |_| {})
                .unwrap();
        // A stop set before the reply is read is the result, whatever the worker answered.
        let (one_off, stopped_thread) = worker(Fake::default());
        let stop = AtomicBool::new(true);
        let stopped = diarize_with(one_off, &of_samples(&second, &[]), &second, &stop, &mut |_| {});
        assert_eq!(stopped, Err(STOPPED.to_string()));
        stopped_thread.join().unwrap();
        assert_eq!(
            segments,
            [
                SpeakerSegment { start: 0.0, end: 0.5, speaker: 0 },
                SpeakerSegment { start: 0.6, end: 1.0, speaker: 1 },
            ]
        );
        thread.join().unwrap();
    }

    #[test]
    fn the_worker_ends_with_its_input_and_reports_a_lost_worker() {
        let mut fake = Fake::default();
        // Input that ends after a load request: one reply, then the loop ends.
        let mut input = Vec::new();
        write_message(
            &mut input,
            &Request::Load {
                spec: spec(Recognizer::OfflineTransducer, "e"),
                dir: "/m".into(),
                threads: 1,
            },
            &[],
        )
        .unwrap();
        let mut output = Vec::new();
        serve(&mut fake, &input[..], &mut output).unwrap();
        let reply: Reply = serde_json::from_slice(output.strip_suffix(b"\n").unwrap()).unwrap();
        assert_eq!(reply, Reply::Streaming { live: false });
        // A request cut short (fewer samples than announced) is an error, not a hang.
        let mut cut = Vec::new();
        write_message(
            &mut cut,
            &Request::Transcribe { rate: 16_000, language: None, samples: 10 },
            &[0.0; 3],
        )
        .unwrap();
        assert!(serve(&mut Fake::default(), &cut[..], Vec::new()).is_err());
    }

    /// The real worker tells the voices of a recording apart from its file, and a stop kills it
    /// at once. Build the app first (`cargo build`) and set AHEM_SPEAKERS=<the speaker models
    /// folder> and AHEM_AUDIO=<a recording of a minute or more>.
    #[test]
    #[ignore = "needs the built app and the speaker models: cargo test -- --ignored voices_in_a_real_worker --nocapture"]
    fn voices_in_a_real_worker() {
        let exe = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/ahem");
        let request = Request::Diarize {
            dir: PathBuf::from(std::env::var("AHEM_SPEAKERS").unwrap()),
            words: Vec::new(),
            audio: Some(PathBuf::from(std::env::var("AHEM_AUDIO").unwrap())),
            samples: 0,
        };
        let started = Instant::now();
        let mut shares = 0;
        let worker = Worker::spawn_from(&exe, None).unwrap();
        let segments = diarize_with(worker, &request, &[], &never(), &mut |_| shares += 1).unwrap();
        let whole = started.elapsed();
        println!(
            "{} segments, {shares} progress lines, {:.1} s",
            segments.len(),
            whole.as_secs_f64()
        );
        assert!(!segments.is_empty() && shares > 0);

        // Stopped a second in: the worker is killed, long before it would have finished.
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        let stopped = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_secs(1));
                stop.store(true, Ordering::Relaxed);
            });
            let worker = Worker::spawn_from(&exe, None).unwrap();
            diarize_with(worker, &request, &[], &stop, &mut |_| {})
        });
        println!("stopped after {:.1} s", started.elapsed().as_secs_f64());
        assert_eq!(stopped, Err(STOPPED.to_string()));
        assert!(started.elapsed() < whole / 2 + Duration::from_secs(2));
    }

    /// The real worker: the app binary, a model and a recording. Build the app first
    /// (`cargo build`) and set AHEM_MODEL_DIR=<a sherpa-onnx model folder named by its id> and
    /// AHEM_AUDIO=<a recording>. A streaming model is also fed like a dictation, 0.2 s at a time,
    /// in the worker and in this process, to compare how long each piece takes.
    #[test]
    #[ignore = "needs the built app and a model: cargo test -- --ignored worker_end_to_end --nocapture"]
    fn worker_end_to_end() {
        use super::super::sherpa::SherpaEngine;
        let exe = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/ahem");
        let dir = PathBuf::from(std::env::var("AHEM_MODEL_DIR").unwrap());
        let id = dir.file_name().unwrap().to_string_lossy().into_owned();
        let spec = crate::storage::catalog::find(&id).unwrap().sherpa.unwrap();
        let audio = crate::files::decode(Path::new(&std::env::var("AHEM_AUDIO").unwrap())).unwrap();
        let samples = &audio.samples[..audio.samples.len().min(16_000 * 20)];
        let started = Instant::now();
        let mut engine = SherpaProcess::start_with(
            Worker::spawn_from(&exe, None).unwrap(),
            &spec,
            &dir,
            4,
            &never(),
        )
        .unwrap();
        println!("loaded in {:.2} s", started.elapsed().as_secs_f64());
        let started = Instant::now();
        let text = engine.transcribe(samples, 16_000, &TranscribeOptions::default()).unwrap();
        println!("{:.2} s: {}", started.elapsed().as_secs_f64(), text.text);
        assert!(!text.text.is_empty());
        if engine.start_live(None, 16_000) {
            let mut worst = Duration::ZERO;
            let mut total = Duration::ZERO;
            for piece in samples.chunks(3_200) {
                let started = Instant::now();
                engine.feed_live(piece);
                let took = started.elapsed();
                worst = worst.max(took);
                total += took;
            }
            let streamed = engine.finish_live().unwrap();
            let pieces = samples.chunks(3_200).count() as u32;
            println!(
                "worker: {pieces} pieces, {:.1} ms each, worst {:.1} ms: {}",
                total.as_secs_f64() * 1000.0 / f64::from(pieces),
                worst.as_secs_f64() * 1000.0,
                streamed.text
            );
            let here = SherpaEngine::load(&spec, &dir, 4).unwrap();
            let mut live = here.start_live(None, 16_000).unwrap();
            let mut total_here = Duration::ZERO;
            for piece in samples.chunks(3_200) {
                let started = Instant::now();
                here.feed_live(&mut live, piece);
                total_here += started.elapsed();
            }
            let text_here = here.finish_live(live).unwrap();
            println!(
                "in this process: {:.1} ms each",
                total_here.as_secs_f64() * 1000.0 / f64::from(pieces)
            );
            assert_eq!(streamed.text, text_here.text, "the worker streams like the app did");
        }
        engine.stop();
    }
}
