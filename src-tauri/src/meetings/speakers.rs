//! Telling the voices of the computer's sound apart: sherpa-onnx's offline speaker
//! diarization with pyannote segmentation 3.0 (MIT) and 3D-Speaker ERes2NetV2 (Apache-2.0),
//! both from the official k2-fsa releases with pinned size and SHA-256.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{CString, c_void};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
use sherpa_onnx_sys as sys;
use ureq::Agent;

use super::transcript::{SpeakerSegment, sentences, voice_at};
use crate::audio::resample::TARGET_RATE;
use crate::files::Word;
use crate::runtimes::download::{self, DownloadError, Expected};

/// The id the download events carry.
pub const MODELS_ID: &str = "speakers";

struct Asset {
    /// What the interface calls it.
    name: &'static str,
    url: &'static str,
    size: u64,
    sha256: &'static str,
    /// The file name in the models folder.
    file: &'static str,
    /// For an archive: the file taken out of its top folder.
    inner: Option<&'static str>,
}

/// Where each voice speaks (MIT).
const SEGMENTATION: Asset = Asset {
    name: "pyannote segmentation 3.0",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2",
    size: 6_958_444,
    sha256: "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488",
    file: "segmentation.onnx",
    inner: Some("model.int8.onnx"),
};

/// What each voice sounds like (Apache-2.0).
const EMBEDDING: Asset = Asset {
    name: "3D-Speaker ERes2NetV2",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2netv2_sv_zh-cn_16k-common.onnx",
    size: 71_441_526,
    sha256: "bf1a75b9930474cf3389ef415e6e5d38ca96fea4a3a00f7e301d080a58ee2239",
    file: "embedding-eres2netv2.onnx",
    inner: None,
};

const ASSETS: [&Asset; 2] = [&SEGMENTATION, &EMBEDDING];
/// Written once both files are in place.
const READY: &str = "ready";
/// The CAM++ model of earlier versions: a folder with it is not ready, and the next download
/// removes it.
const OLD_FILES: [&str; 1] = ["embedding.onnx"];

/// Voices closer than this are one speaker; higher merges more. Measured 2026-11-09 on the four
/// test recordings of the sherpa-onnx `speaker-segmentation-models` release (four voices in
/// Chinese, three times two in English) and a Russian news recording with two voices:
/// ERes2NetV2 with [`merge_short_voices`] found the right voices in all five from 0.60 to 0.75.
/// CAM++, the model before, had no such threshold: 0.6 and up merged the two Russian voices,
/// 0.58 and below split an English one; six other models of the release did no better.
const CLUSTER_THRESHOLD: f32 = 0.67;
/// Voices that sound at least this alike (cosine of how they sound) are one speaker. Measured
/// 2026-11-10 on a 10-minute screen recording of a call with two speakers: the clustering gave
/// six voices with a sound, alike 0.72–0.94 within each speaker and 0.25–0.47 between them. But
/// two men of a Russian interview from YouTube (66 s, 2026-11-10) are 0.70 alike: at 0.6 they
/// were joined into one speaker, though each stretch of either sounds 0.79–0.99 like its own
/// voice and 0.41–0.75 like the other. So the line is between 0.70 and 0.72.
const SAME_VOICE: f32 = 0.71;

/// A voice heard for less than this in the whole recording is a scrap of a neighbour's speech:
/// the segmentation gives such scraps where a recording starts mid-word or voices overlap.
const SHORTEST_VOICE: f64 = 1.5;
/// The shortest stretch where a voice speaks alone that goes into how it sounds.
const SHORTEST_PART: f64 = 1.0;
/// How much of a voice, at most, goes into how it sounds, in seconds: enough to be steady,
/// bounded so that a long meeting does not take much longer.
const VOICE_SAMPLE: f64 = 120.0;
/// The shortest sentence that gets the voice it sounds like; a shorter one keeps the voice of
/// the segments.
const SHORTEST_SENTENCE: f64 = 0.6;
/// Audio taken around a sentence's words, in seconds: the engines place words a little early
/// or late.
const SENTENCE_MARGIN: f64 = 0.1;
/// The second model's progress: the share for its pass in sherpa-onnx; joining the voices and
/// the sentences take the rest. Measured 2026-11-10 on a 10.5-minute recording: about 52 s
/// against 19 s.
const FIRST_PASS: f64 = 0.75;
/// How far the segmentation window (10 s) moves each time, as a share of its length; each
/// position gets a voice sample per voice, the slowest part. Measured 2026-11-10 on a 10.5-minute
/// call with two speakers: 0.1 (the `sherpa-onnx` crate's default) took 144 s for 868 samples,
/// 0.2 took 73 s for 434 with the same two voices after joining, 0.3 left a third voice.
const WINDOW_SHIFT: f32 = 0.2;

/// Threads for both models: all but one, so the rest of the app stays responsive. Measured
/// 2026-11-10 on a Ryzen 5 7600X (6 cores, 12 threads): 12 threads instead of 6 took the voice
/// samples of a 10.5-minute recording from 144 s to 102 s.
fn threads() -> i32 {
    std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1).max(1)) as i32
}

/// A model as the interface lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModelInfo {
    pub name: &'static str,
    /// Where it is downloaded from, for the Models page to name.
    pub url: &'static str,
    pub size: u64,
}

