//! Microphone capture with cpal: any device and sample format, mixed to mono in memory.
//!
//! The stream lives on its own thread because cpal streams are not `Send` on every platform.

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, SampleFormat, SizedSample, Stream, StreamConfig};

use super::{resample, wav};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
    pub default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    NoDevice,
    DeviceNotFound(String),
    UnsupportedFormat(String),
    /// The audio system rejected the device or the stream failed while recording.
    Device(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice => write!(f, "no microphone found"),
            Self::DeviceNotFound(id) => write!(f, "microphone {id} is not connected"),
            Self::UnsupportedFormat(format) => write!(f, "unsupported sample format {format}"),
            Self::Device(detail) => write!(f, "microphone error: {detail}"),
        }
    }
}

impl std::error::Error for CaptureError {}

fn device_error(error: impl fmt::Display) -> CaptureError {
    CaptureError::Device(error.to_string())
}

pub fn input_devices() -> Result<Vec<InputDevice>, CaptureError> {
    let host = cpal::default_host();
    let default =
        host.default_input_device().and_then(|device| device.id().ok()).map(|id| id.to_string());
    let devices = host.input_devices().map_err(device_error)?;
    Ok(devices
        .filter_map(|device| {
            let id = device.id().ok()?.to_string();
            Some(InputDevice {
                default: default.as_deref() == Some(id.as_str()),
                name: device.to_string(),
                id,
            })
        })
        .collect())
}

/// Mono audio at the device's sample rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Captured {
    pub samples: Vec<f32>,
    pub rate: u32,
}

impl Captured {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / f64::from(self.rate.max(1)))
    }

    /// 16 kHz mono 16-bit WAV for Whisper.
    pub fn to_wav(&self) -> Vec<u8> {
        wav::encode(&resample::to_16k(&self.samples, self.rate), resample::TARGET_RATE)
    }
}

/// Shared between the audio callback and the recorder.
#[derive(Default)]
pub(crate) struct Sink {
    pub(crate) samples: Mutex<Vec<f32>>,
    /// RMS of the latest callback block, as `f32` bits.
    pub(crate) level: AtomicU32,
    pub(crate) failure: Mutex<Option<String>>,
}

impl Sink {
    pub(crate) fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
}

pub struct Recorder {
    sink: Arc<Sink>,
    rate: u32,
    stop: mpsc::Sender<()>,
    thread: JoinHandle<u32>,
}

impl Recorder {
    /// `device_id` from [`input_devices`]; `None` records from the default microphone.
    pub fn start(device_id: Option<String>) -> Result<Self, CaptureError> {
        let sink = Arc::new(Sink::default());
        let (ready_tx, ready_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let shared = Arc::clone(&sink);
        let thread = thread::spawn(move || match open(device_id.as_deref(), &shared) {
            Ok((stream, rate)) => {
                let _ = ready_tx.send(Ok(rate));
                let _ = stop_rx.recv();
                drop(stream);
                rate
            }
            Err(error) => {
                let _ = ready_tx.send(Err(error));
                0
            }
        });
        match ready_rx.recv() {
            Ok(Ok(rate)) => Ok(Self { sink, rate, stop: stop_tx, thread }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => Err(CaptureError::Device("capture thread ended".into())),
        }
    }

    /// Current input level, 0.0–1.0, for the recording indicator.
    pub fn level(&self) -> f32 {
        self.sink.level()
    }

    /// Why the device stopped delivering, if it did.
    pub fn interrupted(&self) -> Option<String> {
        self.sink.failure.lock().expect("capture lock").clone()
    }

    /// The device's sample rate.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// A copy of the mono samples recorded from position `from` on; recording goes on.
    pub fn samples_since(&self, from: usize) -> Vec<f32> {
        let samples = self.sink.samples.lock().expect("capture lock");
        samples.get(from..).map(<[f32]>::to_vec).unwrap_or_default()
    }

    pub fn stop(self) -> Result<Captured, CaptureError> {
        let _ = self.stop.send(());
        let rate = self
            .thread
            .join()
            .map_err(|_| CaptureError::Device("capture thread panicked".into()))?;
        let failure = self.sink.failure.lock().expect("capture lock").take();
        let samples = std::mem::take(&mut *self.sink.samples.lock().expect("capture lock"));
        kept(failure, Captured { samples, rate })
    }
}

/// What a recording leaves: a device that failed midway (a headset that went away) keeps what
/// was recorded before; only a recording with nothing in it is the device's error.
fn kept(failure: Option<String>, captured: Captured) -> Result<Captured, CaptureError> {
    match failure {
        Some(failure) if captured.samples.is_empty() => Err(CaptureError::Device(failure)),
        Some(failure) => {
            log_line!("the microphone failed during a recording; its start is kept: {failure}");
            Ok(captured)
        }
        None => Ok(captured),
    }
}

fn open(device_id: Option<&str>, sink: &Arc<Sink>) -> Result<(Stream, u32), CaptureError> {
    let host = cpal::default_host();
    let device = match device_id {
        None => host.default_input_device().ok_or(CaptureError::NoDevice)?,
        Some(id) => host
            .input_devices()
            .map_err(device_error)?
            .find(|device| device.id().is_ok_and(|candidate| candidate.to_string() == id))
            .ok_or_else(|| CaptureError::DeviceNotFound(id.to_string()))?,
    };
    open_device(&device, sink)
}

/// Starts recording `device` into `sink` as mono at the device's rate. An output device is
/// recorded through loopback where cpal has it (Windows).
pub(crate) fn open_device(
    device: &cpal::Device,
    sink: &Arc<Sink>,
) -> Result<(Stream, u32), CaptureError> {
    let supported = device
        .default_input_config()
        .or_else(|_| device.default_output_config())
        .map_err(device_error)?;
    let config: StreamConfig = supported.config();
    let rate = config.sample_rate;
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(device, &config, sink),
        SampleFormat::I16 => build::<i16>(device, &config, sink),
        SampleFormat::U16 => build::<u16>(device, &config, sink),
        SampleFormat::I32 => build::<i32>(device, &config, sink),
        SampleFormat::U8 => build::<u8>(device, &config, sink),
        other => return Err(CaptureError::UnsupportedFormat(other.to_string())),
    }?;
    stream.play().map_err(device_error)?;
    Ok((stream, rate))
}

fn build<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    sink: &Arc<Sink>,
) -> Result<Stream, CaptureError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    let data_sink = Arc::clone(sink);
    let error_sink = Arc::clone(sink);
    device
        .build_input_stream(
            *config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let block: Vec<f32> = data.iter().map(|sample| sample.to_sample::<f32>()).collect();
                let mut samples = data_sink.samples.lock().expect("capture lock");
                let level = mix_into(&block, channels, &mut samples);
                data_sink.level.store(level.to_bits(), Ordering::Relaxed);
            },
            move |error| record_failure(&error_sink, &error),
            None,
        )
        .map_err(device_error)
}

