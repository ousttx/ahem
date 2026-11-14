//! Arbiter for the single speech engine: one job at a time, dictation first.
//!
//! A file transcription pauses between chunks while a dictation records or waits for
//! recognition; the running chunk is never interrupted and the file is never cancelled
//! because of dictation.

use std::collections::VecDeque;

use serde::Serialize;

use super::InvalidTransition;
use super::session::SessionId;

const MACHINE: &str = "scheduler";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct FileTaskId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Job {
    Dictation { session: SessionId },
    FileChunk { file: FileTaskId, chunk: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerInput {
    EngineReady,
    EngineUnavailable,
    /// The engine runs a configuration no longer chosen and only finishes the stream open at
    /// it: only that session's dictation may go to it.
    EngineFinishing(SessionId),
    /// A dictation started recording: file chunks wait until it ends.
    RecordingStarted,
    RecordingEnded,
    EnqueueDictation(SessionId),
    /// A failed dictation recognized again: after the dictations and, like file chunks, only
    /// while nothing records, so a long retry does not hold up a new dictation (a retry already
    /// running is not stopped).
    EnqueueRetry(SessionId),
    EnqueueFileChunk {
        file: FileTaskId,
        chunk: u32,
    },
    /// Drops a queued dictation job of a session that has ended.
    CancelDictation(SessionId),
    /// Drops the queued chunks of a file task.
    CancelFile(FileTaskId),
    JobFinished,
}

#[derive(Debug, Default)]
pub struct Scheduler {
    engine_ready: bool,
    finishing: Option<SessionId>,
    recordings: u32,
    dictations: VecDeque<SessionId>,
    retries: VecDeque<SessionId>,
    file_chunks: VecDeque<(FileTaskId, u32)>,
    running: Option<Job>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn running(&self) -> Option<Job> {
        self.running
    }

    /// A job is at the engine.
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// File work is held back for a dictation.
    #[cfg(test)]
    pub fn files_paused(&self) -> bool {
        self.recordings > 0
            || !self.dictations.is_empty()
            || !self.retries.is_empty()
            || matches!(self.running, Some(Job::Dictation { .. }))
    }

    /// Returns the job to start now, if any.
    pub fn handle(&mut self, input: SchedulerInput) -> Result<Option<Job>, InvalidTransition> {
        use SchedulerInput as I;

        match input {
            I::EngineReady => (self.engine_ready, self.finishing) = (true, None),
            I::EngineUnavailable => (self.engine_ready, self.finishing) = (false, None),
            I::EngineFinishing(session) => {
                (self.engine_ready, self.finishing) = (false, Some(session))
            }
            I::RecordingStarted => self.recordings += 1,
            I::RecordingEnded => {
                if self.recordings == 0 {
                    return Err(self.invalid("recording_ended"));
                }
                self.recordings -= 1;
            }
            I::EnqueueDictation(session) => self.dictations.push_back(session),
            I::EnqueueRetry(session) => self.retries.push_back(session),
            I::EnqueueFileChunk { file, chunk } => self.file_chunks.push_back((file, chunk)),
            I::CancelDictation(session) => {
                self.dictations.retain(|queued| *queued != session);
                self.retries.retain(|queued| *queued != session);
            }
            I::CancelFile(file) => self.file_chunks.retain(|(queued, _)| *queued != file),
            I::JobFinished => {
                if self.running.take().is_none() {
                    return Err(self.invalid("job_finished"));
                }
            }
        }
        Ok(self.dispatch())
    }

    fn dispatch(&mut self) -> Option<Job> {
        if self.running.is_some() {
            return None;
        }
        if !self.engine_ready {
            let session = self.finishing?;
            let queued = self.dictations.iter().position(|queued| *queued == session)?;
            self.dictations.remove(queued);
            let job = Job::Dictation { session };
            self.running = Some(job);
            return Some(job);
        }
        let job = if let Some(session) = self.dictations.pop_front() {
            Job::Dictation { session }
        } else if self.recordings > 0 {
            return None;
        } else if let Some(session) = self.retries.pop_front() {
            Job::Dictation { session }
        } else {
            let (file, chunk) = self.file_chunks.pop_front()?;
            Job::FileChunk { file, chunk }
        };
        self.running = Some(job);
        Some(job)
    }

    fn invalid(&self, input: &'static str) -> InvalidTransition {
        let state = if self.running.is_some() { "running" } else { "idle" };
        InvalidTransition::new(MACHINE, state, input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SchedulerInput as I;

    const FILE: FileTaskId = FileTaskId(9);

    fn run(scheduler: &mut Scheduler, input: SchedulerInput) -> Option<Job> {
        scheduler.handle(input).expect("valid input")
    }

    fn dictation(id: u64) -> Job {
        Job::Dictation { session: SessionId(id) }
    }

    fn chunk(chunk: u32) -> Job {
        Job::FileChunk { file: FILE, chunk }
    }

    fn ready() -> Scheduler {
        let mut scheduler = Scheduler::new();
        run(&mut scheduler, I::EngineReady);
        scheduler
    }

    #[test]
    fn a_retry_waits_while_a_dictation_records() {
        let mut scheduler = ready();
        run(&mut scheduler, I::RecordingStarted);
        assert_eq!(run(&mut scheduler, I::EnqueueRetry(SessionId(5))), None);
        assert_eq!(run(&mut scheduler, I::RecordingEnded), Some(dictation(5)));
    }

    #[test]
    fn a_retry_waits_for_dictations_and_files_wait_for_it() {
        let mut scheduler = ready();
        run(&mut scheduler, I::EnqueueFileChunk { file: FILE, chunk: 0 });
        assert_eq!(scheduler.running(), Some(chunk(0)));
        run(&mut scheduler, I::EnqueueRetry(SessionId(5)));
        run(&mut scheduler, I::EnqueueDictation(SessionId(6)));
        run(&mut scheduler, I::EnqueueFileChunk { file: FILE, chunk: 1 });
        assert_eq!(run(&mut scheduler, I::JobFinished), Some(dictation(6)));
        assert_eq!(run(&mut scheduler, I::JobFinished), Some(dictation(5)));
        assert_eq!(run(&mut scheduler, I::JobFinished), Some(chunk(1)));
    }

    #[test]
    fn dictation_waits_for_the_engine() {
        let mut scheduler = Scheduler::new();
        assert_eq!(run(&mut scheduler, I::EnqueueDictation(SessionId(1))), None);
        assert_eq!(run(&mut scheduler, I::EngineReady), Some(dictation(1)));
    }

    #[test]
    fn dictation_goes_before_file_chunks() {
        let mut scheduler = Scheduler::new();
        run(&mut scheduler, I::EnqueueFileChunk { file: FILE, chunk: 0 });
        run(&mut scheduler, I::EnqueueDictation(SessionId(1)));
        assert_eq!(run(&mut scheduler, I::EngineReady), Some(dictation(1)));
        assert_eq!(run(&mut scheduler, I::JobFinished), Some(chunk(0)));
    }

    #[test]
    fn file_chunks_wait_while_recording() {
        let mut scheduler = ready();
        run(&mut scheduler, I::RecordingStarted);
        assert!(scheduler.files_paused());
        assert_eq!(run(&mut scheduler, I::EnqueueFileChunk { file: FILE, chunk: 0 }), None);
        assert_eq!(run(&mut scheduler, I::RecordingEnded), Some(chunk(0)));
        assert!(!scheduler.files_paused());
    }

    #[test]
    fn running_chunk_is_not_interrupted() {
        let mut scheduler = ready();
        assert_eq!(
            run(&mut scheduler, I::EnqueueFileChunk { file: FILE, chunk: 0 }),
            Some(chunk(0))
        );
        run(&mut scheduler, I::RecordingStarted);
        run(&mut scheduler, I::RecordingEnded);
        assert_eq!(run(&mut scheduler, I::EnqueueDictation(SessionId(2))), None);
        assert!(scheduler.files_paused());
        assert_eq!(scheduler.running(), Some(chunk(0)));
        assert_eq!(run(&mut scheduler, I::JobFinished), Some(dictation(2)));
    }

    #[test]
    fn one_job_at_a_time() {
        let mut scheduler = ready();
        assert_eq!(run(&mut scheduler, I::EnqueueDictation(SessionId(1))), Some(dictation(1)));
        assert_eq!(run(&mut scheduler, I::EnqueueDictation(SessionId(2))), None);
        assert_eq!(run(&mut scheduler, I::JobFinished), Some(dictation(2)));
        assert_eq!(run(&mut scheduler, I::JobFinished), None);
    }

    #[test]
    fn cancelled_work_is_dropped() {
        let mut scheduler = Scheduler::new();
        run(&mut scheduler, I::EnqueueDictation(SessionId(1)));
        run(&mut scheduler, I::EnqueueFileChunk { file: FILE, chunk: 0 });
        run(&mut scheduler, I::EnqueueFileChunk { file: FILE, chunk: 1 });
        run(&mut scheduler, I::CancelDictation(SessionId(1)));
        run(&mut scheduler, I::CancelFile(FILE));
        assert_eq!(run(&mut scheduler, I::EngineReady), None);
    }

    #[test]
    fn unavailable_engine_stops_dispatch() {
        let mut scheduler = ready();
        run(&mut scheduler, I::EngineUnavailable);
        assert_eq!(run(&mut scheduler, I::EnqueueDictation(SessionId(1))), None);
    }

    /// An engine that only finishes a stream takes that session's dictation and nothing else;
    /// the rest waits for the engine chosen next.
    #[test]
    fn a_finishing_engine_takes_only_its_stream() {
        let mut scheduler = ready();
        run(&mut scheduler, I::RecordingStarted);
        run(&mut scheduler, I::EngineFinishing(SessionId(1)));
        run(&mut scheduler, I::EnqueueFileChunk { file: FILE, chunk: 0 });
        run(&mut scheduler, I::RecordingEnded);
        assert_eq!(run(&mut scheduler, I::EnqueueDictation(SessionId(2))), None);
        assert_eq!(run(&mut scheduler, I::EnqueueDictation(SessionId(1))), Some(dictation(1)));
        assert_eq!(run(&mut scheduler, I::JobFinished), None);
        assert_eq!(run(&mut scheduler, I::EngineReady), Some(dictation(2)));
        assert_eq!(run(&mut scheduler, I::JobFinished), Some(chunk(0)));
    }

    #[test]
    fn invalid_inputs_are_errors() {
        let mut scheduler = Scheduler::new();
        assert_eq!(scheduler.handle(I::JobFinished).unwrap_err().input, "job_finished");
        assert_eq!(scheduler.handle(I::RecordingEnded).unwrap_err().input, "recording_ended");
    }
}