pub fn model_list() -> Vec<ModelInfo> {
    ASSETS
        .iter()
        .map(|asset| ModelInfo { name: asset.name, url: asset.url, size: asset.size })
        .collect()
}

/// Bytes to download in all, for the download's progress.
const DOWNLOAD_SIZE: u64 = SEGMENTATION.size + EMBEDDING.size;

/// Diarizations running now, of meetings and of files alike: the models are not deleted
/// meanwhile.
static IN_USE: AtomicUsize = AtomicUsize::new(0);

/// Counts one diarization while it lives.
struct InUse;

impl InUse {
    fn new() -> Self {
        IN_USE.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for InUse {
    fn drop(&mut self) {
        IN_USE.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub struct SpeakerModels {
    dir: PathBuf,
    /// Where the worker that tells voices apart writes its errors; without one they go where
    /// the app's own output goes.
    log: Option<PathBuf>,
}

impl SpeakerModels {
    /// `dir`: `<models>/speakers`.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, log: None }
    }

    /// The worker's errors go to `log` (its own: a diarization runs beside the dictation engine,
    /// whose log starts over at each of its starts).
    pub fn with_log(self, log: PathBuf) -> Self {
        Self { log: Some(log), ..self }
    }

    /// Not ready, but a stopped download left files in the folder.
    pub fn partial(&self) -> bool {
        !self.ready() && fs::read_dir(&self.dir).is_ok_and(|mut entries| entries.next().is_some())
    }

    /// Whether a meeting or a file is being told apart by voice now.
    pub fn in_use(&self) -> bool {
        IN_USE.load(Ordering::Relaxed) > 0
    }

    pub fn ready(&self) -> bool {
        self.dir.join(READY).is_file()
            && ASSETS.iter().all(|asset| self.dir.join(asset.file).is_file())
    }

    /// Downloads and checks both models. `progress` gets (bytes so far, total) over both.
    pub fn download(
        &self,
        agent: &Agent,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<(), DownloadError> {
        fs::create_dir_all(&self.dir)?;
        let _ = fs::remove_file(self.dir.join(READY));
        for old in OLD_FILES {
            let _ = fs::remove_file(self.dir.join(old));
        }
        let mut before = 0;
        for asset in ASSETS {
            let target = self.dir.join(asset.file);
            if !target.is_file() {
                let expected = Expected { size: asset.size, sha256: asset.sha256 };
                let mut step = |done: u64, _total: u64| progress(before + done, DOWNLOAD_SIZE);
                match asset.inner {
                    None => {
                        download::download(agent, asset.url, &target, expected, cancel, &mut step)?
                    }
                    Some(inner) => {
                        let archive = self.dir.join(format!("{}.tar.bz2", asset.file));
                        download::download(
                            agent, asset.url, &archive, expected, cancel, &mut step,
                        )?;
                        let unpacked = take_out(&archive, inner, &target);
                        let _ = fs::remove_file(&archive);
                        unpacked?;
                    }
                }
            }
            before += asset.size;
            progress(before, DOWNLOAD_SIZE);
        }
        fs::write(self.dir.join(READY), b"")?;
        Ok(())
    }

    pub fn delete(&self) -> io::Result<()> {
        match fs::remove_dir_all(&self.dir) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// The voices of the recording `audio`, in order of start. Given the `words` of its transcript,
    /// one segment per sentence instead, in the order of the words, each with the voice it
    /// sounds most like: the segmentation alone misses quick turns, such as an interviewer's
    /// short questions inside the guest's long answers. Runs in a worker process, so
    /// the models' memory goes back to the system when it ends. `progress` gets the share done,
    /// from 0 to 1; setting `cancel` kills the worker.
    pub fn diarize(
        &self,
        audio: &Path,
        words: &[Word],
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(f64),
    ) -> Result<Vec<SpeakerSegment>, String> {
        if !self.ready() {
            return Err("the speaker models are not downloaded".into());
        }
        let _in_use = InUse::new();
        let log = self.log.as_deref();
        crate::engines::sherpa_process::diarize(&self.dir, audio, words, log, cancel, progress)
    }

    /// [`Self::diarize`] in this process: the worker's part, and tests.
    pub(crate) fn diarize_here(
        &self,
        samples: &[f32],
        words: &[Word],
        progress: &mut dyn FnMut(f64),
    ) -> Result<Vec<SpeakerSegment>, String> {
        if !self.ready() {
            return Err("the speaker models are not downloaded".into());
        }
        let path = |asset: &Asset| self.dir.join(asset.file).to_string_lossy().into_owned();
        let threads = threads();
        // In a block: the diarization's models are let go before `by_sentence` loads its own.
        let found = {
            let diarization =
                Diarization::create(&path(&SEGMENTATION), &path(&EMBEDDING), threads, WINDOW_SHIFT)
                    .ok_or_else(|| String::from("cannot load the speaker models"))?;
            if diarization.sample_rate() != TARGET_RATE as i32 {
                return Err(format!("the speaker models expect {} Hz", diarization.sample_rate()));
            }
            diarization
                .process(samples, &mut |done, total| {
                    progress(FIRST_PASS * f64::from(done) / f64::from(total.max(1)));
                })
                .ok_or_else(|| String::from("telling the speakers apart failed"))?
        };
        let segments = merge_short_voices(found);
        let embedding = SpeakerEmbeddingExtractorConfig {
            model: Some(path(&EMBEDDING)),
            num_threads: threads,
            ..Default::default()
        };
        let mut rest = |share: f64| progress(FIRST_PASS + (1.0 - FIRST_PASS) * share);
        Ok(by_sentence(&embedding, samples, words, segments, &mut rest))
    }
}

/// sherpa-onnx's offline speaker diarization through its C API: the `sherpa-onnx` crate does
/// not pass on the progress.
struct Diarization(*const sys::OfflineSpeakerDiarization);

unsafe extern "C" {
    /// In the C API of the linked library but not in `sherpa-onnx-sys`. The callback gets the
    /// voice samples done and their total, on the calling thread; its answer is ignored.
    fn SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
        sd: *const sys::OfflineSpeakerDiarization,
        samples: *const f32,
        n: i32,
        callback: extern "C" fn(i32, i32, *mut c_void) -> i32,
        arg: *mut c_void,
    ) -> *const sys::OfflineSpeakerDiarizationResult;
}

impl Diarization {
    /// The `sherpa-onnx` crate's defaults (voices of at least 0.3 s, pauses of 0.5 s) with
    /// [`CLUSTER_THRESHOLD`]; `window_shift`: how far the segmentation window moves, as a share
    /// of its length.
    fn create(
        segmentation: &str,
        embedding: &str,
        threads: i32,
        window_shift: f32,
    ) -> Option<Self> {
        let segmentation = CString::new(segmentation).ok()?;
        let embedding = CString::new(embedding).ok()?;
        let cpu = c"cpu";
        let config = sys::OfflineSpeakerDiarizationConfig {
            segmentation: sys::OfflineSpeakerSegmentationModelConfig {
                pyannote: sys::OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: segmentation.as_ptr(),
                    window_shift_ratio: window_shift,
                },
                num_threads: threads,
                debug: 0,
                provider: cpu.as_ptr(),
            },
            embedding: sys::SpeakerEmbeddingExtractorConfig {
                model: embedding.as_ptr(),
                num_threads: threads,
                debug: 0,
                provider: cpu.as_ptr(),
            },
            clustering: sys::FastClusteringConfig {
                num_clusters: -1,
                threshold: CLUSTER_THRESHOLD,
                compute_confidence: 0,
            },
            min_duration_on: 0.3,
            min_duration_off: 0.5,
        };
        // SAFETY: the strings outlive the call, and sherpa-onnx copies the configuration.
        let raw = unsafe { sys::SherpaOnnxCreateOfflineSpeakerDiarization(&config) };
        (!raw.is_null()).then_some(Self(raw))
    }

    fn sample_rate(&self) -> i32 {
        // SAFETY: a diarization from `create`.
        unsafe { sys::SherpaOnnxOfflineSpeakerDiarizationGetSampleRate(self.0) }
    }

    /// The voices in order of start; `progress` gets (done, total) as the work goes.
    fn process(
        &self,
        samples: &[f32],
        progress: &mut dyn FnMut(i32, i32),
    ) -> Option<Vec<SpeakerSegment>> {
        extern "C" fn report(done: i32, total: i32, arg: *mut c_void) -> i32 {
            // SAFETY: `arg` is the `progress` below, alive for the whole call that calls this.
            let progress = unsafe { &mut *arg.cast::<&mut dyn FnMut(i32, i32)>() };
            progress(done, total);
            0
        }
        let n = i32::try_from(samples.len()).ok()?;
        let mut progress = progress;
        let arg = (&raw mut progress).cast::<c_void>();
        // SAFETY: a diarization from `create`; `samples` holds `n` values; the callback runs on
        // this thread during the call only.
        let result = unsafe {
            SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
                self.0,
                samples.as_ptr(),
                n,
                report,
                arg,
            )
        };
        if result.is_null() {
            return None;
        }
        // SAFETY: a result of the call above, destroyed once here; the sorted segments are
        // `count` long and freed after they are copied.
        unsafe {
            let count = sys::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments(result);
            let sorted = if count > 0 {
                sys::SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime(result)
            } else {
                std::ptr::null()
            };
            let segments = if sorted.is_null() {
                Vec::new()
            } else {
                let copied = std::slice::from_raw_parts(sorted, count as usize)
                    .iter()
                    .map(|segment| SpeakerSegment {
                        start: f64::from(segment.start),
                        end: f64::from(segment.end),
                        speaker: segment.speaker,
                    })
                    .collect();
                sys::SherpaOnnxOfflineSpeakerDiarizationDestroySegment(sorted);
                copied
            };
            sys::SherpaOnnxOfflineSpeakerDiarizationDestroyResult(result);
            Some(segments)
        }
    }
}

impl Drop for Diarization {
    fn drop(&mut self) {
        // SAFETY: a diarization from `create`, destroyed only here.
        unsafe { sys::SherpaOnnxDestroyOfflineSpeakerDiarization(self.0) }
    }
}

/// The voices that sound alike joined ([`join_alike_voices`]); then, given `words`, the
/// segments as sentences of `words`, each with the voice whose sound it is closest to. With
/// fewer than two voices to compare, or when the model fails, the segments stay as they are.
/// `progress` gets the share done, from 0 to 1.
fn by_sentence(
    config: &SpeakerEmbeddingExtractorConfig,
    samples: &[f32],
    words: &[Word],
    mut segments: Vec<SpeakerSegment>,
    progress: &mut dyn FnMut(f64),
) -> Vec<SpeakerSegment> {
    let Some(extractor) = SpeakerEmbeddingExtractor::create(config) else { return segments };
    let embed = |start: f64, end: f64| embedding(&extractor, samples, start, end);
    let parts = voice_parts(&segments);
    // The sentences are counted again once the voices are joined: there are fewer then.
    let (mut done, mut steps) =
        (0, parts.values().map(Vec::len).sum::<usize>() + sentences(words, &segments).len());
    let mut report = |done: usize, steps: usize| progress(done as f64 / steps.max(1) as f64);
    let mut heard: Vec<(i32, Vec<f32>, f64)> = Vec::new();
    for (voice, stretches) in parts {
        let mut weighted = Vec::new();
        for (a, b) in stretches {
            if let Some(sound) = embed(a, b) {
                weighted.push((sound, b - a));
            }
            done += 1;
            report(done, steps);
        }
        let length = weighted.iter().map(|(_, length)| length).sum();
        if let Some(sound) = average(weighted.into_iter()) {
            heard.push((voice, sound, length));
        }
    }
    if heard.len() < 2 {
        return segments;
    }
    let (voices, renamed) = join_alike_voices(heard);
    for segment in &mut segments {
        if let Some(&voice) = renamed.get(&segment.speaker) {
            segment.speaker = voice;
        } else if !voices.iter().any(|(voice, _)| *voice == segment.speaker) {
            // Too little of this voice alone to know its sound: the voice its audio sounds like.
            if let Some(voice) =
                embed(segment.start, segment.end).and_then(|sound| closest(&voices, &sound))
            {
                segment.speaker = voice;
            }
        }
    }
    if words.is_empty() || voices.len() < 2 {
        return segments;
    }
    let said = sentences(words, &segments);
    steps = done + said.len();
    let mut found = Vec::with_capacity(said.len());
    for sentence in said {
        let (start, end) = sentence.audio;
        let segments_say = voice_at(&segments, (start + end) / 2.0);
        let speaker = if end - start >= SHORTEST_SENTENCE {
            embed(start - SENTENCE_MARGIN, end + SENTENCE_MARGIN)
                .and_then(|sound| closest(&voices, &sound))
                .unwrap_or(segments_say)
        } else {
            segments_say
        };
        let (start, end) = sentence.covers;
        found.push(SpeakerSegment { start, end, speaker });
        done += 1;
        report(done, steps);
    }
    found
}

/// The voices left (number and sound) and, for each number that went to another, the number it
/// went to.
type Joined = (Vec<(i32, Vec<f32>)>, HashMap<i32, i32>);

/// Joins voices that sound at least [`SAME_VOICE`] alike, the most alike pair first: the
/// segmentation splits a speaker whose sound changes over a long recording. The joined voice
/// keeps the number of the one heard longer and sounds like the two, weighted by how long each
/// is heard. `voices`: number, sound and seconds heard.
fn join_alike_voices(mut voices: Vec<(i32, Vec<f32>, f64)>) -> Joined {
    let mut renamed: HashMap<i32, i32> = HashMap::new();
    loop {
        let mut most: Option<(usize, usize, f32)> = None;
        for i in 0..voices.len() {
            for j in i + 1..voices.len() {
                let sounds = alike(&voices[i].1, &voices[j].1);
                if sounds >= SAME_VOICE && most.is_none_or(|(.., best)| sounds > best) {
                    most = Some((i, j, sounds));
                }
            }
        }
        let Some((i, j, _)) = most else { break };
        let (keep, gone) =
            if voices[i].2 >= voices[j].2 { (voices[i].0, j) } else { (voices[j].0, i) };
        let (gone, gone_sound, gone_length) = voices.remove(gone);
        let Some(kept) = voices.iter_mut().find(|voice| voice.0 == keep) else { break };
        if let Some(sound) =
            average([(kept.1.clone(), kept.2), (gone_sound, gone_length)].into_iter())
        {
            kept.1 = sound;
        }
        kept.2 += gone_length;
        for to in renamed.values_mut().filter(|to| **to == gone) {
            *to = keep;
        }
        renamed.insert(gone, keep);
    }
    (voices.into_iter().map(|(voice, sound, _)| (voice, sound)).collect(), renamed)
}

/// For each voice, the stretches where it alone speaks, at least [`SHORTEST_PART`] long,
/// longest first and at most [`VOICE_SAMPLE`] seconds in all. Overlapping segments of one voice
/// (a scrap given to it by [`merge_short_voices`]) count once.
fn voice_parts(segments: &[SpeakerSegment]) -> BTreeMap<i32, Vec<(f64, f64)>> {
    let mut alone: BTreeMap<i32, Vec<(f64, f64)>> = BTreeMap::new();
    for segment in segments {
        let mut pieces = vec![(segment.start, segment.end)];
        for other in segments.iter().filter(|other| other.speaker != segment.speaker) {
            pieces = pieces
                .into_iter()
                .flat_map(|(a, b)| {
                    if other.end <= a || other.start >= b {
                        vec![(a, b)]
                    } else {
                        vec![(a, other.start), (other.end, b)]
                    }
                })
                .filter(|(a, b)| b > a)
                .collect();
        }
        alone.entry(segment.speaker).or_default().extend(pieces);
    }
    let mut parts: BTreeMap<i32, Vec<(f64, f64)>> = BTreeMap::new();
    for (voice, mut pieces) in alone {
        pieces.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut joined: Vec<(f64, f64)> = Vec::new();
        for (start, end) in pieces {
            match joined.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => joined.push((start, end)),
            }
        }
        joined.retain(|(a, b)| b - a >= SHORTEST_PART);
        parts.insert(voice, joined);
    }
    for list in parts.values_mut() {
        list.sort_by(|x, y| (y.1 - y.0).total_cmp(&(x.1 - x.0)));
        // The last stretch taken is cut to what is left, so one long monologue is not taken whole.
        let mut left = VOICE_SAMPLE;
        let mut taken = Vec::new();
        for (a, b) in list.drain(..) {
            if left < SHORTEST_PART {
                break;
            }
            let end = b.min(a + left);
            left -= end - a;
            taken.push((a, end));
        }
        *list = taken;
    }
    parts.retain(|_, list| !list.is_empty());
    parts
}

/// How `samples[start..end]` (seconds) sounds, as a unit vector; `None` for too little audio.
fn embedding(
    extractor: &SpeakerEmbeddingExtractor,
    samples: &[f32],
    start: f64,
    end: f64,
) -> Option<Vec<f32>> {
    let rate = f64::from(TARGET_RATE);
    let from = ((start.max(0.0) * rate) as usize).min(samples.len());
    let to = ((end.max(0.0) * rate) as usize).min(samples.len());
    if to <= from {
        return None;
    }
    let stream = extractor.create_stream()?;
    stream.accept_waveform(TARGET_RATE as i32, &samples[from..to]);
    stream.input_finished();
    if !extractor.is_ready(&stream) {
        return None;
    }
    unit(extractor.compute(&stream)?)
}

/// The weighted average of unit vectors, as a unit vector.
fn average(weighted: impl Iterator<Item = (Vec<f32>, f64)>) -> Option<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for (vector, weight) in weighted {
        let sum = sum.get_or_insert_with(|| vec![0.0; vector.len()]);
        for (total, value) in sum.iter_mut().zip(&vector) {
            *total += value * weight as f32;
        }
    }
    unit(sum?)
}

