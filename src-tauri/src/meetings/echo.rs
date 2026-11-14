//! A meeting recorded from speakers: the microphone also hears the others.
//!
//! The computer's sound is exactly what the speakers played, so it tells what the microphone
//! may have heard. After the recording:
//!
//! 1. [`find_echo`] compares the loudness of both tracks in 10 ms frames and finds the delay
//!    from the speakers to the microphone and how loud the echo is. Without a clear match (the
//!    user wore headphones) nothing else happens: the microphone track is used as recorded.
//! 2. [`echo_mask`] marks the microphone's frames that are no louder than the echo expected
//!    there; [`gate`] silences them before the track is transcribed. Frames clearly louder than
//!    the echo are the user speaking and stay.
//! 3. [`drop_echo_words`] removes from the user's words the ones the others said at the same
//!    moment, for echo left where both spoke at once.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::files::{FileDecoder, Word};

/// 10 ms at 16 kHz.
pub const FRAME: usize = 160;
const RATE: f64 = 16_000.0;
/// Delays searched, in frames: the loopback may run a little behind the microphone, and
/// Bluetooth speakers play up to a few hundred milliseconds late.
const MIN_LAG: i64 = -30;
const MAX_LAG: i64 = 100;
/// Below this the tracks' loudness does not follow each other: no echo.
const MIN_CORRELATION: f32 = 0.35;
/// Frames of the computer's sound this much above its quietest are speech.
const ACTIVE_DB: f32 = 15.0;
/// A frame quieter than this holds no sound at all: below one step of 16-bit audio.
const DIGITAL_SILENCE_DB: f32 = -90.0;
/// The room keeps the sound a while: the echo expected in a frame takes the frames of the
/// computer's sound up to this many before it, fading by `TAIL_DECAY` per frame.
const TAIL: usize = 15;
const TAIL_DECAY: f32 = 0.7;
/// The found delay may be off by this much.
const SLACK: usize = 2;
/// The user is speaking when the microphone is louder than the expected echo by this factor of
/// energy (6 dB).
const MARGIN: f32 = 4.0;
/// The user speaks when this many frames in a row are clearly louder than the echo; a single
/// loud frame is the echo's own unevenness.
const MIN_TALK: usize = 3;
/// Around the user's speech this many frames stay too, echo and all: over the others the user's
/// quieter syllables are not louder than the echo, and a chopped phrase is not recognized. The
/// echo left there is removed from the text ([`drop_echo_words`]).
const KEEP_AROUND: usize = 25;
/// Silenced stretches shorter than this are kept: a pause inside the user's phrase.
const MIN_SILENCED: usize = 30;
/// A word of the user that matches a word of the others within this of the delayed time is echo.
const WORD_SLACK: f64 = 0.35;

/// The echo of the computer's sound in the microphone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Echo {
    /// Frames from the computer's sound to the microphone.
    pub lag: i64,
    /// Energy of the echo relative to the computer's sound.
    pub gain: f32,
    /// How closely the loudness of the tracks follows each other at that delay.
    pub correlation: f32,
}

impl Echo {
    pub fn delay_seconds(&self) -> f64 {
        self.lag as f64 * FRAME as f64 / RATE
    }
}

/// Mean energy of each 10 ms frame; a partial last frame counts.
#[cfg(test)]
pub fn energies(samples: &[f32]) -> Vec<f32> {
    let mut meter = EnergyMeter::default();
    meter.push(samples);
    meter.finish()
}

/// The mean energy of each 10 ms frame of audio that arrives in blocks.
#[derive(Default)]
pub struct EnergyMeter {
    sum: f32,
    count: usize,
    out: Vec<f32>,
}

impl EnergyMeter {
    pub fn push(&mut self, samples: &[f32]) {
        for sample in samples {
            self.sum += sample * sample;
            self.count += 1;
            if self.count == FRAME {
                self.out.push(self.sum / FRAME as f32);
                self.sum = 0.0;
                self.count = 0;
            }
        }
    }

