//! Splitting a file's 16 kHz audio into engine requests.
//!
//! A request carries at most 32 s. Its own part ends at the quietest 200 ms frame between 25 s
//! and 30 s from the request start (at the frame centre), and 2 s of audio on each side of the
//! own part go along as context, so words cut by the boundary are heard whole. A window owns
//! the words whose midpoint falls in its own part; the last window also owns words that end
//! past the audio. The rest of the file (under 32 s) is the last window. At a seam the two
//! transcripts are first matched by text: [`super::words::merge`].

use std::borrow::Cow;
use std::ops::Range;

use crate::audio::resample::TARGET_RATE;

const RATE: usize = TARGET_RATE as usize;
/// Longest request; a shorter remainder becomes the last window.
const WINDOW: usize = 32 * RATE;
/// Audio a window hears past its own part on each side, in seconds.
pub const CONTEXT_SECONDS: usize = 2;
const CONTEXT: usize = CONTEXT_SECONDS * RATE;
/// Where the boundary is searched, from the request start.
const SEARCH_FROM: usize = 25 * RATE;
const SEARCH_TO: usize = 30 * RATE;
const FRAME: usize = RATE / 5;
/// whisper.cpp returns nothing for input under one second.
const MIN_REQUEST: usize = RATE;

/// One engine request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    /// Position in the file, from 0; the scheduler's chunk number.
    pub index: u32,
    /// First sample sent to the engine (context included), in 16 kHz file samples.
    pub start: usize,
    /// End of the samples sent, exclusive.
    pub end: usize,
    /// Start of the own part, seconds from the file start.
    pub keep_from: f64,
    /// End of the own part, seconds; the last window ends at the end of the audio.
    pub keep_to: f64,
    pub last: bool,
}

impl Window {
    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }

    /// File time of the request's first sample, in seconds: engine times are relative to it.
    pub fn offset(&self) -> f64 {
        self.start as f64 / f64::from(TARGET_RATE)
    }

    /// Whether a word with this midpoint (file time, seconds) belongs to this window.
    pub fn owns(&self, midpoint: f64) -> bool {
        midpoint >= self.keep_from && (self.last || midpoint < self.keep_to)
    }

    /// Audio to send, from the whole file's samples; padded with silence to one second.
    pub fn audio<'a>(&self, file: &'a [f32]) -> Cow<'a, [f32]> {
        let audio = &file[self.range()];
        if audio.len() >= MIN_REQUEST {
            return Cow::Borrowed(audio);
        }
        let mut padded = audio.to_vec();
        padded.resize(MIN_REQUEST, 0.0);
        Cow::Owned(padded)
    }
}

/// Windows for a whole file in memory. Deterministic: the same audio gives the same windows.
pub fn plan(samples: &[f32]) -> Vec<Window> {
    let mut planner = Planner::new();
    let mut windows = Vec::new();
    while let Some(window) = planner.next(&samples[planner.start()..], true) {
        windows.push(window);
    }
    windows
}

/// Window planning over a stream, so a long file need not be in memory at once: feed it the
/// audio from [`Planner::start`] on. Gives the same windows as [`plan`].
#[derive(Debug, Clone, Default)]
pub struct Planner {
    start: usize,
    keep_from: usize,
    index: u32,
    done: bool,
}

impl Planner {
    pub fn new() -> Self {
        Self::default()
    }

    /// First file sample the next window uses; the caller may drop audio before it, but only
    /// after taking the previous window's audio.
    pub fn start(&self) -> usize {
        self.start
    }

    /// The next window. `tail` is the audio from [`Planner::start`] on; `complete` says the file
    /// ends with it. `None` means more audio is needed, or (once `complete`) that the plan is done.
    pub fn next(&mut self, tail: &[f32], complete: bool) -> Option<Window> {
        if self.done {
            return None;
        }
        if tail.len() >= WINDOW {
            let boundary = self.start + quietest_point(&tail[SEARCH_FROM..SEARCH_TO]) + SEARCH_FROM;
            let window = self.window(boundary + CONTEXT, boundary, false);
            self.start = boundary - CONTEXT;
            self.keep_from = boundary;
            self.index += 1;
            return Some(window);
        }
        if !complete {
            return None;
        }
        self.done = true;
        let end = self.start + tail.len();
        (end > self.keep_from).then(|| self.window(end, end, true))
    }

    fn window(&self, end: usize, keep_to: usize, last: bool) -> Window {
        let seconds = |sample: usize| sample as f64 / f64::from(TARGET_RATE);
        Window {
            index: self.index,
            start: self.start,
            end,
            keep_from: seconds(self.keep_from),
            keep_to: seconds(keep_to),
            last,
        }
    }
}

