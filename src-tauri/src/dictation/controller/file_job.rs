//! The file job of the orchestrator: a user's file or a meeting track, read window by window,
//! transcribed between dictations, merged into words, told apart by voice when asked, given to
//! the mode's language model, saved and kept in the list of transcripts.

use super::*;

impl<S: Services> Controller<S> {
    /// Cancels the running job if it is a meeting track (`track`) or the user's file.
    pub(super) fn cancel_file(&mut self, track: bool) {
        let current = self.file.as_ref().filter(|job| job.request.reply.is_some() == track);
        if let Some(id) = current.map(|job| job.id) {
            self.end_file(FileEvent::Cancelled { file: id.0 });
        }
    }

    /// The language model of a file: only a mode that processes has one. "Clean text" is for
    /// dictation; a plain dictation file is saved as it is recognized.
    fn file_llm_job(&self, mode: &Mode) -> Option<LlmJob> {
        self.llm_job(mode).filter(|_| mode.processes())
    }

    pub(super) fn file_job(&mut self, file: FileTaskId) -> Option<&mut FileJob> {
        self.file.as_mut().filter(|job| job.id == file)
    }

    pub(super) fn start_file(&mut self, request: FileRequest) {
        let id = FileTaskId(self.next_file);
        self.next_file += 1;
        let reply = request.reply.clone();
        let emit = &self.emit;
        let fail = |problem, detail: &str| match &reply {
            Some(reply) => {
                let _ = reply.send(TrackUpdate::Failed { problem, detail: detail.into() });
            }
            None => emit(CoreEvent::File(FileEvent::Failed {
                file: id.0,
                problem,
                detail: detail.into(),
                transcript: None,
                held: false,
            })),
        };
        if self.file.is_some() {
            return fail(FileProblem::Busy, "");
        }
        // A result held from the last file is let go with the user's next one (a meeting's track
        // is no file of the user's).
        if request.reply.is_none() {
            self.held_result = None;
        }
        let mode = self.mode(&request.mode_id);
        let source = request.source.clone();
        match &source {
            FileSource::Text(_) if self.file_llm_job(&mode).is_none() => {
                return fail(FileProblem::Processing, "the mode has no language model");
            }
            FileSource::Text(_) => {}
            FileSource::Media(_) => {
                // The interface refuses it before; a meeting's next track may come after the
                // model was changed.
                if !crate::engines::transcribes_media(self.settings.model_id.as_deref()) {
                    return fail(FileProblem::NoWordTimes, "");
                }
                match self.usable_engine() {
                    Err(BlockReason::NoModel) => return fail(FileProblem::NoModel, ""),
                    Err(_) => return fail(FileProblem::NoEngine, ""),
                    Ok(_) => {}
                }
            }
        }
        let name = request.name.clone();
        self.file = Some(FileJob {
            id,
            request,
            mode,
            waiting: None,
            running: None,
            parts: Vec::new(),
            duration: None,
            ended: false,
            holds_engine: matches!(source, FileSource::Media(_)),
            words: Vec::new(),
            turns: Vec::new(),
            processed: None,
        });
        if reply.is_none() {
            (self.emit)(CoreEvent::File(FileEvent::Started { file: id.0, name }));
        }
        match source {
            FileSource::Media(path) => {
                self.engine_input(EngineInput::Acquire);
                self.services.open_file(id, path);
            }
            FileSource::Text(text) => self.process_file_text(text),
        }
    }

    pub(super) fn chunk_transcribed(
        &mut self,
        file: FileTaskId,
        chunk: u32,
        result: Result<String, String>,
    ) {
        let whisper = self.whisper_started;
        let Some(job) = self.file_job(file) else { return };
        let Some(window) = job.running.take().filter(|window| window.index == chunk) else {
            return;
        };
        let words = match result.map(|json| parse_words(&json, &window)) {
            Ok(Ok(words)) => words,
            Ok(Err(error)) => return self.fail_file(FileProblem::Transcription, error.to_string()),
            Err(detail) => return self.fail_file(FileProblem::Transcription, detail),
        };
        // Whisper invents credits on silence, and a dictation drops them too; a microphone track
        // with the echo silenced is mostly silence.
        let words = if whisper { without_credits(words, &window) } else { words };
        job.parts.push((window, words));
        let millis = |seconds: f64| (seconds.max(0.0) * 1000.0).round() as u64;
        let (done_ms, total_ms) = (millis(window.keep_to), job.total_ms());
        match &job.request.reply {
            Some(reply) => {
                let _ = reply.send(TrackUpdate::Progress { done_ms, total_ms });
            }
            None => {
                let progress = FileEvent::Progress { file: file.0, done_ms, total_ms };
                (self.emit)(CoreEvent::File(progress));
            }
        }
        self.finish_file_if_done();
    }