    pub fn finish(mut self) -> Vec<f32> {
        if self.count > 0 {
            self.out.push(self.sum / self.count as f32);
        }
        self.out
    }
}

fn db(energy: f32) -> f32 {
    10.0 * (energy + 1e-10).log10()
}

fn percentile(values: &[f32], share: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    sorted[((sorted.len() - 1) as f32 * share) as usize]
}

/// The computer's sound at microphone frame `i` with delay `lag`, if the tracks overlap there.
fn shifted(others: &[f32], i: usize, lag: i64) -> Option<f32> {
    let at = i as i64 - lag;
    (0..others.len() as i64).contains(&at).then(|| others[at as usize])
}

/// The echo in the microphone, or `None` when its loudness does not follow the computer's
/// sound (headphones, or the others were silent).
pub fn find_echo(me: &[f32], others: &[f32]) -> Option<Echo> {
    let others_db: Vec<f32> = others.iter().map(|e| db(*e)).collect();
    // Their silence, measured where something sounds: digital silence (nothing played yet,
    // the gaps the timeline fills) would put it at nothing, and the noise of their open
    // microphone would count as their speech.
    let sounding: Vec<f32> =
        others_db.iter().copied().filter(|level| *level > DIGITAL_SILENCE_DB).collect();
    if sounding.is_empty() {
        return None;
    }
    let floor = percentile(&sounding, 0.1);
    let active = others_db.iter().filter(|level| **level > floor + ACTIVE_DB).count();
    // Under a second of the others speaking says nothing.
    if active < 100 {
        return None;
    }
    // Only where the others speak: there the microphone holds their echo, if any, and the
    // user's own speech elsewhere does not blur the match. Levels in dB, so the syllables of
    // quiet and loud stretches count alike.
    let me_db: Vec<f32> = me.iter().map(|e| db(*e)).collect();
    let speaking = |at: i64| others_db[at as usize] > floor + ACTIVE_DB;
    let mut best: Option<(i64, f32)> = None;
    for lag in MIN_LAG..=MAX_LAG {
        let pairs = (0..me_db.len()).filter_map(|i| {
            let at = i as i64 - lag;
            ((0..others_db.len() as i64).contains(&at) && speaking(at))
                .then(|| (me_db[i], others_db[at as usize]))
        });
        let correlation = pearson(pairs);
        if best.is_none_or(|(_, top)| correlation > top) {
            best = Some((lag, correlation));
        }
    }
    let (lag, correlation) = best?;
    #[cfg(test)]
    LAST_CORRELATION.with(|last| last.set(correlation));
    if correlation < MIN_CORRELATION {
        return None;
    }
    // Where the others speak, most microphone frames hold only their echo: the median ratio
    // is the echo's level, the user's own speech over it does not move it much.
    let ratios: Vec<f32> = (0..me.len())
        .filter_map(|i| {
            let at = i as i64 - lag;
            let source = shifted(others, i, lag)?;
            (others_db[at as usize] > floor + ACTIVE_DB).then(|| me[i] / source.max(1e-12))
        })
        .collect();
    Some(Echo { lag, gain: percentile(&ratios, 0.5), correlation })
}

