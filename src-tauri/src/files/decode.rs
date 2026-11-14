//! Audio and video files to Whisper's input, 16 kHz mono `f32`, with the pure-Rust symphonia
//! and, for Opus, the reference libopus.
//!
//! Containers: WAV, AIFF, MP3, MP4/M4A/MOV (also fragmented), ADTS `.aac`, FLAC, Ogg, MKV/WebM.
//! Codecs: PCM (integer, float, A-law, µ-law), IMA/MS ADPCM, MP3, AAC-LC (mono, stereo), ALAC,
//! FLAC, Vorbis, Opus (mono, stereo). Multichannel Opus and AAC, HE-AAC with explicit
//! signalling, AC-3/E-AC-3, DTS, WMA and others are demuxed but not decoded:
//! `UnsupportedCodec` names them.
//!
//! The first audio track is decoded packet by packet, mixed to mono (channel average) and
//! resampled in pieces of a few seconds, so besides the 16 kHz result (about 230 MB per hour as
//! `f32`) only a few seconds of input are in memory. [`FileDecoder`] hands the result out block
//! by block (with [`super::chunks::Planner`] a file need not be in memory at all); [`decode`]
//! collects it.

use std::fmt;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use symphonia::core::codecs::audio::well_known::{
    CODEC_ID_AAC, CODEC_ID_AC3, CODEC_ID_AC4, CODEC_ID_DCA, CODEC_ID_EAC3, CODEC_ID_MONKEYS_AUDIO,
    CODEC_ID_MP1, CODEC_ID_MP2, CODEC_ID_SPEEX, CODEC_ID_TRUEHD, CODEC_ID_WAVPACK, CODEC_ID_WMA,
};
use symphonia::core::codecs::audio::{AudioCodecId, AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as MediaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, Track, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia_adapter_libopus::OpusDecoder;

use crate::audio::resample::{Resampler, TARGET_RATE};

const RATE: usize = TARGET_RATE as usize;
/// [`FileDecoder::next_block`] returns at least this much audio, except at the end.
const BLOCK: usize = 4 * RATE;
/// Memory is reserved up front only for durations a container can plausibly claim.
const LONGEST_RESERVATION: Duration = Duration::from_secs(12 * 3600);

/// symphonia's codecs enabled in `Cargo.toml`, plus Opus.
static CODECS: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut codecs = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut codecs);
    codecs.register_audio_decoder::<OpusDecoder>();
    codecs
});

/// A whole file as Whisper's input.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// 16 kHz mono.
    pub samples: Vec<f32>,
    pub duration: Duration,
}

