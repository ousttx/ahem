//! Platform-independent core: state machines, scheduler and typed events.
//!
//! Named `domain` because `core` is Rust's built-in crate. No OS-specific code here:
//! machines take inputs and return events and effects; the shell performs the effects.

use std::fmt;

pub mod app_rules;
pub mod clean_text;
pub mod context;
pub mod engine;
pub mod events;
pub mod gesture;
pub mod modes;
pub mod replacements;
pub mod scheduler;
pub mod services;
pub mod session;

/// An input the current state does not accept. The state is left unchanged.
///
/// Holds only names, never payloads, so dictation text cannot leak into logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTransition {
    pub machine: &'static str,
    pub state: &'static str,
    pub input: &'static str,
}

impl InvalidTransition {
    fn new(machine: &'static str, state: &'static str, input: &'static str) -> Self {
        Self { machine, state, input }
    }
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: input `{}` is not valid in state `{}`", self.machine, self.input, self.state)
    }
}

impl std::error::Error for InvalidTransition {}