#[cfg(test)]
thread_local! {
    /// The best correlation of the last search, for tests.
    static LAST_CORRELATION: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

fn pearson(pairs: impl Iterator<Item = (f32, f32)>) -> f32 {
    let (mut n, mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
    for (x, y) in pairs {
        let (x, y) = (f64::from(x), f64::from(y));
        n += 1.0;
        sx += x;
        sy += y;
        sxx += x * x;
        syy += y * y;
        sxy += x * y;
    }
    if n < 2.0 {
        return 0.0;
    }
    let covariance = sxy - sx * sy / n;
    let spread = ((sxx - sx * sx / n) * (syy - sy * sy / n)).sqrt();
    if spread <= 0.0 { 0.0 } else { (covariance / spread) as f32 }
}

/// For each microphone frame, whether it holds only the echo and is silenced.
pub fn echo_mask(me: &[f32], others: &[f32], echo: &Echo) -> Vec<bool> {
    let expected = |i: usize| {
        let mut top = 0f32;
        for back in 0..=TAIL + SLACK {
            let at = i as i64 - echo.lag - back as i64 + SLACK as i64;
            if (0..others.len() as i64).contains(&at) {
                let fade = TAIL_DECAY.powi(back.saturating_sub(2 * SLACK) as i32);
                top = top.max(others[at as usize] * fade);
            }
        }
        top * echo.gain
    };
    // The microphone's own noise: an echo below it is not there.
    let noise = percentile(me, 0.1) * 2.0;
    let mut mask = vec![true; me.len()];
    // Where no echo is heard every frame is kept: the user, or quiet.
    for (i, frame) in mask.iter_mut().enumerate() {
        if expected(i) <= noise {
            *frame = false;
        }
    }
    // Over the echo: frames clearly louder than it, several in a row, are the user; the stretch
    // around them is kept whole.
    let over = |i: usize| {
        let echo_here = expected(i);
        echo_here > noise && me[i] >= echo_here * MARGIN
    };
    let mut i = 0;
    while i < me.len() {
        if !over(i) {
            i += 1;
            continue;
        }
        let start = i;
        while i < me.len() && over(i) {
            i += 1;
        }
        if i - start >= MIN_TALK {
            let to = (i + KEEP_AROUND).min(mask.len());
            mask[start.saturating_sub(KEEP_AROUND)..to].iter_mut().for_each(|frame| *frame = false);
        }
    }
    // Short silenced stretches stay as recorded.
    let mut i = 0;
    while i < mask.len() {
        if !mask[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < mask.len() && mask[i] {
            i += 1;
        }
        if i - start < MIN_SILENCED {
            mask[start..i].iter_mut().for_each(|frame| *frame = false);
        }
    }
    mask
}

/// Silences the masked frames of `samples`, which start at sample `offset` of the track, with
/// a short fade so no clicks are made.
pub fn gate(samples: &mut [f32], offset: usize, mask: &[bool]) {
    const RAMP: usize = 32;
    for (index, sample) in samples.iter_mut().enumerate() {
        let position = offset + index;
        let frame = position / FRAME;
        let silenced = |frame: usize| mask.get(frame).copied().unwrap_or(false);
        if !silenced(frame) {
            continue;
        }
        let within = position % FRAME;
        // Fade at the edges next to kept frames.
        let before = frame > 0 && !silenced(frame - 1) && within < RAMP;
        let after = !silenced(frame + 1) && FRAME - within <= RAMP;
        let factor = if before {
            1.0 - within as f32 / RAMP as f32
        } else if after {
            1.0 - (FRAME - within) as f32 / RAMP as f32
        } else {
            0.0
        };
        *sample *= factor;
    }
}

/// Words compared without case and punctuation.
fn plain(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn similar(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let longest = a.len().max(b.len());
    longest >= 4 && edit_distance(&a, &b) * 4 <= longest
}

fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let replaced = previous + usize::from(x != y);
            previous = row[j + 1];
            row[j + 1] = replaced.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

/// The user's words without the ones that echo the others: the same word, said by the others
/// `delay` seconds earlier. A matching word stands alone only if it is long; short ones go
/// only next to another match, so the user's own "yes" over the others' "yes" stays.
pub fn drop_echo_words(me: Vec<Word>, others: &[Word], delay: f64) -> Vec<Word> {
    let echoes: Vec<bool> = me
        .iter()
        .map(|word| {
            let text = plain(&word.text);
            let at = word.midpoint() - delay;
            others.iter().any(|other| {
                (other.midpoint() - at).abs() <= WORD_SLACK && similar(&text, &plain(&other.text))
            })
        })
        .collect();
    me.into_iter()
        .enumerate()
        .filter(|(i, word)| {
            if !echoes[*i] {
                return true;
            }
            let neighbour =
                (*i > 0 && echoes[i - 1]) || echoes.get(i + 1).copied().unwrap_or(false);
            let long = plain(&word.text).chars().count() >= 5;
            !(neighbour || long)
        })
        .map(|(_, word)| word)
        .collect()
}

/// Energies of a whole recording, read block by block.
pub fn file_energies(path: &Path, cancel: &AtomicBool) -> Result<Vec<f32>, String> {
    let mut decoder = FileDecoder::open(path).map_err(|e| e.to_string())?;
    let mut meter = EnergyMeter::default();
    while let Some(block) = decoder.next_block().map_err(|e| e.to_string())? {
        if cancel.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        meter.push(&block);
    }
    Ok(meter.finish())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn a_stop_ends_reading_a_track() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("echo-stop-{}.opus", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = crate::audio::ogg_opus::OggOpusWriter::new(file, 1).unwrap();
        writer.write(&vec![0.0; 16_000 * 3]).unwrap();
        writer.finish().unwrap();
        assert!(file_energies(&path, &AtomicBool::new(true)).is_err());
        assert!(file_energies(&path, &AtomicBool::new(false)).is_ok());
    }

    /// Speech-like sound: bursts of noise with syllable rhythm, in `turns` of (start, end) seconds.
    pub(crate) fn voice(seed: u32, seconds: f64, turns: &[(f64, f64)], loudness: f32) -> Vec<f32> {
        let mut state = seed;
        let mut noise = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        let rate = 16_000.0;
        (0..(seconds * rate) as usize)
            .map(|n| {
                let t = n as f64 / rate;
                let on = turns.iter().any(|(start, end)| (*start..*end).contains(&t));
                // Syllables at a seed-dependent pace, about four a second, a little uneven.
                let pace = 3.5 + f64::from(seed % 7) * 0.25;
                let wobble = 0.3 * (std::f64::consts::TAU * 0.7 * t + f64::from(seed)).sin();
                let syllable = (std::f64::consts::TAU * pace * t + wobble + f64::from(seed))
                    .sin()
                    .max(0.0) as f32;
                if on { noise() * syllable * loudness } else { noise() * 0.001 }
            })
            .collect()
    }

    /// What a microphone hears of a speaker: delayed, quieter, with a short room tail.
    pub(crate) fn through_the_room(source: &[f32], delay_ms: usize, level: f32) -> Vec<f32> {
        let delay = delay_ms * 16;
        let mut out = vec![0.0; source.len()];
        for (i, sample) in source.iter().enumerate() {
            for (tap, weight) in [(0, 1.0), (400, 0.4), (1_100, 0.15)] {
                if let Some(slot) = out.get_mut(i + delay + tap) {
                    *slot += sample * level * weight;
                }
            }
        }
        out
    }

    pub(crate) fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b).map(|(x, y)| x + y).collect()
    }

    fn energy_in(samples: &[f32], from: f64, to: f64) -> f32 {
        let range = (from * 16_000.0) as usize..(to * 16_000.0) as usize;
        samples[range].iter().map(|s| s * s).sum()
    }

    pub(crate) const OTHERS_TURNS: [(f64, f64); 3] = [(0.5, 4.0), (8.0, 11.0), (15.0, 19.0)];
    pub(crate) const MY_TURNS: [(f64, f64); 2] = [(4.5, 7.5), (11.5, 14.5)];

    #[test]
    fn the_echo_is_found_with_its_delay() {
        let others = voice(7, 20.0, &OTHERS_TURNS, 0.3);
        let me = mix(&through_the_room(&others, 120, 0.25), &voice(99, 20.0, &MY_TURNS, 0.4));
        let echo = find_echo(&energies(&me), &energies(&others)).expect("echo");
        // The room's reflections pull the match up to a frame later; the mask allows for that.
        assert!((12..=13).contains(&echo.lag), "{echo:?}");
        assert!((echo.delay_seconds() - echo.lag as f64 * 0.01).abs() < 1e-9);
        assert!(echo.correlation > 0.4, "{echo:?}");
    }

    /// The computer's track begins with digital silence (nothing played before the call, or
    /// the gaps the timeline fills): it does not lower the others' silence to nothing, so the
    /// noise of their open microphone does not count as their speech.
    #[test]
    fn digital_silence_before_the_call_does_not_hide_the_echo() {
        let before = vec![0.0; 16_000 * 4];
        // Their microphone stays open: its noise fills their pauses, at about -75 dB.
        let open_mic: Vec<f32> = voice(11, 20.0, &[], 0.0).iter().map(|s| s * 0.3).collect();
        let call = mix(&voice(7, 20.0, &OTHERS_TURNS, 0.3), &open_mic);
        let others = [before.as_slice(), &call].concat();
        let mine = [before.as_slice(), &voice(99, 20.0, &MY_TURNS, 0.4)].concat();
        // The microphone hears the room also before the call.
        // A quiet echo: speakers turned down, or far from the microphone.
        let me = mix(&mix(&through_the_room(&others, 120, 0.04), &mine), &voice(5, 24.0, &[], 0.0));
        let echo = find_echo(&energies(&me), &energies(&others)).expect("echo");
        assert!((12..=13).contains(&echo.lag), "{echo:?}");
    }

    #[test]
    fn headphones_give_no_echo() {
        let others = voice(7, 20.0, &OTHERS_TURNS, 0.3);
        let me = voice(99, 20.0, &MY_TURNS, 0.4);
        assert_eq!(find_echo(&energies(&me), &energies(&others)), None);
        // Well below the threshold, not just under it.
        let found = LAST_CORRELATION.with(std::cell::Cell::get);
        assert!(found < MIN_CORRELATION / 2.0, "{found}");
        // Nobody on the other side.
        let silent = voice(3, 20.0, &[], 0.3);
        assert_eq!(find_echo(&energies(&me), &energies(&silent)), None);
    }

    #[test]
    fn the_echo_is_silenced_and_the_user_kept() {
        let others = voice(7, 20.0, &OTHERS_TURNS, 0.3);
        let mine = voice(99, 20.0, &MY_TURNS, 0.4);
        let mut me = mix(&through_the_room(&others, 120, 0.25), &mine);
        let (me_e, others_e) = (energies(&me), energies(&others));
        let echo = find_echo(&me_e, &others_e).unwrap();
        let mask = echo_mask(&me_e, &others_e, &echo);
        let before: Vec<f32> = me.clone();
        gate(&mut me, 0, &mask);
        for (start, end) in OTHERS_TURNS {
            let left = energy_in(&me, start + 0.2, end) / energy_in(&before, start + 0.2, end);
            assert!(left < 0.1, "echo at {start}–{end}: {left} of its energy left");
        }
        for (start, end) in MY_TURNS {
            let kept = energy_in(&me, start, end) / energy_in(&before, start, end);
            assert!(kept > 0.95, "user at {start}–{end}: {kept} of the energy kept");
        }
    }

    #[test]
    fn late_and_loud_speakers_are_handled() {
        let others = voice(7, 20.0, &OTHERS_TURNS, 0.3);
        let mine = voice(99, 20.0, &MY_TURNS, 0.4);
        // A Bluetooth speaker 0.3 s late; laptop speakers about as loud as the user.
        for (delay_ms, level) in [(300, 0.25), (40, 1.0)] {
            let mut me = mix(&through_the_room(&others, delay_ms, level), &mine);
            let (me_e, others_e) = (energies(&me), energies(&others));
            let echo = find_echo(&me_e, &others_e).expect("echo");
            assert!(echo.lag.abs_diff(delay_ms as i64 / 10) <= 1, "{delay_ms} ms: {echo:?}");
            let before = me.clone();
            gate(&mut me, 0, &echo_mask(&me_e, &others_e, &echo));
            for (start, end) in OTHERS_TURNS {
                let left = energy_in(&me, start + 0.4, end) / energy_in(&before, start + 0.4, end);
                assert!(left < 0.1, "{delay_ms} ms, echo at {start}: {left} left");
            }
            for (start, end) in MY_TURNS {
                let kept = energy_in(&me, start, end) / energy_in(&before, start, end);
                assert!(kept > 0.9, "{delay_ms} ms, user at {start}: {kept} kept");
            }
        }
    }

    #[test]
    fn the_user_over_the_others_is_kept() {
        let others = voice(7, 20.0, &OTHERS_TURNS, 0.3);
        // The user also speaks inside the others' second turn.
        let mine = voice(99, 20.0, &[(4.5, 7.5), (9.0, 10.0)], 0.5);
        let mut me = mix(&through_the_room(&others, 80, 0.2), &mine);
        let (me_e, others_e) = (energies(&me), energies(&others));
        let echo = find_echo(&me_e, &others_e).unwrap();
        let before = me.clone();
        gate(&mut me, 0, &echo_mask(&me_e, &others_e, &echo));
        let kept = energy_in(&me, 9.0, 10.0) / energy_in(&before, 9.0, 10.0);
        assert!(kept > 0.8, "{kept}");
    }

    #[test]
    fn gating_by_blocks_matches_the_whole() {
        let mask: Vec<bool> = (0..20).map(|i| (5..12).contains(&i)).collect();
        let source: Vec<f32> = (0..20 * FRAME).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut whole = source.clone();
        gate(&mut whole, 0, &mask);
        let mut blocks = source.clone();
        for (index, block) in blocks.chunks_mut(333).enumerate() {
            gate(block, index * 333, &mask);
        }
        assert_eq!(whole, blocks);
        assert!(whole[8 * FRAME..10 * FRAME].iter().all(|s| *s == 0.0));
        assert_eq!(whole[..5 * FRAME], source[..5 * FRAME]);
    }

    fn word(start: f64, text: &str) -> Word {
        Word { start, end: start + 0.3, text: format!(" {text}") }
    }

    #[test]
    fn echoed_words_are_dropped_and_the_users_kept() {
        let others =
            vec![word(1.0, "Давайте"), word(1.4, "обсудим"), word(1.8, "релиз."), word(6.0, "да")];
        let me = vec![
            // The echo, 0.1 s later and recognized a little differently.
            word(1.1, "давайте"),
            word(1.5, "обсудем"),
            word(1.9, "релиз"),
            // The user's own words.
            word(3.0, "Хорошо,"),
            word(3.4, "давайте."),
            // The user's "да" at the same moment as the others' "да": short and alone, kept.
            word(6.1, "да"),
        ];
        let kept: Vec<String> =
            drop_echo_words(me, &others, 0.1).into_iter().map(|w| w.text).collect();
        assert_eq!(kept, [" Хорошо,", " давайте.", " да"]);
    }

    #[test]
    fn words_are_compared_loosely() {
        assert!(similar("обсудим", "обсудем"));
        assert!(similar("kubernetes", "kubernetis"));
        assert!(!similar("да", "до"));
        assert!(!similar("релиз", "вопрос"));
        assert_eq!(plain(" Релиз!"), "релиз");
    }

    /// A real meeting: `AHEM_MEETING_DIR="<data>/meetings/<meeting folder>" cargo test -- --ignored
    /// real_meeting --nocapture`. Prints what was found and writes `me-checked.opus` next to
    /// the tracks to listen to; the recording itself is not changed.
    #[test]
    #[ignore = "needs a recorded meeting in AHEM_MEETING_DIR"]
    fn real_meeting() {
        let dir =
            std::path::PathBuf::from(std::env::var("AHEM_MEETING_DIR").expect("AHEM_MEETING_DIR"));
        let never = AtomicBool::new(false);
        let me = file_energies(&dir.join("me.opus"), &never).unwrap();
        let others = file_energies(&dir.join("others.opus"), &never).unwrap();
        println!("me {} frames, others {} frames", me.len(), others.len());
        let Some(echo) = find_echo(&me, &others) else {
            println!("no echo: {}", LAST_CORRELATION.with(std::cell::Cell::get));
            return;
        };
        let mask = echo_mask(&me, &others, &echo);
        // Half-second steps: the others (at the delay), the microphone, the share kept.
        for start in (0..me.len()).step_by(50) {
            let end = (start + 50).min(me.len());
            let level =
                |values: &[f32]| db(values.iter().sum::<f32>() / values.len().max(1) as f32);
            let shifted_others: Vec<f32> =
                (start..end).filter_map(|i| shifted(&others, i, echo.lag)).collect();
            let kept = mask[start..end].iter().filter(|m| !**m).count() * 100 / (end - start);
            println!(
                "{:5.1} s  others {:6.1} dB  me {:6.1} dB  kept {kept:3} %",
                start as f64 / 100.0,
                level(&shifted_others),
                level(&me[start..end])
            );
        }
        // AHEM_SPOTS=545,2640: 0.4 s of frames from each of these frames on.
        if let Ok(spots) = std::env::var("AHEM_SPOTS") {
            for spot in spots.split(',').map(|s| s.parse::<usize>().unwrap()) {
                for i in spot..(spot + 40).min(me.len()) {
                    let others_here = shifted(&others, i, echo.lag).unwrap_or(0.0);
                    println!(
                        "  {:.2}: others {:6.1} me {:6.1} echo {:6.1} {}",
                        i as f64 / 100.0,
                        db(others_here),
                        db(me[i]),
                        db(others_here * echo.gain),
                        if mask[i] { "silenced" } else { "kept" }
                    );
                }
            }
        }
        let silenced = mask.iter().filter(|m| **m).count();
        println!(
            "{echo:?}: {silenced} of {} frames silenced ({:.0} %)",
            mask.len(),
            100.0 * silenced as f64 / mask.len() as f64
        );
        let mut samples = crate::files::decode(&dir.join("me.opus")).unwrap().samples;
        gate(&mut samples, 0, &mask);
        let file = std::fs::File::create(dir.join("me-checked.opus")).unwrap();
        let mut writer = crate::audio::ogg_opus::OggOpusWriter::new(file, 4).unwrap();
        writer.write(&samples).unwrap();
        writer.finish().unwrap();

        // AHEM_MODEL_DIR=<data>/models/<sherpa model id>: the transcript as the app makes it.
        let Ok(model_dir) = std::env::var("AHEM_MODEL_DIR") else { return };
        use crate::engines::sherpa::SherpaEngine;
        use crate::engines::whisper_server::TranscribeOptions;
        use crate::files::{join_text, merge, parse_words, plan};
        use crate::meetings::transcript::{Speaker, turns};
        let model_dir = std::path::PathBuf::from(model_dir);
        let id = model_dir.file_name().unwrap().to_string_lossy().into_owned();
        let spec = crate::storage::catalog::find(&id).unwrap();
        let engine = SherpaEngine::load(spec.sherpa.as_ref().unwrap(), &model_dir, 6).unwrap();
        let options = TranscribeOptions::default();
        let words = |audio: &[f32]| {
            let parts = plan(audio).into_iter().map(|window| {
                let json = engine.transcribe_words(&window.audio(audio), 16_000, &options).unwrap();
                (window, parse_words(&json, &window).unwrap())
            });
            merge(parts.collect::<Vec<_>>())
        };
        let raw = words(&crate::files::decode(&dir.join("me.opus")).unwrap().samples);
        let cleaned = words(&samples);
        let others_words = words(&crate::files::decode(&dir.join("others.opus")).unwrap().samples);
        println!("\nme as recorded: {}", join_text(&raw));
        println!("me without echo: {}", join_text(&cleaned));
        let mine = drop_echo_words(cleaned, &others_words, echo.delay_seconds());
        println!("me after the text check: {}\n", join_text(&mine));
        let others_words = others_words.into_iter().map(|word| (Speaker::Others, word)).collect();
        for turn in turns(mine, others_words) {
            let name = if turn.speaker == Speaker::Me { "me" } else { "others" };
            println!("{:5.1}–{:5.1} {name:6} {}", turn.start, turn.end, turn.text());
        }
    }
}