#[derive(Debug)]
pub enum DecodeError {
    /// The file cannot be opened or read.
    Io(io::Error),
    /// Not a container we can read (or not a media file).
    UnsupportedFormat,
    /// No audio track, e.g. a video without sound.
    NoAudioTrack,
    /// The first audio track's codec (short name such as `opus`) cannot be decoded.
    UnsupportedCodec { codec: String, detail: String },
    /// Damaged beyond the packets that could be skipped.
    Malformed(String),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "cannot read the file: {error}"),
            Self::UnsupportedFormat => write!(f, "not a supported audio or video file"),
            Self::NoAudioTrack => write!(f, "the file has no audio track"),
            Self::UnsupportedCodec { codec, detail } => {
                write!(f, "unsupported audio codec {codec} ({detail})")
            }
            Self::Malformed(detail) => write!(f, "damaged file: {detail}"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn malformed(error: MediaError) -> DecodeError {
    DecodeError::Malformed(error.to_string())
}

/// Decodes the whole file into memory.
pub fn decode(path: &Path) -> Result<Decoded, DecodeError> {
    let mut decoder = FileDecoder::open(path)?;
    let mut samples = Vec::new();
    if let Some(expected) = decoder.duration_hint().filter(|hint| *hint <= LONGEST_RESERVATION) {
        // One allocation: a growing vector briefly needs up to three times its size while it
        // moves. Containers estimate (MP3 without a header), hence the margin.
        let length = expected.as_secs_f64() * 1.05 * RATE as f64;
        let _ = samples.try_reserve_exact(length as usize + 30 * RATE);
    }
    while let Some(block) = decoder.next_block()? {
        samples.extend_from_slice(&block);
    }
    let duration = Duration::from_secs_f64(samples.len() as f64 / RATE as f64);
    Ok(Decoded { samples, duration })
}

struct AudioTrack {
    id: u32,
    decoder: Box<dyn AudioDecoder>,
    codec: String,
}

/// The first audio track of a file, decoded on demand.
pub struct FileDecoder {
    format: Box<dyn FormatReader>,
    track: AudioTrack,
    duration_hint: Option<Duration>,
    /// The input rate and the resampler for it.
    resampler: Option<(u32, Resampler)>,
    interleaved: Vec<f32>,
    output: Vec<f32>,
    decoded_packets: u64,
    failed_packets: u64,
    ended: bool,
}

impl FileDecoder {
    pub fn open(path: &Path) -> Result<Self, DecodeError> {
        let file = File::open(path).map_err(DecodeError::Io)?;
        let source = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
            hint.with_extension(extension);
        }
        let format = symphonia::default::get_probe()
            .probe(&hint, source, FormatOptions::default(), MetadataOptions::default())
            .map_err(|error| match error {
                MediaError::Unsupported(_) => DecodeError::UnsupportedFormat,
                MediaError::IoError(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    DecodeError::UnsupportedFormat
                }
                MediaError::IoError(error) => DecodeError::Io(error),
                other => malformed(other),
            })?;
        let (track, duration_hint) = open_track(format.as_ref())?;
        Ok(Self {
            format,
            track,
            duration_hint,
            resampler: None,
            interleaved: Vec::new(),
            output: Vec::new(),
            decoded_packets: 0,
            failed_packets: 0,
            ended: false,
        })
    }

    /// Duration stated by the container, for progress before decoding ends; may be missing or
    /// an estimate.
    pub fn duration_hint(&self) -> Option<Duration> {
        self.duration_hint
    }

    /// The next piece of 16 kHz mono audio (a few seconds); `None` once the file has ended.
    pub fn next_block(&mut self) -> Result<Option<Vec<f32>>, DecodeError> {
        while self.output.len() < BLOCK && !self.ended {
            self.read_packet()?;
        }
        if self.output.is_empty() {
            return Ok(None);
        }
        Ok(Some(std::mem::take(&mut self.output)))
    }

    fn read_packet(&mut self) -> Result<(), DecodeError> {
        let packet = match self.format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => return self.end(),
            // A file cut short ends where its data ends.
            Err(MediaError::IoError(error)) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return self.end();
            }
            // A new track list (chained Ogg streams): go on with its first audio track.
            Err(MediaError::ResetRequired) => {
                self.flush_resampler();
                self.track = open_track(self.format.as_ref())?.0;
                return Ok(());
            }
            Err(MediaError::IoError(error)) => return Err(DecodeError::Io(error)),
            Err(error) => return Err(malformed(error)),
        };
        if packet.track_id != self.track.id {
            return Ok(());
        }
        let (rate, channels) = match self.track.decoder.decode(&packet) {
            Ok(buffer) => {
                self.decoded_packets += 1;
                buffer.copy_to_vec_interleaved(&mut self.interleaved);
                (buffer.spec().rate(), buffer.num_planes().max(1))
            }
            // A damaged packet is skipped, as players do.
            Err(
                MediaError::DecodeError(_) | MediaError::IoError(_) | MediaError::ResetRequired,
            ) => {
                self.failed_packets += 1;
                return Ok(());
            }
            Err(MediaError::Unsupported(detail)) => {
                let codec = self.track.codec.clone();
                return Err(DecodeError::UnsupportedCodec { codec, detail: detail.into() });
            }
            Err(error) => return Err(malformed(error)),
        };
        if self.interleaved.is_empty() {
            return Ok(());
        }
        if rate == 0 {
            return Err(DecodeError::Malformed("no sample rate".into()));
        }
        if self.resampler.as_ref().is_none_or(|(at, _)| *at != rate) {
            self.flush_resampler();
            self.resampler = Some((rate, Resampler::new(rate)));
        }
        let mono: Vec<f32> = self
            .interleaved
            .chunks_exact(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect();
        if let Some((_, resampler)) = &mut self.resampler {
            self.output.extend(resampler.push(&mono));
        }
        Ok(())
    }

    fn end(&mut self) -> Result<(), DecodeError> {
        self.ended = true;
        self.flush_resampler();
        if self.decoded_packets == 0 && self.failed_packets > 0 {
            let detail = format!("none of {} audio packets could be decoded", self.failed_packets);
            return Err(DecodeError::Malformed(detail));
        }
        Ok(())
    }

    fn flush_resampler(&mut self) {
        if let Some((_, mut resampler)) = self.resampler.take() {
            self.output.extend(resampler.finish());
        }
    }
}

