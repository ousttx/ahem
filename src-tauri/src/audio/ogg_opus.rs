//! Writing 16 kHz mono audio as an Ogg Opus file while it is recorded.
//!
//! libopus (`opusic-sys`, the build that also decodes) encodes 20 ms packets; a page goes out
//! about once a second, so a crash loses at most the last second and the rest still decodes.

use std::io::{self, Write};

use opusic_sys::{
    OPUS_APPLICATION_VOIP, OPUS_GET_LOOKAHEAD_REQUEST, OPUS_OK, OPUS_SET_BITRATE_REQUEST,
    OpusEncoder, opus_encode_float, opus_encoder_create, opus_encoder_ctl, opus_encoder_destroy,
};

use super::resample::TARGET_RATE;

/// 20 ms at 16 kHz.
const FRAME: usize = TARGET_RATE as usize / 50;
/// Ogg Opus counts granules at 48 kHz whatever the input rate.
const GRANULES_PER_SAMPLE: u64 = 48_000 / TARGET_RATE as u64;
const BITRATE: i32 = 24_000;
/// Packets per page: one second.
const PAGE_PACKETS: usize = 50;
/// Larger than any 20 ms packet at this bitrate.
const MAX_PACKET: usize = 1_500;

struct Encoder(*mut OpusEncoder);

// SAFETY: the encoder is used by one owner at a time; libopus keeps no thread-local state.
unsafe impl Send for Encoder {}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: created by `opus_encoder_create` and destroyed only here.
        unsafe { opus_encoder_destroy(self.0) }
    }
}

impl Encoder {
    /// The encoder and its lookahead in input samples.
    fn new() -> io::Result<(Self, u16)> {
        let mut error = 0;
        // SAFETY: `error` outlives the call; the encoder is checked before use.
        let raw = unsafe {
            opus_encoder_create(TARGET_RATE as i32, 1, OPUS_APPLICATION_VOIP, &mut error)
        };
        if error != OPUS_OK || raw.is_null() {
            return Err(io::Error::other(format!("opus encoder: error {error}")));
        }
        let encoder = Self(raw);
        let mut lookahead: i32 = 0;
        // SAFETY: valid encoder; the requests take an `i32` value and an `*mut i32`.
        let (bitrate, request) = unsafe {
            (
                opus_encoder_ctl(encoder.0, OPUS_SET_BITRATE_REQUEST, BITRATE),
                opus_encoder_ctl(encoder.0, OPUS_GET_LOOKAHEAD_REQUEST, &mut lookahead),
            )
        };
        if bitrate != OPUS_OK || request != OPUS_OK {
            return Err(io::Error::other("opus encoder: settings refused"));
        }
        Ok((encoder, u16::try_from(lookahead).unwrap_or(0)))
    }

    fn encode(&mut self, frame: &[f32]) -> io::Result<Vec<u8>> {
        debug_assert_eq!(frame.len(), FRAME);
        let mut packet = vec![0; MAX_PACKET];
        // SAFETY: valid encoder; `frame` holds `FRAME` samples and `packet` its stated length.
        let length = unsafe {
            opus_encode_float(
                self.0,
                frame.as_ptr(),
                FRAME as i32,
                packet.as_mut_ptr(),
                packet.len() as i32,
            )
        };
        let length = usize::try_from(length)
            .map_err(|_| io::Error::other(format!("opus encoder: error {length}")))?;
        packet.truncate(length);
        Ok(packet)
    }
}

pub struct OggOpusWriter<W: Write> {
    out: W,
    encoder: Encoder,
    serial: u32,
    sequence: u32,
    /// Pre-skip in 48 kHz granules.
    pre_skip: u64,
    /// Input not yet a whole frame.
    frame: Vec<f32>,
    /// Encoded packets of the page being filled.
    packets: Vec<Vec<u8>>,
    /// Input samples taken so far.
    samples: u64,
    /// Samples covered by the packets already written or waiting.
    encoded: u64,
}

