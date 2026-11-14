//! Audio: microphone capture, conversion to Whisper's 16 kHz mono WAV, output muting and format.
//! Dictation audio stays in memory; only meetings are written to disk, as Ogg Opus.

pub mod capture;
pub mod mute;
pub mod ogg_opus;
pub mod output;
pub mod resample;
pub mod speech;
pub mod wav;