/// Keeps the stream's first failure. An overrun or underrun is a glitch the stream goes on
/// after, not a failure: taken for one, a meeting reopened its Windows loopback several times
/// a second while the other side spoke, and kept only scraps of it (2026-11-09).
fn record_failure(sink: &Sink, error: &cpal::Error) {
    if error.kind() != ErrorKind::Xrun {
        sink.failure.lock().expect("capture lock").get_or_insert(error.to_string());
        // Nothing comes any more: the level does not stay at its last value.
        sink.level.store(0f32.to_bits(), Ordering::Relaxed);
    }
}

/// Appends interleaved frames as mono (channel average); returns the block's RMS.
fn mix_into(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) -> f32 {
    let mut energy = 0.0;
    let mut frames = 0;
    for frame in interleaved.chunks(channels) {
        let value = frame.iter().sum::<f32>() / frame.len() as f32;
        energy += value * value;
        frames += 1;
        out.push(value);
    }
    if frames == 0 { 0.0 } else { (energy / frames as f32).sqrt() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_that_fails_midway_keeps_what_was_recorded() {
        let some = || Captured { samples: vec![0.1; 48_000], rate: 48_000 };
        let gone = || Some("device disconnected".to_string());
        assert_eq!(kept(gone(), some()).unwrap().samples.len(), 48_000);
        let nothing = Captured { samples: Vec::new(), rate: 48_000 };
        assert!(matches!(kept(gone(), nothing), Err(CaptureError::Device(_))));
        assert!(kept(None, some()).is_ok());
    }

    #[test]
    fn channels_are_averaged_into_mono() {
        let mut out = Vec::new();
        let level = mix_into(&[0.5, 0.1, -0.2, -0.4], 2, &mut out);
        assert_eq!(out, vec![0.3, -0.3]);
        assert!((level - 0.3).abs() < 1e-6);
    }

    #[test]
    fn mono_passes_through() {
        let mut out = vec![1.0];
        mix_into(&[0.25, -0.25], 1, &mut out);
        assert_eq!(out, vec![1.0, 0.25, -0.25]);
    }

    #[test]
    fn a_glitch_is_not_a_failure() {
        let sink = Sink::default();
        record_failure(&sink, &ErrorKind::Xrun.into());
        assert_eq!(*sink.failure.lock().unwrap(), None);
        record_failure(&sink, &ErrorKind::DeviceNotAvailable.into());
        record_failure(&sink, &ErrorKind::Xrun.into());
        assert_eq!(
            sink.failure.lock().unwrap().as_deref(),
            Some(cpal::Error::from(ErrorKind::DeviceNotAvailable).to_string().as_str())
        );
    }

    #[test]
    fn captured_audio_becomes_16k_wav() {
        let captured = Captured { samples: vec![0.0; 48_000], rate: 48_000 };
        assert_eq!(captured.duration(), Duration::from_secs(1));
        let wav = captured.to_wav();
        assert_eq!(wav.len(), 44 + 32_000);
    }

    /// Records one second from the default microphone; the audio is discarded.
    #[test]
    #[ignore = "uses the real microphone: cargo test -- --ignored records_from"]
    fn records_from_the_default_microphone() {
        println!("{:?}", input_devices().unwrap());
        let recorder = Recorder::start(None).unwrap();
        thread::sleep(Duration::from_secs(1));
        let captured = recorder.stop().unwrap();
        println!("rate {} Hz, {:?}", captured.rate, captured.duration());
        assert!(captured.duration() > Duration::from_millis(800));
    }
}