impl<W: Write> OggOpusWriter<W> {
    /// Writes the two header pages. `serial` tells streams apart; any number will do.
    pub fn new(mut out: W, serial: u32) -> io::Result<Self> {
        let (encoder, lookahead) = Encoder::new()?;
        let pre_skip = u64::from(lookahead) * GRANULES_PER_SAMPLE;
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 1]); // version, mono
        head.extend_from_slice(&(pre_skip as u16).to_le_bytes());
        head.extend_from_slice(&TARGET_RATE.to_le_bytes()); // the original rate
        head.extend_from_slice(&0i16.to_le_bytes()); // gain
        head.push(0); // mapping family 0
        let vendor = b"ahem";
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0u32.to_le_bytes()); // no comments
        out.write_all(&page(serial, 0, 0x02, 0, &[head]))?;
        out.write_all(&page(serial, 1, 0, 0, &[tags]))?;
        out.flush()?;
        Ok(Self {
            out,
            encoder,
            serial,
            sequence: 2,
            pre_skip,
            frame: Vec::with_capacity(FRAME),
            packets: Vec::new(),
            samples: 0,
            encoded: 0,
        })
    }

    /// Takes 16 kHz mono samples; a page is written once a second of packets is ready.
    pub fn write(&mut self, mut samples: &[f32]) -> io::Result<()> {
        self.samples += samples.len() as u64;
        while !samples.is_empty() {
            let take = (FRAME - self.frame.len()).min(samples.len());
            self.frame.extend_from_slice(&samples[..take]);
            samples = &samples[take..];
            if self.frame.len() == FRAME {
                let packet = self.encoder.encode(&self.frame)?;
                self.frame.clear();
                // A full page is written when the next packet comes, not when it fills: the last
                // page of the stream then always holds a packet, so its smaller granule (the
                // end of the audio) never follows a page that already counted past it. A page
                // also holds at most 255 lacing values; a long packet may close it early.
                if self.packets.len() == PAGE_PACKETS
                    || lacing_values(&self.packets) + packet.len() / 255 + 1 > 255
                {
                    self.flush_page(false)?;
                }
                self.packets.push(packet);
                self.encoded += FRAME as u64;
            }
        }
        Ok(())
    }

    /// Seconds of audio taken so far.
    #[cfg(test)]
    pub fn seconds(&self) -> f64 {
        self.samples as f64 / f64::from(TARGET_RATE)
    }

    /// Encodes the rest (padded with silence, which the end granule trims) and ends the stream.
    pub fn finish(mut self) -> io::Result<W> {
        // The encoder's lookahead holds the last samples back: push them out with silence, in
        // as few frames as that takes, so the end trims less than one packet (RFC 7845 §4.5).
        let tail = (self.pre_skip / GRANULES_PER_SAMPLE) as usize;
        let target = self.samples;
        let padding = (self.frame.len() + tail).div_ceil(FRAME) * FRAME - self.frame.len();
        let silence = vec![0.0; padding];
        self.write(&silence)?;
        self.samples = target;
        self.flush_page(true)?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn flush_page(&mut self, last: bool) -> io::Result<()> {
        // RFC 7845: a page's granule counts all decoded samples, pre-skip included; the last
        // page's is smaller, so the padding after the end is trimmed.
        let granule = if last {
            self.pre_skip + self.samples * GRANULES_PER_SAMPLE
        } else {
            self.encoded * GRANULES_PER_SAMPLE
        };
        let packets = std::mem::take(&mut self.packets);
        let header_type = if last { 0x04 } else { 0 };
        self.out.write_all(&page(self.serial, self.sequence, header_type, granule, &packets))?;
        self.out.flush()?;
        self.sequence += 1;
        Ok(())
    }
}

fn lacing_values(packets: &[Vec<u8>]) -> usize {
    packets.iter().map(|packet| packet.len() / 255 + 1).sum()
}