/// Centre of the 200 ms frame with the least energy; the first one on a tie.
fn quietest_point(search: &[f32]) -> usize {
    let energy =
        |frame: &[f32; FRAME]| frame.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>();
    let quietest = search
        .as_chunks::<FRAME>()
        .0
        .iter()
        .map(energy)
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(0, |(index, _)| index);
    quietest * FRAME + FRAME / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(value: f64) -> usize {
        (value * RATE as f64).round() as usize
    }

    /// Deterministic loud noise, like continuous speech.
    fn noise(length: usize) -> Vec<f32> {
        let mut state: u32 = 12_345;
        (0..length)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((state >> 8) as f32 / (1 << 23) as f32 - 1.0) * 0.3
            })
            .collect()
    }

    fn silence(audio: &mut [f32], from: f64, to: f64) {
        audio[seconds(from)..seconds(to)].fill(0.0);
    }

    #[test]
    fn empty_audio_has_no_windows() {
        assert!(plan(&[]).is_empty());
    }

    #[test]
    fn short_file_is_one_window() {
        let audio = noise(seconds(10.0));
        assert_eq!(
            plan(&audio),
            vec![Window {
                index: 0,
                start: 0,
                end: audio.len(),
                keep_from: 0.0,
                keep_to: 10.0,
                last: true
            }]
        );
        assert_eq!(plan(&noise(WINDOW - 1)).len(), 1);
        assert_eq!(plan(&noise(WINDOW)).len(), 2);
    }

    #[test]
    fn long_file_windows_cover_it_with_context() {
        let audio = noise(seconds(200.0));
        let windows = plan(&audio);
        assert!(windows.len() >= 7, "{}", windows.len());
        assert_eq!(windows[0].start, 0);
        assert_eq!(windows[0].keep_from, 0.0);
        for (i, pair) in windows.windows(2).enumerate() {
            let (a, b) = (&pair[0], &pair[1]);
            assert_eq!(a.index as usize, i);
            assert!(!a.last);
            assert!(a.end - a.start <= WINDOW, "window {i} too long");
            let boundary = seconds(a.keep_to);
            assert_eq!(a.end, boundary + CONTEXT, "right context");
            assert_eq!(b.start, boundary - CONTEXT, "left context");
            assert_eq!(b.keep_from, a.keep_to, "own parts touch");
            let from_start = boundary - a.start;
            assert!((SEARCH_FROM..SEARCH_TO).contains(&from_start), "{from_start}");
        }
        let last = windows.last().unwrap();
        assert!(last.last);
        assert_eq!(last.end, audio.len());
        assert_eq!(last.keep_to, 200.0);
        assert!(last.end - last.start < WINDOW);
    }

    #[test]
    fn boundary_is_the_quietest_frame() {
        let mut audio = noise(seconds(70.0));
        // Frame 10 of the first search range: 25.0 s + 10 × 0.2 s.
        silence(&mut audio, 27.0, 27.2);
        let windows = plan(&audio);
        assert_eq!(windows[0].keep_to, 27.1);
        assert_eq!(windows[0].end, seconds(29.1));
        // The second request starts at 25.1 s; its search range is 50.1–55.1 s.
        silence(&mut audio, 52.3, 52.5);
        let windows = plan(&audio);
        assert_eq!(windows[1].start, seconds(25.1));
        assert_eq!(windows[1].keep_from, 27.1);
        assert_eq!(windows[1].keep_to, 52.4);
    }

    #[test]
    fn silence_everywhere_takes_the_first_frame() {
        let windows = plan(&vec![0.0; seconds(40.0)]);
        assert_eq!(windows[0].keep_to, 25.1);
        assert_eq!(windows.len(), 2);
    }

    #[test]
    fn planning_is_deterministic() {
        let audio = noise(seconds(95.0));
        assert_eq!(plan(&audio), plan(&audio));
    }

    #[test]
    fn streaming_gives_the_same_windows() {
        let mut audio = noise(seconds(130.0));
        silence(&mut audio, 26.4, 26.6);
        let expected = plan(&audio);

        let mut planner = Planner::new();
        let mut buffer: Vec<f32> = Vec::new();
        let mut buffer_start = 0;
        let mut windows = Vec::new();
        let mut fed = 0;
        let mut step = 7_919;
        loop {
            let next = (fed + step).min(audio.len());
            buffer.extend_from_slice(&audio[fed..next]);
            fed = next;
            step = step * 3 % 50_021 + 1_000;
            let complete = fed == audio.len();
            while let Some(window) =
                planner.next(&buffer[planner.start() - buffer_start..], complete)
            {
                let sent = &buffer[window.start - buffer_start..window.end - buffer_start];
                assert_eq!(sent, &audio[window.range()]);
                windows.push(window);
                buffer.drain(..planner.start() - buffer_start);
                buffer_start = planner.start();
            }
            if complete {
                break;
            }
            assert!(buffer.len() < WINDOW, "the buffer stays under one request");
        }
        assert_eq!(windows, expected);
    }

    #[test]
    fn ownership_uses_the_own_part() {
        let windows = plan(&noise(seconds(60.0)));
        let (first, second, last) = (&windows[0], &windows[1], windows.last().unwrap());
        assert!(first.owns(0.0));
        assert!(!first.owns(first.keep_to));
        assert!(second.owns(first.keep_to));
        assert!(!second.owns(first.keep_to - 0.01));
        assert!(!second.owns(second.keep_to));
        assert!(last.owns(1_000.0), "the last window keeps words past the end");
        assert!(!last.owns(last.keep_from - 0.01));
    }

    #[test]
    fn short_requests_are_padded_to_a_second() {
        let audio = noise(seconds(0.5));
        let window = plan(&audio)[0];
        let sent = window.audio(&audio);
        assert_eq!(sent.len(), RATE);
        assert_eq!(&sent[..audio.len()], &audio[..]);
        assert!(sent[audio.len()..].iter().all(|s| *s == 0.0));

        let long = noise(seconds(3.0));
        assert!(matches!(plan(&long)[0].audio(&long), Cow::Borrowed(_)));
        assert_eq!(plan(&long)[0].offset(), 0.0);
    }
}
