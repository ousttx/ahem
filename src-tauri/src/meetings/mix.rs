//! Both tracks of a meeting in one file, to listen to the call as it was. The two
//! tracks start at the same moment and fill their gaps with silence, so sample `n` of one is
//! sample `n` of the other.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::audio::ogg_opus::OggOpusWriter;
use crate::files::FileDecoder;

/// Writes the sum of `tracks` to `target`, through a `.part` file so that a stopped or failed
/// mix leaves nothing half written. A track that cannot be opened is left out; with none at all
/// it is an error. Stops when `cancel` is set.
pub fn mix(tracks: &[&Path], target: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let mut decoders: Vec<FileDecoder> =
        tracks.iter().filter_map(|path| FileDecoder::open(path).ok()).collect();
    if decoders.is_empty() {
        return Err("no track to mix".into());
    }
    let part = part_path(target);
    let written = write(&mut decoders, &part, cancel);
    match written {
        Ok(true) => fs::rename(&part, target).map_err(|error| error.to_string()),
        Ok(false) => {
            let _ = fs::remove_file(&part);
            Err("stopped".into())
        }
        Err(error) => {
            let _ = fs::remove_file(&part);
            Err(error)
        }
    }
}

/// Where the mix is written until it is whole.
pub fn part_path(target: &Path) -> std::path::PathBuf {
    target.with_extension("opus.part")
}

/// `false` when stopped.
fn write(decoders: &mut [FileDecoder], part: &Path, cancel: &AtomicBool) -> Result<bool, String> {
    let file = File::create(part).map_err(|error| error.to_string())?;
    let mut writer =
        OggOpusWriter::new(BufWriter::new(file), 4).map_err(|error| error.to_string())?;
    // Decoded and not yet mixed, per track; a track that has ended is silence.
    let mut pending: Vec<Vec<f32>> = vec![Vec::new(); decoders.len()];
    let mut ended = vec![false; decoders.len()];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        for (index, decoder) in decoders.iter_mut().enumerate() {
            while !ended[index] && pending[index].is_empty() {
                match decoder.next_block() {
                    Ok(Some(block)) => pending[index].extend(block),
                    // A damaged end of a track ends it; the rest is still mixed.
                    Ok(None) | Err(_) => ended[index] = true,
                }
            }
        }
        // As far as every track that goes on has samples; the longest one sets the end.
        let ready = pending
            .iter()
            .zip(&ended)
            .filter(|(_, ended)| !**ended)
            .map(|(samples, _)| samples.len())
            .min()
            .unwrap_or_else(|| pending.iter().map(Vec::len).max().unwrap_or(0));
        if ready == 0 {
            break;
        }
        let mut mixed = vec![0.0f32; ready];
        for samples in &mut pending {
            let take = ready.min(samples.len());
            for (sum, sample) in mixed.iter_mut().zip(samples.drain(..take)) {
                *sum += sample;
            }
        }
        for sample in &mut mixed {
            *sample = sample.clamp(-1.0, 1.0);
        }
        writer.write(&mixed).map_err(|error| error.to_string())?;
    }
    writer.finish().map_err(|error| error.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::f32::consts::TAU;
    use std::path::PathBuf;

    use super::*;
    use crate::files::decode;

    const RATE: usize = 16_000;

    fn test_dir(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("mix-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tone(path: &Path, hertz: f32, seconds: f32, from: f32) {
        let mut writer =
            OggOpusWriter::new(BufWriter::new(File::create(path).unwrap()), 1).unwrap();
        let samples: Vec<f32> = (0..(seconds * RATE as f32) as usize)
            .map(|n| {
                let at = n as f32 / RATE as f32;
                if at < from { 0.0 } else { 0.3 * (TAU * hertz * at).sin() }
            })
            .collect();
        writer.write(&samples).unwrap();
        writer.finish().unwrap();
    }

    /// Loudness of `samples` between two times, in seconds.
    fn level(samples: &[f32], from: f32, to: f32) -> f32 {
        let part = &samples[(from * RATE as f32) as usize..(to * RATE as f32) as usize];
        (part.iter().map(|s| s * s).sum::<f32>() / part.len() as f32).sqrt()
    }

    #[test]
    fn both_tracks_are_heard_in_one_file_as_long_as_the_longer() {
        let dir = test_dir("both");
        let (me, others, call) =
            (dir.join("me.opus"), dir.join("others.opus"), dir.join("call.opus"));
        // The user from the start for 2 s; the others from 1 s to 3 s.
        tone(&me, 440.0, 2.0, 0.0);
        tone(&others, 660.0, 3.0, 1.0);
        mix(&[&me, &others], &call, &AtomicBool::new(false)).unwrap();
        assert!(!dir.join("call.opus.part").exists());

        let mixed = decode(&call).unwrap().samples;
        let seconds = mixed.len() as f32 / RATE as f32;
        assert!((seconds - 3.0).abs() < 0.1, "{seconds} s");
        let alone = level(&mixed, 0.2, 0.8);
        let together = level(&mixed, 1.2, 1.8);
        let others_alone = level(&mixed, 2.2, 2.8);
        assert!(alone > 0.15 && others_alone > 0.15, "{alone} {others_alone}");
        // Two tones at once are louder than one.
        assert!(together > alone * 1.2, "{together} vs {alone}");
    }

    #[test]
    fn a_missing_track_is_left_out_and_a_stop_leaves_nothing() {
        let dir = test_dir("missing");
        let (me, call) = (dir.join("me.opus"), dir.join("call.opus"));
        tone(&me, 440.0, 1.0, 0.0);
        mix(&[&me, &dir.join("others.opus")], &call, &AtomicBool::new(false)).unwrap();
        let seconds = decode(&call).unwrap().samples.len() as f32 / RATE as f32;
        assert!((seconds - 1.0).abs() < 0.1, "{seconds} s");

        fs::remove_file(&call).unwrap();
        assert!(mix(&[&me], &call, &AtomicBool::new(true)).is_err());
        assert!(!call.exists() && !dir.join("call.opus.part").exists());
        assert!(mix(&[&dir.join("none.opus")], &call, &AtomicBool::new(false)).is_err());
    }
}