fn open_track(format: &dyn FormatReader) -> Result<(AudioTrack, Option<Duration>), DecodeError> {
    let track = format.first_track(TrackType::Audio).ok_or(DecodeError::NoAudioTrack)?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or(DecodeError::NoAudioTrack)?;
    let codec = codec_name(params.codec);
    let decoder =
        CODECS.make_audio_decoder(params, &AudioDecoderOptions::default()).map_err(|error| {
            match error {
                MediaError::Unsupported(detail) => {
                    DecodeError::UnsupportedCodec { codec: codec.clone(), detail: detail.into() }
                }
                other => malformed(other),
            }
        })?;
    let duration = track_duration(track, params.sample_rate);
    Ok((AudioTrack { id: track.id, decoder, codec }, duration))
}

fn track_duration(track: &Track, rate: Option<u32>) -> Option<Duration> {
    let seconds = match (track.time_base, track.duration) {
        (Some(base), Some(duration)) => base.calc_duration(duration)?.as_secs_f64(),
        _ => track.num_frames? as f64 / f64::from(rate.filter(|rate| *rate > 0)?),
    };
    Duration::try_from_secs_f64(seconds).ok()
}

/// Short codec name for messages: the decoder's own name, or a known codec we cannot decode.
fn codec_name(id: AudioCodecId) -> String {
    if let Some(registered) = CODECS.get_audio_decoder(id) {
        return registered.codec.info.short_name.to_string();
    }
    let name = match id {
        CODEC_ID_AAC => "aac",
        CODEC_ID_AC3 => "ac3",
        CODEC_ID_EAC3 => "eac3",
        CODEC_ID_AC4 => "ac4",
        CODEC_ID_DCA => "dts",
        CODEC_ID_TRUEHD => "truehd",
        CODEC_ID_WMA => "wma",
        CODEC_ID_SPEEX => "speex",
        CODEC_ID_MP1 => "mp1",
        CODEC_ID_MP2 => "mp2",
        CODEC_ID_WAVPACK => "wavpack",
        CODEC_ID_MONKEYS_AUDIO => "ape",
        other => return format!("codec {other}"),
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    fn artifacts() -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts/files");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(name: &str, bytes: &[u8]) -> PathBuf {
        let path = artifacts().join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn tone(frequency: f32, rate: u32, seconds: f32, amplitude: f32) -> Vec<f32> {
        (0..(rate as f32 * seconds) as usize)
            .map(|i| amplitude * (2.0 * PI * frequency * i as f32 / rate as f32).sin())
            .collect()
    }

    /// Deterministic noise in [-0.5, 0.5).
    fn noise(length: usize) -> Vec<f32> {
        let mut state: u32 = 987_654_321;
        (0..length)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect()
    }

    fn interleave(channels: &[Vec<f32>]) -> Vec<f32> {
        (0..channels[0].len()).flat_map(|i| channels.iter().map(move |c| c[i])).collect()
    }

    fn quantize(sample: f32) -> i16 {
        (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16
    }

    /// What the decoder sees after 16-bit quantization, mixed to mono.
    fn quantized_mono(channels: &[Vec<f32>]) -> Vec<f32> {
        (0..channels[0].len())
            .map(|i| {
                let sum: f32 = channels.iter().map(|c| f32::from(quantize(c[i])) / 32_768.0).sum();
                sum / channels.len() as f32
            })
            .collect()
    }

    fn wav(rate: u32, channels: u16, float: bool, interleaved: &[f32]) -> Vec<u8> {
        let width: u16 = if float { 4 } else { 2 };
        let data_len = (interleaved.len() * usize::from(width)) as u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&(if float { 3u16 } else { 1 }).to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * u32::from(channels * width)).to_le_bytes());
        wav.extend_from_slice(&(channels * width).to_le_bytes());
        wav.extend_from_slice(&(width * 8).to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for sample in interleaved {
            if float {
                wav.extend_from_slice(&sample.to_le_bytes());
            } else {
                wav.extend_from_slice(&quantize(*sample).to_le_bytes());
            }
        }
        wav
    }

    fn crc8(data: &[u8]) -> u8 {
        data.iter().fold(0, |mut crc, byte| {
            crc ^= byte;
            for _ in 0..8 {
                crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
            }
            crc
        })
    }

    fn crc16(data: &[u8]) -> u16 {
        data.iter().fold(0, |mut crc, byte| {
            crc ^= u16::from(*byte) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
            }
            crc
        })
    }

    /// 16-bit FLAC with uncompressed (verbatim) subframes.
    fn flac(rate: u32, channels: &[Vec<f32>]) -> Vec<u8> {
        const BLOCK_SIZE: usize = 4096;
        let frames = channels[0].len();
        let count = channels.len() as u64;
        let mut flac = b"fLaC".to_vec();
        flac.extend_from_slice(&[0x80, 0, 0, 34]); // last metadata block: STREAMINFO, 34 bytes
        flac.extend_from_slice(&(BLOCK_SIZE as u16).to_be_bytes());
        flac.extend_from_slice(&(BLOCK_SIZE as u16).to_be_bytes());
        flac.extend_from_slice(&[0; 6]); // frame sizes unknown
        let packed =
            (u64::from(rate) << 44) | ((count - 1) << 41) | ((16 - 1) << 36) | frames as u64;
        flac.extend_from_slice(&packed.to_be_bytes());
        flac.extend_from_slice(&[0; 16]); // MD5 unknown
        for (number, start) in (0..frames).step_by(BLOCK_SIZE).enumerate() {
            let size = BLOCK_SIZE.min(frames - start);
            let mut frame = vec![0xFF, 0xF8, 0x70, ((count as u8 - 1) << 4) | 0x08];
            let mut utf8 = [0; 4];
            let number = char::from_u32(number as u32).unwrap().encode_utf8(&mut utf8);
            frame.extend_from_slice(number.as_bytes());
            frame.extend_from_slice(&(size as u16 - 1).to_be_bytes());
            frame.push(crc8(&frame));
            for channel in channels {
                frame.push(0x02); // verbatim
                for sample in &channel[start..start + size] {
                    frame.extend_from_slice(&quantize(*sample).to_be_bytes());
                }
            }
            let crc = crc16(&frame);
            frame.extend_from_slice(&crc.to_be_bytes());
            flac.extend_from_slice(&frame);
        }
        flac
    }

    fn ogg_page(header_type: u8, granule: u64, sequence: u32, packet: &[u8]) -> Vec<u8> {
        assert!(packet.len() < 255);
        let mut page = b"OggS".to_vec();
        page.extend_from_slice(&[0, header_type]);
        page.extend_from_slice(&granule.to_le_bytes());
        page.extend_from_slice(&0x5654_4D21u32.to_le_bytes()); // serial
        page.extend_from_slice(&sequence.to_le_bytes());
        page.extend_from_slice(&[0; 4]); // CRC, filled below
        page.extend_from_slice(&[1, packet.len() as u8]);
        page.extend_from_slice(packet);
        let crc = page.iter().fold(0u32, |mut crc, byte| {
            crc ^= u32::from(*byte) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
            }
            crc
        });
        page[22..26].copy_from_slice(&crc.to_le_bytes());
        page
    }

    /// Ogg Opus: the headers, then one page per 20 ms packet.
    fn ogg_opus(channels: u8, pre_skip: u16, packets: &[Vec<u8>]) -> Vec<u8> {
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, channels]); // version, channels
        head.extend_from_slice(&pre_skip.to_le_bytes());
        head.extend_from_slice(&48_000u32.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes()); // gain
        if channels <= 2 {
            head.push(0); // mapping family 0: mono or stereo
        } else {
            // Family 1: one uncoupled stream per channel.
            head.extend_from_slice(&[1, channels, 0]);
            head.extend(0..channels);
        }
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&4u32.to_le_bytes());
        tags.extend_from_slice(b"test");
        tags.extend_from_slice(&0u32.to_le_bytes());
        let mut ogg = ogg_page(0x02, 0, 0, &head);
        ogg.extend(ogg_page(0, 0, 1, &tags));
        for (number, packet) in packets.iter().enumerate() {
            let header_type = if number + 1 == packets.len() { 0x04 } else { 0 };
            let granule = u64::from(pre_skip) + 960 * (number as u64 + 1);
            ogg.extend(ogg_page(header_type, granule, number as u32 + 2, packet));
        }
        ogg
    }

    /// 48 kHz mono audio as 20 ms Opus packets from libopus, with the encoder's pre-skip.
    fn opus_packets(samples: &[f32]) -> (u16, Vec<Vec<u8>>) {
        use opusic_sys::{
            OPUS_APPLICATION_AUDIO, OPUS_GET_LOOKAHEAD_REQUEST, OPUS_OK, OPUS_SET_BITRATE_REQUEST,
            opus_encode_float, opus_encoder_create, opus_encoder_ctl, opus_encoder_destroy,
        };
        const FRAME: usize = 960;
        // SAFETY: the encoder is checked after creation and destroyed once at the end; every
        // buffer outlives the call that gets its pointer and has the length passed with it.
        unsafe {
            let mut error = 0;
            let encoder = opus_encoder_create(48_000, 1, OPUS_APPLICATION_AUDIO, &mut error);
            assert_eq!(error, OPUS_OK);
            // 24 kbit/s is about 60 bytes per packet: one page each, see `ogg_page`.
            assert_eq!(opus_encoder_ctl(encoder, OPUS_SET_BITRATE_REQUEST, 24_000), OPUS_OK);
            let mut lookahead: i32 = 0;
            let request = opus_encoder_ctl(encoder, OPUS_GET_LOOKAHEAD_REQUEST, &mut lookahead);
            assert_eq!(request, OPUS_OK);
            let mut packets = Vec::new();
            for frame in samples.chunks(FRAME) {
                let mut pcm = frame.to_vec();
                pcm.resize(FRAME, 0.0);
                let mut packet = vec![0; 254];
                let length = opus_encode_float(
                    encoder,
                    pcm.as_ptr(),
                    FRAME as i32,
                    packet.as_mut_ptr(),
                    packet.len() as i32,
                );
                assert!(length > 0, "opus_encode_float: {length}");
                packet.truncate(length as usize);
                packets.push(packet);
            }
            opus_encoder_destroy(encoder);
            (lookahead as u16, packets)
        }
    }

    fn assert_close(actual: &[f32], expected: &[f32], tolerance: f32) {
        assert_eq!(actual.len(), expected.len(), "length");
        let worst = actual.iter().zip(expected).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(worst <= tolerance, "max difference {worst}");
    }

    fn zero_crossings(samples: &[f32]) -> usize {
        samples.windows(2).filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0)).count()
    }

    #[test]
    fn stereo_44k_wav_becomes_16k_mono() {
        let left = tone(440.0, 44_100, 3.0, 0.5);
        let right = vec![0.0; left.len()];
        let channels = [left, right];
        let path = write("stereo-44k.wav", &wav(44_100, 2, false, &interleave(&channels)));

        let decoded = decode(&path).unwrap();
        assert_close(
            &decoded.samples,
            &crate::audio::resample::to_16k(&quantized_mono(&channels), 44_100),
            1e-5,
        );
        // `to_16k` floors the output length in floating point: 3 s may come out one sample short.
        assert!(decoded.samples.len().abs_diff(48_000) <= 1, "{}", decoded.samples.len());
        assert!(decoded.duration.abs_diff(Duration::from_secs(3)) <= Duration::from_millis(1));
        let crossings = zero_crossings(&decoded.samples);
        assert!((2_635..=2_645).contains(&crossings), "{crossings} zero crossings");
        let peak = decoded.samples[1_000..47_000].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.25).abs() < 0.01, "averaged with a silent channel: peak {peak}");
    }

    #[test]
    fn float_48k_wav_is_resampled() {
        let samples = tone(1_000.0, 48_000, 2.5, 0.8);
        let path = write("mono-48k-float.wav", &wav(48_000, 1, true, &samples));
        let decoded = decode(&path).unwrap();
        assert_close(&decoded.samples, &crate::audio::resample::to_16k(&samples, 48_000), 1e-5);
        assert_eq!(decoded.duration, Duration::from_secs_f64(2.5));
    }

    #[test]
    fn wav_at_16k_passes_through() {
        let samples = noise(16_000 * 5 + 123);
        let path = write("mono-16k.wav", &wav(16_000, 1, false, &samples));
        let mut decoder = FileDecoder::open(&path).unwrap();
        let hint = decoder.duration_hint().unwrap().as_secs_f64();
        assert!((hint - 80_123.0 / 16_000.0).abs() < 1e-6, "{hint}");
        let mut decoded = Vec::new();
        while let Some(block) = decoder.next_block().unwrap() {
            assert!(!block.is_empty());
            decoded.extend(block);
        }
        assert!(decoder.next_block().unwrap().is_none());
        assert_close(&decoded, &quantized_mono(&[samples]), 1e-7);
    }

    #[test]
    fn wav_at_8k_is_upsampled() {
        let samples = tone(300.0, 8_000, 2.0, 0.5);
        let path = write("mono-8k.wav", &wav(8_000, 1, false, &samples));
        let decoded = decode(&path).unwrap();
        assert_eq!(decoded.samples.len(), 32_000);
        assert_close(
            &decoded.samples,
            &crate::audio::resample::to_16k(&quantized_mono(&[samples]), 8_000),
            1e-5,
        );
    }

    #[test]
    fn empty_wav_has_no_audio() {
        let path = write("empty.wav", &wav(16_000, 1, false, &[]));
        let decoded = decode(&path).unwrap();
        assert!(decoded.samples.is_empty());
        assert_eq!(decoded.duration, Duration::ZERO);
    }

    #[test]
    fn flac_is_decoded() {
        let left = tone(523.0, 44_100, 2.0, 0.4);
        let right = tone(220.0, 44_100, 2.0, 0.3);
        let channels = [left, right];
        let path = write("stereo-44k.flac", &flac(44_100, &channels));
        let decoded = decode(&path).unwrap();
        assert_close(
            &decoded.samples,
            &crate::audio::resample::to_16k(&quantized_mono(&channels), 44_100),
            1e-5,
        );
    }

    #[test]
    fn opus_is_decoded() {
        // Opus is lossy: length, pitch and level are checked, not samples.
        let (pre_skip, packets) = opus_packets(&tone(440.0, 48_000, 1.0, 0.5));
        let path = write("voice.opus", &ogg_opus(1, pre_skip, &packets));
        let decoded = decode(&path).unwrap();
        let length = decoded.samples.len();
        assert!(length.abs_diff(16_000) <= 320, "{length} samples");
        let middle = &decoded.samples[4_000..12_000];
        let crossings = zero_crossings(middle);
        assert!((435..=445).contains(&crossings), "{crossings} zero crossings");
        let peak = middle.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.1, "peak {peak}");
    }

    #[test]
    fn multichannel_opus_is_reported_as_unsupported() {
        let path = write("surround.opus", &ogg_opus(6, 312, &[vec![0xF8, 0xFF, 0xFE]]));
        match decode(&path) {
            Err(DecodeError::UnsupportedCodec { codec, .. }) => assert_eq!(codec, "opus"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn other_files_are_rejected() {
        let path = write("notes.mp3", b"This is a text file, not audio at all.\n");
        assert!(matches!(decode(&path), Err(DecodeError::UnsupportedFormat)));
        let missing = artifacts().join("missing.wav");
        assert!(matches!(decode(&missing), Err(DecodeError::Io(_))));
    }

    /// Synthesized speech from `runtimes/test-audio`; `AHEM_TEST_AUDIO_DIR` points elsewhere.
    #[test]
    #[ignore = "needs runtimes/test-audio: cargo test -- --ignored decodes_test_speech"]
    fn decodes_test_speech() {
        let dir = std::env::var_os("AHEM_TEST_AUDIO_DIR").map(PathBuf::from).unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtimes/test-audio")
        });
        let decoded = decode(&dir.join("en-long.wav")).unwrap();
        let windows = crate::files::chunks::plan(&decoded.samples);
        println!("{:?}, {} windows", decoded.duration, windows.len());
        assert!(decoded.duration > Duration::from_secs(20));
        assert_eq!(windows.last().unwrap().end, decoded.samples.len());
    }

    /// Prints what happens to every file in `AHEM_MEDIA_DIR`: a check for real-world files.
    #[test]
    #[ignore = "set AHEM_MEDIA_DIR, then cargo test -- --ignored decodes_sample_media --nocapture"]
    fn decodes_sample_media() {
        let dir = PathBuf::from(std::env::var_os("AHEM_MEDIA_DIR").expect("AHEM_MEDIA_DIR"));
        let mut paths: Vec<PathBuf> =
            fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().path()).collect();
        paths.sort();
        for path in paths {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let hint = FileDecoder::open(&path).map(|decoder| decoder.duration_hint());
            match decode(&path) {
                Ok(decoded) => {
                    let peak = decoded.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                    println!(
                        "{name}: {:?} (container: {hint:?}), peak {peak:.2}",
                        decoded.duration
                    );
                }
                Err(error) => println!("{name}: {error}"),
            }
        }
    }
}
