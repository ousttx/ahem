//! Speech-to-text engines and the language models that post-process their text.

pub mod chatgpt;
pub mod cloud;
pub mod hallucinations;
pub mod languages;
pub mod llama_server;
pub mod llm;
pub mod local_server;
pub mod sherpa;
pub mod sherpa_process;
pub mod whisper_server;

/// Whether the dictation model translates to English; without a known model nothing is ruled
/// out.
pub fn translates(model_id: Option<&str>) -> bool {
    let Some(id) = model_id else { return true };
    crate::storage::catalog::find(id)
        .map(|spec| spec.translate)
        .or_else(|| cloud::find(id).map(|spec| spec.translate))
        .unwrap_or(true)
}

/// Audio and video are transcribed window by window and joined by word times: every model and
/// every cloud engine but one without word times (`gpt-transcribe`) gives them.
pub fn transcribes_media(model_id: Option<&str>) -> bool {
    model_id.and_then(cloud::find).is_none_or(|spec| spec.word_times())
}

#[cfg(test)]
mod tests {
    #[test]
    fn media_files_need_an_engine_with_word_times() {
        use super::transcribes_media;
        assert!(transcribes_media(Some("whisper-large-v3")));
        assert!(transcribes_media(Some("groq-whisper-large-v3")));
        assert!(transcribes_media(Some("deepgram-nova-3")));
        assert!(!transcribes_media(Some("openai-gpt-transcribe")));
        assert!(transcribes_media(None), "without a model the orchestrator reports it");
    }

    #[test]
    fn translation_depends_on_the_dictation_model() {
        use super::translates;
        assert!(translates(Some("whisper-large-v3")));
        assert!(!translates(Some("gigaam-v3-e2e-rnnt")));
        assert!(translates(Some("groq-whisper-large-v3")));
        assert!(!translates(Some("deepgram-nova-3")));
        assert!(translates(None));
    }
}