fn unit(mut vector: Vec<f32>) -> Option<Vec<f32>> {
    let length = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if !length.is_normal() {
        return None;
    }
    vector.iter_mut().for_each(|value| *value /= length);
    Some(vector)
}

/// How alike two sounds are: the cosine of unit vectors.
fn alike(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The voice whose sound is closest to `sound`.
fn closest(voices: &[(i32, Vec<f32>)], sound: &[f32]) -> Option<i32> {
    voices
        .iter()
        .max_by(|a, b| alike(&a.1, sound).total_cmp(&alike(&b.1, sound)))
        .map(|(voice, _)| *voice)
}

/// Gives the segments of voices heard for less than [`SHORTEST_VOICE`] in total to the voice of
/// the nearest segment of a longer one. When every voice is that short, nothing changes.
fn merge_short_voices(segments: Vec<SpeakerSegment>) -> Vec<SpeakerSegment> {
    let mut totals: HashMap<i32, f64> = HashMap::new();
    for segment in &segments {
        *totals.entry(segment.speaker).or_default() += segment.end - segment.start;
    }
    let long = |speaker: i32| totals.get(&speaker).is_some_and(|total| *total >= SHORTEST_VOICE);
    let kept: Vec<SpeakerSegment> =
        segments.iter().copied().filter(|segment| long(segment.speaker)).collect();
    if kept.is_empty() {
        return segments;
    }
    segments
        .into_iter()
        .map(|mut segment| {
            if !long(segment.speaker) {
                let gap = |other: &SpeakerSegment| {
                    (other.start - segment.end).max(segment.start - other.end).max(0.0)
                };
                if let Some(nearest) = kept.iter().min_by(|a, b| gap(a).total_cmp(&gap(b))) {
                    segment.speaker = nearest.speaker;
                }
            }
            segment
        })
        .collect()
}

/// Takes `inner` out of the archive's top folder (or top level) into `target`.
fn take_out(archive: &Path, inner: &str, target: &Path) -> Result<(), DownloadError> {
    download::unpack_named(archive, &[inner], |_| target.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_come_only_from_the_official_releases() {
        for asset in ASSETS {
            assert!(
                asset.url.starts_with(
                    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-"
                ),
                "{}",
                asset.url
            );
            assert_eq!(asset.sha256.len(), 64);
            assert!(asset.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            assert!(asset.size > 1_000_000);
        }
    }

    #[test]
    fn the_interface_lists_both_models() {
        let list = model_list();
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().map(|m| m.size).sum::<u64>(), DOWNLOAD_SIZE);
        assert_eq!(list[0].name, SEGMENTATION.name);
        assert_eq!(list[1].name, EMBEDDING.name);
        assert_eq!(list[0].url, SEGMENTATION.url);
        assert_eq!(list[1].url, EMBEDDING.url);
    }

    #[test]
    fn models_telling_voices_apart_are_in_use() {
        let models = SpeakerModels::new(PathBuf::from("speakers"));
        let running = InUse::new();
        assert!(models.in_use());
        drop(running);
        assert!(!models.in_use());
    }

    #[test]
    fn nothing_is_ready_before_the_download() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("speakers-{}", std::process::id()));
        let models = SpeakerModels::new(dir.clone());
        assert!(!models.ready());
        let nowhere = Path::new("no-such-recording.opus");
        assert!(models.diarize(nowhere, &[], &AtomicBool::new(false), &mut |_| {}).is_err());
        assert!(models.diarize_here(&[0.0; 16_000], &[], &mut |_| {}).is_err());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(READY), b"").unwrap();
        assert!(!models.ready(), "the marker alone is not enough");
        models.delete().unwrap();
        models.delete().unwrap();
    }

    #[test]
    fn a_stopped_download_is_partial() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("speakers-partial-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let models = SpeakerModels::new(dir.clone());
        assert!(!models.partial(), "no folder");
        fs::create_dir_all(&dir).unwrap();
        assert!(!models.partial(), "an empty folder");
        fs::write(dir.join("embedding-eres2netv2.onnx.part"), b"half").unwrap();
        assert!(models.partial());
        models.delete().unwrap();
        assert!(!dir.exists() && !models.partial());
    }

    #[test]
    fn a_folder_with_the_old_model_is_not_ready() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("speakers-old-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for name in [READY, SEGMENTATION.file, OLD_FILES[0]] {
            fs::write(dir.join(name), b"").unwrap();
        }
        let models = SpeakerModels::new(dir);
        assert!(!models.ready());
        models.delete().unwrap();
    }

    fn segment(start: f64, end: f64, speaker: i32) -> SpeakerSegment {
        SpeakerSegment { start, end, speaker }
    }

    #[test]
    fn a_scrap_of_a_voice_joins_its_neighbour() {
        // As in a Russian news recording: a word at the start came out as its own voice.
        let found = merge_short_voices(vec![
            segment(0.0, 0.7, 0),
            segment(0.5, 1.4, 1),
            segment(1.6, 10.4, 0),
            segment(33.1, 43.5, 3),
        ]);
        let speakers: Vec<i32> = found.iter().map(|s| s.speaker).collect();
        assert_eq!(speakers, [0, 0, 0, 3]);
        assert_eq!(found[1].start, 0.5);
    }

    #[test]
    fn a_scrap_takes_the_nearest_long_voice() {
        let found = merge_short_voices(vec![
            segment(0.0, 5.0, 0),
            segment(9.0, 9.6, 2),
            segment(10.0, 15.0, 1),
        ]);
        assert_eq!(found[1].speaker, 1);
        // Two scraps alone stay as they are.
        let alone = vec![segment(0.0, 0.5, 0), segment(1.0, 1.4, 1)];
        assert_eq!(merge_short_voices(alone.clone()), alone);
    }

    #[test]
    fn a_voice_sounds_like_where_it_speaks_alone() {
        let parts = voice_parts(&[
            segment(0.0, 10.0, 0),
            // A question inside the answer, and one at the end over a short scrap.
            segment(5.0, 7.0, 1),
            segment(12.0, 14.0, 1),
            segment(13.0, 13.5, 0),
        ]);
        assert_eq!(parts[&0], [(0.0, 5.0), (7.0, 10.0)]);
        // 13.5–14 is shorter than SHORTEST_PART.
        assert_eq!(parts[&1], [(12.0, 13.0)]);
        // A scrap given to a voice overlaps its segment: counted once, and two short pieces
        // that touch make one long enough.
        let parts = voice_parts(&[
            segment(0.0, 0.7, 0),
            segment(0.5, 1.4, 0),
            segment(3.0, 3.6, 0),
            segment(3.6, 4.2, 0),
        ]);
        assert_eq!(parts[&0], [(0.0, 1.4), (3.0, 4.2)]);
        // A long voice gives about VOICE_SAMPLE seconds, longest stretches first.
        let long: Vec<SpeakerSegment> =
            (0..10).map(|i| segment(f64::from(i) * 50.0, f64::from(i) * 50.0 + 40.0, 2)).collect();
        assert_eq!(parts_total(&voice_parts(&long)[&2]), 120.0);
        // One long monologue, or stretches longer than what is left, are cut to VOICE_SAMPLE.
        assert_eq!(voice_parts(&[segment(0.0, 600.0, 3)])[&3], [(0.0, 120.0)]);
        let two = [segment(0.0, 100.0, 4), segment(200.0, 300.0, 4)];
        assert_eq!(parts_total(&voice_parts(&two)[&4]), 120.0);
    }

    #[test]
    fn a_speaker_split_by_the_clustering_is_joined() {
        // As in a call recording: two speakers, the host in two voices, the guest in three.
        let host = unit(vec![1.0, 0.1, 0.0]).unwrap();
        let guest = unit(vec![0.1, 1.0, 0.2]).unwrap();
        let (voices, renamed) = join_alike_voices(vec![
            (1, unit(vec![0.9, 0.3, 0.1]).unwrap(), 8.0),
            (3, guest.clone(), 144.0),
            (8, unit(vec![0.0, 0.9, 0.5]).unwrap(), 17.0),
            (9, unit(vec![0.2, 0.8, 0.3]).unwrap(), 28.0),
            (10, host.clone(), 345.0),
        ]);
        let mut left: Vec<i32> = voices.iter().map(|(voice, _)| *voice).collect();
        left.sort_unstable();
        assert_eq!(left, [3, 10], "each speaker keeps the number heard longest");
        assert_eq!(renamed, HashMap::from([(1, 10), (8, 3), (9, 3)]));
        // A voice joined first and then joined to a longer one: both go to the longer one.
        let (voices, renamed) = join_alike_voices(vec![
            (0, unit(vec![0.899, 0.438, 0.0]).unwrap(), 1.0),
            (1, unit(vec![0.896, 0.429, 0.08]).unwrap(), 2.0),
            (2, unit(vec![1.0, 0.0, 0.0]).unwrap(), 10.0),
        ]);
        assert_eq!(voices.len(), 1);
        assert_eq!(renamed, HashMap::from([(0, 2), (1, 2)]));
        // Voices that do not sound alike stay apart.
        let (voices, renamed) = join_alike_voices(vec![(0, host, 5.0), (1, guest, 5.0)]);
        assert_eq!(voices.len(), 2);
        assert!(renamed.is_empty());
        // Two men of an interview sound 0.70 alike, and still are two.
        let (a, b) = (vec![1.0, 0.0], unit(vec![0.70, 0.714]).unwrap());
        assert!((alike(&a, &b) - 0.70).abs() < 0.005);
        let (voices, renamed) = join_alike_voices(vec![(0, a, 20.0), (1, b, 40.0)]);
        assert_eq!(voices.len(), 2);
        assert!(renamed.is_empty());
    }

    fn parts_total(parts: &[(f64, f64)]) -> f64 {
        parts.iter().map(|(a, b)| b - a).sum()
    }

    #[test]
    fn a_sentence_goes_to_the_voice_it_sounds_like() {
        let host = unit(vec![1.0, 0.2, 0.0]).unwrap();
        let guest = unit(vec![0.1, 1.0, 0.3]).unwrap();
        let voices = [(4, host.clone()), (0, guest.clone())];
        assert_eq!(closest(&voices, &unit(vec![0.9, 0.3, 0.1]).unwrap()), Some(4));
        assert_eq!(closest(&voices, &unit(vec![0.0, 0.8, 0.5]).unwrap()), Some(0));
        // The average leans to the longer stretch.
        let mixed = average([(host.clone(), 3.0), (guest, 1.0)].into_iter()).unwrap();
        assert_eq!(closest(&voices, &mixed), Some(4));
        assert!(unit(vec![0.0, 0.0]).is_none());
        assert!(average(std::iter::empty()).is_none());
    }

    /// The turns of a real recording: AHEM_AUDIO=<the recording>, AHEM_WORDS=<a JSON file with
    /// its words, `[{"start": 0.0, "end": 0.4, "text": " Hi"}, …]`>, and the models in
    /// AHEM_SPEAKERS or else `runtimes/models/speakers` (see `voices_are_counted`).
    #[test]
    #[ignore = "needs a recording, its words and the models: cargo test -- --ignored sentences_get_their_voices --nocapture"]
    fn sentences_get_their_voices() {
        use super::super::transcript::{others_speakers, turns};
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtimes");
        let models = SpeakerModels::new(
            std::env::var("AHEM_SPEAKERS")
                .map(PathBuf::from)
                .unwrap_or_else(|_| root.join("models/speakers")),
        );
        let audio = crate::files::decode(Path::new(&std::env::var("AHEM_AUDIO").unwrap())).unwrap();
        let words: Vec<Word> = serde_json::from_str(
            &fs::read_to_string(std::env::var("AHEM_WORDS").unwrap()).unwrap(),
        )
        .unwrap();
        let started = std::time::Instant::now();
        let segments = models.diarize_here(&audio.samples, &words, &mut |_| {}).unwrap();
        println!(
            "{:.1} s for {:.1} s of audio",
            started.elapsed().as_secs_f64(),
            audio.samples.len() as f64 / f64::from(TARGET_RATE)
        );
        let turns = turns(Vec::new(), others_speakers(words, &segments));
        for turn in &turns {
            println!("[{:.1}] {:?}: {}", turn.start, turn.speaker, turn.text());
        }
        assert!(!turns.is_empty());
    }

    /// How long each model takes on a recording and which voices come out, for each setting in
    /// AHEM_RUNS (`<window shift>:<threads>,…`, by default the app's): AHEM_AUDIO=<the
    /// recording>, AHEM_SPEAKERS=<the speaker models folder>.
    #[test]
    #[ignore = "needs a recording and the models: cargo test -- --ignored voices_of_a_recording --nocapture"]
    fn voices_of_a_recording() {
        use std::time::Instant;
        let models = SpeakerModels::new(PathBuf::from(std::env::var("AHEM_SPEAKERS").unwrap()));
        let audio = crate::files::decode(Path::new(&std::env::var("AHEM_AUDIO").unwrap())).unwrap();
        println!("{:.1} s of audio", audio.samples.len() as f64 / f64::from(TARGET_RATE));
        let path = |asset: &Asset| models.dir.join(asset.file).to_string_lossy().into_owned();
        let runs = std::env::var("AHEM_RUNS").unwrap_or(format!("{WINDOW_SHIFT}:{}", threads()));
        let totals = |segments: &[SpeakerSegment]| {
            let mut totals: BTreeMap<i32, f64> = BTreeMap::new();
            for segment in segments {
                *totals.entry(segment.speaker).or_default() += segment.end - segment.start;
            }
            let list: Vec<String> =
                totals.iter().map(|(voice, total)| format!("{voice}: {total:.0} s")).collect();
            format!("{} voices ({})", totals.len(), list.join(", "))
        };
        for run in runs.split(',') {
            let (shift, threads) = run.split_once(':').unwrap();
            let (shift, threads): (f32, i32) = (shift.parse().unwrap(), threads.parse().unwrap());
            let started = Instant::now();
            let diarization =
                Diarization::create(&path(&SEGMENTATION), &path(&EMBEDDING), threads, shift)
                    .unwrap();
            let loaded = started.elapsed().as_secs_f64();
            let (mut first, mut samples) = (None, 0);
            let found = diarization
                .process(&audio.samples, &mut |_, total| {
                    first.get_or_insert(started.elapsed().as_secs_f64());
                    samples = total;
                })
                .unwrap();
            let whole = started.elapsed().as_secs_f64();
            drop(diarization);
            let segmented = first.unwrap_or(whole);
            let segments = merge_short_voices(found);
            let config = SpeakerEmbeddingExtractorConfig {
                model: Some(path(&EMBEDDING)),
                num_threads: threads,
                ..Default::default()
            };
            let joining = Instant::now();
            let joined = by_sentence(&config, &audio.samples, &[], segments.clone(), &mut |_| {});
            println!(
                "shift {shift}, {threads} threads: loading {loaded:.1} s, segmentation {:.1} s, \
                 voice samples {:.1} s ({samples}), joining {:.1} s",
                segmented - loaded,
                whole - segmented,
                joining.elapsed().as_secs_f64()
            );
            println!("  clustering: {}", totals(&segments));
            println!("  joined:     {}", totals(&joined));
            // How alike the voices of the clustering sound, and what sentences would cost.
            let extractor = SpeakerEmbeddingExtractor::create(&config).unwrap();
            let voices: Vec<(i32, Vec<f32>)> = voice_parts(&segments)
                .into_iter()
                .filter_map(|(voice, parts)| {
                    let weighted = parts.iter().filter_map(|&(a, b)| {
                        Some((embedding(&extractor, &audio.samples, a, b)?, b - a))
                    });
                    Some((voice, average(weighted)?))
                })
                .collect();
            for (voice, sound) in &voices {
                let row: Vec<String> =
                    voices.iter().map(|(_, other)| format!("{:.2}", alike(sound, other))).collect();
                println!("  voice {voice} alike: {}", row.join(" "));
            }
            let started = Instant::now();
            let mut pieces = 0;
            for segment in &joined {
                let mut at = segment.start;
                while at < segment.end {
                    embedding(&extractor, &audio.samples, at, (at + 5.0).min(segment.end));
                    at += 5.0;
                    pieces += 1;
                }
            }
            println!("  {pieces} sentence-sized pieces: {:.1} s", started.elapsed().as_secs_f64());
        }
    }

    /// Downloads the models (78 MB) and counts the voices in the test recordings of the
    /// sherpa-onnx `speaker-segmentation-models` release that are in `runtimes/test-audio`
    /// (`0-four-speakers-zh.wav`, `1-` to `3-two-speakers-en.wav`).
    #[test]
    #[ignore = "downloads the models: cargo test -- --ignored voices_are_counted --nocapture"]
    fn voices_are_counted() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtimes");
        let models = SpeakerModels::new(root.join("models/speakers"));
        let cancel = AtomicBool::new(false);
        models.download(&crate::runtimes::http::agent(), &cancel, &mut |_, _| {}).unwrap();
        let expected = [
            ("0-four-speakers-zh.wav", 4),
            ("1-two-speakers-en.wav", 2),
            ("2-two-speakers-en.wav", 2),
            ("3-two-speakers-en.wav", 2),
        ];
        let mut checked = 0;
        for (name, count) in expected {
            let Ok(decoded) = crate::files::decode(&root.join("test-audio").join(name)) else {
                continue;
            };
            let segments = models.diarize_here(&decoded.samples, &[], &mut |_| {}).unwrap();
            let mut voices: Vec<i32> = segments.iter().map(|s| s.speaker).collect();
            voices.sort_unstable();
            voices.dedup();
            println!("{name}: {} voices in {} segments", voices.len(), segments.len());
            assert_eq!(voices.len(), count, "{name}");
            checked += 1;
        }
        assert!(checked > 0, "no test recordings found");
    }
}