    /// After the last window: merge, then the language model or the export.
    pub(super) fn finish_file_if_done(&mut self) {
        let Some(job) = self.file.as_mut() else { return };
        if !job.ended || job.waiting.is_some() || job.running.is_some() || !job.holds_engine {
            return;
        }
        job.holds_engine = false;
        job.words = merge(std::mem::take(&mut job.parts));
        if job.request.replacements {
            let replacer = Replacer::new(&self.settings.replacements);
            job.words = replace_words(std::mem::take(&mut job.words), &replacer);
        }
        let words = job.words.clone();
        let reply = job.request.reply.clone();
        self.engine_input(EngineInput::Release);
        if let Some(reply) = reply {
            let _ = reply.send(TrackUpdate::Done(words));
            let id = self.file.as_ref().map_or(0, |job| job.id.0);
            return self.end_file(FileEvent::Finished {
                file: id,
                path: String::new(),
                processing_error: None,
                transcript: None,
            });
        }
        if words.is_empty() {
            return self.fail_file(FileProblem::NoSpeech, String::new());
        }
        let Some(job) = self.file.as_ref() else { return };
        if let (FileSource::Media(path), Some(_)) = (&job.request.source, &job.request.speakers) {
            let (id, path) = (job.id, path.clone());
            let total_ms = job.total_ms();
            let step = SpeakersStep::Segments;
            (self.emit)(CoreEvent::File(FileEvent::Speakers {
                file: id.0,
                step,
                done_ms: 0,
                total_ms,
            }));
            return self.services.diarize_file(id, path, words);
        }
        self.finish_words(&[]);
    }

    /// The merged words to the language model or to the file, by speaker when the voices
    /// were told apart and there are at least two.
    pub(super) fn finish_words(&mut self, segments: &[SpeakerSegment]) {
        let Some(job) = self.file.as_mut() else { return };
        let words = job.words.clone();
        job.turns =
            transcript::turns(Vec::new(), transcript::others_speakers(words.clone(), segments));
        let Some(job) = self.file.as_ref() else { return };
        let voices = job.turns.iter().any(|turn| matches!(turn.speaker, Speaker::Person(_)));
        let by_speaker = job
            .request
            .speakers
            .clone()
            .filter(|_| voices)
            .map(|labels| (labels, job.turns.clone()));
        if self.file_llm_job(&job.mode).is_some() {
            let text = match &by_speaker {
                Some((labels, turns)) => turns
                    .iter()
                    .map(|turn| format!("{}: {}", labels.name(turn.speaker), turn.text()))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                None => {
                    paragraphs(&words).into_iter().map(|p| p.text).collect::<Vec<_>>().join("\n\n")
                }
            };
            self.process_file_text(text);
        } else {
            let text = job.exported();
            self.save_file(text, None);
        }
    }

    pub(super) fn process_file_text(&mut self, text: String) {
        let Some(job) = self.file.as_ref() else { return };
        let (id, llm) = (job.id, self.file_llm_job(&job.mode));
        match llm {
            Some(llm) => {
                (self.emit)(CoreEvent::File(FileEvent::Processing { file: id.0 }));
                self.services.process_file(id, text, llm);
            }
            None => {
                self.fail_file(FileProblem::Processing, "the language model is not set up".into())
            }
        }
    }

    pub(super) fn file_processed(&mut self, result: Result<String, String>) {
        let Some(job) = self.file.as_mut() else { return };
        match result {
            Ok(text) => {
                job.processed = Some(text.trim().to_string());
                // A processed file is saved as Markdown or text (`FormatNeedsTimes` otherwise).
                let text = crate::files::export::processed_export(&text, &job.request.format)
                    .unwrap_or_else(|| format!("{}\n", text.trim()));
                self.save_file(text, None);
            }
            // As in dictation: the transcript is kept when the language model fails.
            // By speaker too, as it would be saved without a language model.
            Err(detail) if !job.words.is_empty() => {
                let text = job.exported();
                self.save_file(text, Some(detail));
            }
            Err(detail) => self.fail_file(FileProblem::Processing, detail),
        }
    }