/// One Ogg page holding whole packets (at most 255 lacing values in all).
fn page(serial: u32, sequence: u32, header_type: u8, granule: u64, packets: &[Vec<u8>]) -> Vec<u8> {
    let mut lacing = Vec::new();
    for packet in packets {
        lacing.extend(std::iter::repeat_n(255u8, packet.len() / 255));
        lacing.push((packet.len() % 255) as u8);
    }
    debug_assert!(lacing.len() <= 255, "{} lacing values", lacing.len());
    let mut page = b"OggS".to_vec();
    page.extend_from_slice(&[0, header_type]);
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&sequence.to_le_bytes());
    page.extend_from_slice(&[0; 4]); // CRC, filled below
    page.push(lacing.len() as u8);
    page.extend_from_slice(&lacing);
    for packet in packets {
        page.extend_from_slice(packet);
    }
    let crc = crc32(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

/// Ogg's CRC-32: polynomial 0x04C11DB7, no reflection, zero start.
fn crc32(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |mut crc, byte| {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
        crc
    })
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::files::decode;

    fn test_path(name: &str) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts").join(
            format!("ogg-opus-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)),
        );
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn tone(frequency: f64, seconds: f64) -> Vec<f32> {
        let count = (f64::from(TARGET_RATE) * seconds) as usize;
        (0..count)
            .map(|i| {
                (0.5 * (2.0 * PI * frequency * i as f64 / f64::from(TARGET_RATE)).sin()) as f32
            })
            .collect()
    }

    fn crossings(samples: &[f32]) -> usize {
        samples.windows(2).filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0)).count()
    }

    #[test]
    fn a_recording_decodes_back_with_its_length() {
        let path = test_path("tone.opus");
        let audio = tone(500.0, 3.33);
        let mut writer = OggOpusWriter::new(fs::File::create(&path).unwrap(), 7).unwrap();
        for block in audio.chunks(777) {
            writer.write(block).unwrap();
        }
        assert!((writer.seconds() - 3.33).abs() < 1e-3);
        writer.finish().unwrap();
        let decoded = decode(&path).unwrap();
        let difference = decoded.samples.len().abs_diff(audio.len());
        assert!(difference <= FRAME, "{} vs {}", decoded.samples.len(), audio.len());
        // 500 Hz: a thousand zero crossings a second, give or take the edges.
        let found = crossings(&decoded.samples[1_600..decoded.samples.len() - 1_600]);
        let expected = (3.33 - 0.2) * 1_000.0;
        assert!((found as f64 - expected).abs() < 20.0, "{found} crossings");
    }

    #[test]
    fn a_file_cut_off_by_a_crash_still_decodes() {
        let path = test_path("cut.opus");
        let mut writer = OggOpusWriter::new(fs::File::create(&path).unwrap(), 9).unwrap();
        writer.write(&tone(300.0, 2.5)).unwrap();
        // No `finish`: the writer is dropped as a crash would leave it.
        drop(writer);
        let decoded = decode(&path).unwrap();
        // Two whole pages of one second each were written.
        let seconds = decoded.samples.len() as f64 / f64::from(TARGET_RATE);
        assert!((1.9..=2.1).contains(&seconds), "{seconds} s");
    }

    #[test]
    fn page_granules_count_the_decoded_samples() {
        let path = test_path("granules.opus");
        let mut writer = OggOpusWriter::new(fs::File::create(&path).unwrap(), 5).unwrap();
        writer.write(&tone(400.0, 2.5)).unwrap();
        writer.finish().unwrap();
        let bytes = fs::read(&path).unwrap();
        // Granules of the audio pages: after the two header pages.
        let mut granules = Vec::new();
        let mut at = 0;
        while at + 27 <= bytes.len() {
            assert_eq!(&bytes[at..at + 4], b"OggS");
            granules.push(u64::from_le_bytes(bytes[at + 6..at + 14].try_into().unwrap()));
            let segments = bytes[at + 26] as usize;
            let body: usize = bytes[at + 27..at + 27 + segments].iter().map(|v| *v as usize).sum();
            at += 27 + segments + body;
        }
        let audio = &granules[2..];
        // A full page is 50 packets of 960 granules.
        assert_eq!(audio[0], 50 * 960);
        assert_eq!(audio[1], 100 * 960);
        // The last one ends at the audio's end after the pre-skip.
        let (_, lookahead) = Encoder::new().unwrap();
        assert_eq!(*audio.last().unwrap(), u64::from(lookahead) * 3 + 40_000 * 3);
    }

    /// Granule and number of lacing values of each page.
    fn pages(bytes: &[u8]) -> Vec<(u64, usize)> {
        let mut pages = Vec::new();
        let mut at = 0;
        while at + 27 <= bytes.len() {
            assert_eq!(&bytes[at..at + 4], b"OggS");
            let granule = u64::from_le_bytes(bytes[at + 6..at + 14].try_into().unwrap());
            let segments = bytes[at + 26] as usize;
            let body: usize = bytes[at + 27..at + 27 + segments].iter().map(|v| *v as usize).sum();
            pages.push((granule, segments));
            at += 27 + segments + body;
        }
        pages
    }

    #[test]
    fn the_last_page_holds_a_packet_when_the_packets_fill_whole_pages() {
        // 49 frames and the one of padding: exactly 50 packets, one full page.
        for frames in [49, 99, 50] {
            let path = test_path(&format!("boundary-{frames}.opus"));
            let mut writer = OggOpusWriter::new(fs::File::create(&path).unwrap(), 3).unwrap();
            writer.write(&tone(400.0, frames as f64 * 0.02)).unwrap();
            writer.finish().unwrap();
            let audio = pages(&fs::read(&path).unwrap())[2..].to_vec();
            let (last_granule, last_packets) = *audio.last().unwrap();
            assert!(last_packets > 0, "{frames} frames: an empty last page");
            assert!(audio.windows(2).all(|pair| pair[0].0 <= pair[1].0), "{frames}: {audio:?}");
            let (_, lookahead) = Encoder::new().unwrap();
            assert_eq!(last_granule, u64::from(lookahead) * 3 + frames as u64 * 320 * 3);
            let decoded = crate::files::decode(&path).unwrap().samples.len();
            assert!(decoded.abs_diff(frames * 320) <= 2, "{frames}: {decoded} samples");
        }
    }

    #[test]
    fn the_end_trims_less_than_one_packet_and_granules_never_go_back() {
        let (_, lookahead) = Encoder::new().unwrap();
        let source = tone(400.0, 1.0);
        for extra in [1, 104, 105, 200, 216, 217, 319] {
            let length = 49 * FRAME + extra;
            let path = test_path(&format!("trim-{extra}.opus"));
            let mut writer = OggOpusWriter::new(fs::File::create(&path).unwrap(), 4).unwrap();
            writer.write(&source[..length]).unwrap();
            writer.finish().unwrap();
            let audio = pages(&fs::read(&path).unwrap())[2..].to_vec();
            assert!(audio.windows(2).all(|pair| pair[0].0 <= pair[1].0), "{extra}: {audio:?}");
            // Packets of this bitrate take one lacing value each.
            let packets: usize = audio.iter().map(|(_, lacing)| lacing).sum();
            let last = audio.last().unwrap().0;
            assert_eq!(last, u64::from(lookahead) * 3 + length as u64 * 3, "{extra}");
            let trimmed = packets as u64 * 960 - last;
            assert!(trimmed < 960, "{extra}: {trimmed} granules trimmed");
            let decoded = crate::files::decode(&path).unwrap().samples.len();
            assert!(decoded.abs_diff(length) <= 2, "{extra}: {decoded} samples");
        }
    }

    #[test]
    fn long_packets_never_overflow_a_page() {
        let packets: Vec<Vec<u8>> = vec![vec![0; 600]; 120];
        assert!(lacing_values(&packets) > 255, "the case the writer splits");
        assert_eq!(lacing_values(&[vec![0; 255], vec![0; 10]]), 3);
    }

    #[test]
    fn pages_carry_the_ogg_checksum() {
        let page = page(1, 2, 0, 3, &[vec![1; 300], vec![2; 10]]);
        assert_eq!(page[26], 3, "lacing values: 255, 45, 10");
        assert_eq!(&page[27..30], &[255, 45, 10]);
        let mut zeroed = page.clone();
        zeroed[22..26].copy_from_slice(&[0; 4]);
        assert_eq!(u32::from_le_bytes(page[22..26].try_into().unwrap()), crc32(&zeroed));
        assert_ne!(crc32(&zeroed), 0);
    }
}
