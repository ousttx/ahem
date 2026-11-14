//! The dictation orchestrator: one thread owns the core state machines, feeds them messages in
//! order and performs their effects through [`Services`]. Slow work (engine start,
//! transcription, insertion) runs elsewhere and comes back as a [`Message`].

mod controller;
pub mod engine_worker;
pub mod file_reader;
pub mod services;

pub use controller::{
    Controller, FileRequest, FileSource, Message, ProcessingError, ProcessingFailure, TrackUpdate,
};
pub use services::{Recording, Services};