    pub(super) fn save_file(&mut self, text: String, processing_error: Option<String>) {
        let Some((id, output)) = self.file.as_ref().map(|job| (job.id, job.request.output.clone()))
        else {
            return;
        };
        match self.services.write_file(&output, &text) {
            Ok(()) => {
                let transcript = self.keep_transcript(true);
                self.end_file(FileEvent::Finished {
                    file: id.0,
                    path: output.display().to_string(),
                    processing_error,
                    transcript,
                })
            }
            // The result, maybe hours of work, is kept all the same, to be saved elsewhere: in the
            // list of transcripts if it can be, and in memory until the next file, as the list
            // is on the same disk, which may be the full one.
            Err(detail) => {
                let transcript = self.keep_transcript(false);
                self.held_result = Some(HeldResult { text, transcript });
                self.end_file(FileEvent::Failed {
                    file: id.0,
                    problem: FileProblem::Save,
                    detail,
                    transcript,
                    held: true,
                })
            }
        }
    }

    /// Keeps an audio or video file's transcript in the list of transcripts, with
    /// the file it was `written` to; a text file has none. A failure leaves the saved file as
    /// the result.
    pub(super) fn keep_transcript(&mut self, written: bool) -> Option<i64> {
        let job = self.file.as_ref()?;
        let FileSource::Media(source) = &job.request.source else { return None };
        let millis = |seconds: f64| (seconds.max(0.0) * 1000.0).round() as i64;
        let duration_ms = job.duration.map_or_else(
            || millis(job.turns.last().map_or(0.0, |turn| turn.end)),
            |duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        );
        let transcript = FileTranscript {
            source: source.clone(),
            output: written.then(|| job.request.output.clone()),
            duration_ms,
            turns: job.turns.clone(),
            processed: job.processed.clone(),
        };
        let id = job.id;
        match self.services.keep_transcript(transcript) {
            Ok(entry) => Some(entry),
            Err(detail) => {
                log_line!("file {}: transcript not kept: {detail}", id.0);
                None
            }
        }
    }

    /// Another model was chosen while a media file is transcribed: the rest of it goes to the
    /// new one, unless that one cannot go on with it — no word times, or no translation for a
    /// file being translated (`translated`: the model before translates) — and then the file
    /// stops at once and says why.
    pub(super) fn stop_a_file_the_model_cannot_go_on_with(&mut self, translated: bool) {
        let Some(job) = self.file.as_ref().filter(|job| job.holds_engine) else { return };
        let model = self.settings.model_id.as_deref();
        let problem = if !crate::engines::transcribes_media(model) {
            FileProblem::NoWordTimes
        } else if job.mode.action == ModeAction::TranslateToEnglish
            && translated
            && !crate::engines::translates(model)
        {
            FileProblem::NoTranslation
        } else {
            return;
        };
        self.fail_file(problem, String::new());
    }

    pub(super) fn fail_file(&mut self, problem: FileProblem, detail: String) {
        let Some(id) = self.file.as_ref().map(|job| job.id) else { return };
        self.end_file(FileEvent::Failed {
            file: id.0,
            problem,
            detail,
            transcript: None,
            held: false,
        });
    }

    /// Finished, failed or cancelled: stops reading, drops queued windows, frees the engine; a
    /// window still at the engine frees it with its answer.
    pub(super) fn end_file(&mut self, event: FileEvent) {
        let Some(job) = self.file.take() else { return };
        self.schedule(SchedulerInput::CancelFile(job.id));
        self.services.close_file(job.id);
        if job.holds_engine {
            if job.running.is_some() {
                self.ended_chunk = Some(job.id);
            } else {
                self.engine_input(EngineInput::Release);
            }
        }
        match job.request.reply {
            // `Done` went out already.
            Some(reply) => {
                let update = match event {
                    FileEvent::Failed { problem, detail, .. } => {
                        Some(TrackUpdate::Failed { problem, detail })
                    }
                    FileEvent::Cancelled { .. } => Some(TrackUpdate::Cancelled),
                    _ => None,
                };
                if let Some(update) = update {
                    let _ = reply.send(update);
                }
            }
            None => (self.emit)(CoreEvent::File(event)),
        }
    }
}
