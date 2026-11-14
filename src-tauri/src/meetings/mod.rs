//! Meetings: a call recorded as two tracks — the microphone ("me") and the
//! computer's sound ("others") — kept on disk until the user deletes it, transcribed by speaker.

pub mod echo;
pub mod mix;
pub mod service;
pub mod speakers;
pub mod store;
pub mod timeline;
pub mod track;
pub mod transcript;
