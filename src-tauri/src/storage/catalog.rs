//! Model catalog: the models the app can download, with pinned size and SHA-256.
//!
//! A Whisper model enters the catalog only after it was checked against a conversion of the
//! original weights with the official converter: byte-identical, or differing only in the sign of
//! zero in the first mel filter value. Models are listed from the most demanding to
//! the lightest, and the interface shows them in this order.
//!
//! Every model comes from an official source, checked for each entry: Whisper models from the
//! whisper.cpp repository on Hugging Face at a pinned commit, sherpa-onnx models from the release
//! of the sherpa-onnx project on GitHub, llama.cpp models from the GGUF repositories of
//! the llama.cpp project (`ggml-org`) on Hugging Face at a pinned commit.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::engines::languages;

const CATALOG: &str = include_str!("../../resources/models.json");

/// Whisper models: `<this><40-character commit>/<file>`.
const WHISPER_SOURCE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/";
/// sherpa-onnx models: `<this><file>`.
const SHERPA_SOURCE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/";
/// llama.cpp models: `<this><repository>/resolve/<40-character commit>/<file>`.
const LLAMA_SOURCE: &str = "https://huggingface.co/ggml-org/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    pub id: String,
    pub name: String,
    /// Who made the model and its license, for the credit the Models page gives it (some
    /// licenses, such as CC-BY-4.0, require one).
    pub author: String,
    pub license: String,
    /// What is downloaded: a Whisper `.bin`, a sherpa-onnx `.tar.bz2` archive, or a llama.cpp
    /// model `.gguf` (its audio projector is in [`LlamaSpec`]).
    pub file: String,
    /// Pinned to a repository revision, never a moving branch.
    pub url: String,
    pub size: u64,
    pub sha256: String,
    /// Whisper's built-in translation to English works with this model.
    pub translate: bool,
    /// The model's own language list when it knows fewer languages than Whisper; without one it
    /// recognizes every language of `engines::languages`. The interface gets the resolved list
    /// from [`ModelSpec::languages`].
    #[serde(default, skip_serializing)]
    pub languages: Option<Vec<String>>,
    pub hardware: Hardware,
    /// Present for models run by sherpa-onnx; Whisper models have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sherpa: Option<SherpaSpec>,
    /// Present for models run by llama.cpp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llama: Option<LlamaSpec>,
}

/// A llama.cpp model that hears audio: its projector file and how it is asked and answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlamaSpec {
    /// The multimodal projector (`mmproj-*.gguf`), from the model's repository and commit.
    pub mmproj: ModelFile,
    pub reply: LlamaReply,
    /// Instruction sent with the audio; none for a model that transcribes without one.
    #[serde(default)]
    pub prompt: Option<String>,
}

/// One more file of a model, pinned like the main one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile {
    pub file: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// How a llama.cpp model's answer carries the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlamaReply {
    /// The answer is the transcript.
    Text,
    /// `language Russian<asr_text>…`: the detected language, then the transcript.
    Qwen3Asr,
}

/// A sherpa-onnx model: which recognizer runs it and its files inside the archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SherpaSpec {
    pub recognizer: Recognizer,
    /// sherpa-onnx `model_type`, e.g. `nemo_transducer`; none lets sherpa-onnx detect it.
    #[serde(default)]
    pub model_type: Option<String>,
    pub encoder: String,
    pub decoder: String,
    pub joiner: String,
    pub tokens: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recognizer {
    /// Decodes a whole recording at once.
    OfflineTransducer,
    /// A streaming model: a dictation goes in while it is recorded (`LiveStream`), a file window
    /// and a dictation without a live stream whole.
    OnlineTransducer,
}

impl SherpaSpec {
    /// The files taken from the archive; everything else in it is skipped.
    pub fn files(&self) -> [&str; 4] {
        [&self.encoder, &self.decoder, &self.joiner, &self.tokens]
    }
}

/// What the model needs to be fast enough for dictation. Measured on a Ryzen 5 7600X without
/// the graphics card, for a 3.4 s phrase: Turbo 25 s, Medium 15 s, Small 4.7 s, Parakeet v3
/// 0.17 s, Nemotron 3.5 0.64 s; for an 11.3 s Russian phrase GigaAM v3 takes 0.25 s, Qwen3-ASR
/// 0.6B 1.0 s and 1.7B 2.6 s (llama.cpp, 4.0 s for the first request).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Hardware {
    /// A graphics card with at least this much memory: the engine's peak use measured on an
    /// RTX 4070 SUPER plus about 1 GB for the desktop, rounded up to a common card size.
    Gpu { memory_gb: u32 },
    /// The processor alone is enough.
    Cpu,
}

impl ModelSpec {
    /// Codes of the spoken languages the model recognizes.
    pub fn languages(&self) -> Vec<&'static str> {
        let all = languages::all().iter().map(|(code, _)| *code);
        match &self.languages {
            Some(codes) => all.filter(|code| codes.iter().any(|known| known == code)).collect(),
            None => all.collect(),
        }
    }

    /// The engine reads the dictionary: whisper-server takes it as the prompt; sherpa-onnx and
    /// llama.cpp models take no hint (`engines/sherpa.rs`, `engines/llama_server.rs`).
    pub fn takes_dictionary(&self) -> bool {
        self.sherpa.is_none() && self.llama.is_none()
    }

    /// The engine takes the chosen spoken language: Whisper and the streaming sherpa-onnx model
    /// do; the offline ones do not (Parakeet detects it itself, GigaAM knows only Russian), nor
    /// do llama.cpp models, which detect it.
    pub fn takes_language(&self) -> bool {
        self.llama.is_none()
            && self
                .sherpa
                .as_ref()
                .is_none_or(|sherpa| sherpa.recognizer == Recognizer::OnlineTransducer)
    }

    /// Bytes to download: a llama.cpp model's projector comes on top of the model.
    pub fn download_size(&self) -> u64 {
        self.size + self.llama.as_ref().map_or(0, |llama| llama.mmproj.size)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    models: Vec<ModelSpec>,
}

/// The catalog shipped with the app; its validity is checked by a test.
pub fn models() -> Vec<ModelSpec> {
    embedded().to_vec()
}

pub fn find(id: &str) -> Option<ModelSpec> {
    embedded().iter().find(|model| model.id == id).cloned()
}

/// Parsed and checked once: the catalog is part of the build.
fn embedded() -> &'static [ModelSpec] {
    static MODELS: OnceLock<Vec<ModelSpec>> = OnceLock::new();
    MODELS.get_or_init(|| parse(CATALOG).expect("embedded model catalog is valid"))
}

fn parse(text: &str) -> Result<Vec<ModelSpec>, String> {
    let file: CatalogFile = serde_json::from_str(text).map_err(|error| error.to_string())?;
    if file.models.is_empty() {
        return Err("catalog is empty".into());
    }
    let mut ids = HashSet::new();
    for model in &file.models {
        validate(model)?;
        if !ids.insert(model.id.as_str()) {
            return Err(format!("duplicate model id {}", model.id));
        }
    }
    Ok(file.models)
}

fn validate(model: &ModelSpec) -> Result<(), String> {
    let id = &model.id;
    let safe = |text: &str| {
        !text.is_empty()
            && text.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !text.starts_with('.')
    };
    // No dots: the interface looks up `models.about.<id>`, and i18next splits keys at dots.
    if !safe(id) || id.contains('.') {
        return Err(format!("bad model id {id:?}"));
    }
    let extension = match (&model.sherpa, &model.llama) {
        (Some(_), None) => ".tar.bz2",
        (None, Some(_)) => ".gguf",
        (None, None) => ".bin",
        (Some(_), Some(_)) => return Err(format!("{id}: one engine per model")),
    };
    if model.author.trim().is_empty() || model.license.trim().is_empty() {
        return Err(format!("{id}: no author or license"));
    }
    if !safe(&model.file) || !model.file.ends_with(extension) {
        return Err(format!("{id}: bad file name {:?}", model.file));
    }
    let pinned = |commit: &str| {
        commit.len() == 40 && commit.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
    };
    if (model.sherpa.is_some() || model.llama.is_some()) && model.translate {
        return Err(format!("{id}: only Whisper models translate"));
    }
    match (&model.sherpa, &model.llama) {
        (Some(sherpa), _) => {
            if model.url != format!("{SHERPA_SOURCE}{}", model.file) {
                return Err(format!("{id}: url must be {SHERPA_SOURCE}{}", model.file));
            }
            if let Some(name) = sherpa.files().into_iter().find(|name| !safe(name)) {
                return Err(format!("{id}: bad file name in the archive {name:?}"));
            }
        }
        (None, Some(llama)) => {
            // `<repository>/resolve/<commit>/`, shared by the model and its projector.
            let folder = model.url.strip_prefix(LLAMA_SOURCE).and_then(|rest| {
                let folder = rest.strip_suffix(&model.file)?;
                let (repository, commit) = folder.strip_suffix('/')?.split_once("/resolve/")?;
                (safe(repository) && pinned(commit)).then_some(folder)
            });
            let Some(folder) = folder else {
                return Err(format!(
                    "{id}: url must be {LLAMA_SOURCE}<repository>/resolve/<commit>/{}",
                    model.file
                ));
            };
            let mmproj = &llama.mmproj;
            if !safe(&mmproj.file) || !mmproj.file.ends_with(".gguf") || mmproj.file == model.file {
                return Err(format!("{id}: bad projector file name {:?}", mmproj.file));
            }
            if mmproj.url != format!("{LLAMA_SOURCE}{folder}{}", mmproj.file) {
                return Err(format!("{id}: the projector must come from the model's commit"));
            }
            if mmproj.size == 0 || !is_sha256(&mmproj.sha256) {
                return Err(format!("{id}: the projector needs a size and a sha256"));
            }
            if llama.prompt.as_ref().is_some_and(|prompt| prompt.trim().is_empty()) {
                return Err(format!("{id}: empty prompt"));
            }
        }
        (None, None) => {
            let commit = model
                .url
                .strip_prefix(WHISPER_SOURCE)
                .and_then(|rest| rest.strip_suffix(&format!("/{}", model.file)));
            if !commit.is_some_and(pinned) {
                return Err(format!("{id}: url must be {WHISPER_SOURCE}<commit>/{}", model.file));
            }
        }
    }
    if let Some(codes) = &model.languages {
        let known = |code: &String| languages::all().iter().any(|(known, _)| known == code);
        if codes.is_empty() || !codes.iter().all(known) {
            return Err(format!("{id}: unknown or empty language list"));
        }
    }
    if model.size == 0 {
        return Err(format!("{id}: size is zero"));
    }
    if !is_sha256(&model.sha256) {
        return Err(format!("{id}: sha256 must be 64 lowercase hex characters"));
    }
    if model.hardware == (Hardware::Gpu { memory_gb: 0 }) {
        return Err(format!("{id}: graphics card memory is zero"));
    }
    Ok(())
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

    fn spec() -> ModelSpec {
        ModelSpec {
            id: "m".into(),
            name: "M".into(),
            author: "A".into(),
            license: "MIT".into(),
            file: "m.bin".into(),
            url: format!("{WHISPER_SOURCE}{COMMIT}/m.bin"),
            size: 1,
            sha256: "a".repeat(64),
            translate: true,
            languages: None,
            hardware: Hardware::Gpu { memory_gb: 4 },
            sherpa: None,
            llama: None,
        }
    }

    const LLAMA_COMMIT: &str = "928ab958557df9aa2ef1c93e0e83c7ad0933fae2";

    fn llama_spec() -> ModelSpec {
        let folder = format!("{LLAMA_SOURCE}Qwen3-ASR-0.6B-GGUF/resolve/{LLAMA_COMMIT}/");
        ModelSpec {
            file: "m-Q8_0.gguf".into(),
            url: format!("{folder}m-Q8_0.gguf"),
            size: 10,
            translate: false,
            hardware: Hardware::Cpu,
            llama: Some(LlamaSpec {
                mmproj: ModelFile {
                    file: "mmproj-m-Q8_0.gguf".into(),
                    url: format!("{folder}mmproj-m-Q8_0.gguf"),
                    size: 5,
                    sha256: "b".repeat(64),
                },
                reply: LlamaReply::Qwen3Asr,
                prompt: None,
            }),
            ..spec()
        }
    }

    fn sherpa_spec() -> ModelSpec {
        ModelSpec {
            file: "m.tar.bz2".into(),
            url: format!("{SHERPA_SOURCE}m.tar.bz2"),
            translate: false,
            languages: Some(vec!["en".into(), "ru".into()]),
            hardware: Hardware::Cpu,
            sherpa: Some(SherpaSpec {
                recognizer: Recognizer::OfflineTransducer,
                model_type: Some("nemo_transducer".into()),
                encoder: "encoder.int8.onnx".into(),
                decoder: "decoder.int8.onnx".into(),
                joiner: "joiner.int8.onnx".into(),
                tokens: "tokens.txt".into(),
            }),
            ..spec()
        }
    }

    #[test]
    fn embedded_catalog_is_valid() {
        let models = models();
        assert!(models.iter().any(|model| model.id == "whisper-large-v3"));
        assert!(find("whisper-large-v3-turbo").is_some_and(|model| !model.translate));
        let parakeet = find("parakeet-tdt-0_6b-v3").expect("Parakeet v3");
        let recognizer = |model: &ModelSpec| model.sherpa.as_ref().map(|sherpa| sherpa.recognizer);
        assert_eq!(recognizer(&parakeet), Some(Recognizer::OfflineTransducer));
        assert_eq!(parakeet.languages().len(), 25);
        let nemotron = find("nemotron-3_5-asr-streaming-0_6b").expect("Nemotron 3.5");
        assert_eq!(recognizer(&nemotron), Some(Recognizer::OnlineTransducer));
        assert!(nemotron.languages().contains(&"ja"));
        for id in ["gigaam-v3-e2e-rnnt", "gigaam-v3-rnnt"] {
            let gigaam = find(id).expect(id);
            assert_eq!(recognizer(&gigaam), Some(Recognizer::OfflineTransducer));
            assert_eq!(gigaam.languages(), vec!["ru"]);
        }
        for id in ["qwen3-asr-1_7b", "qwen3-asr-0_6b"] {
            let qwen = find(id).expect(id);
            assert_eq!(qwen.llama.as_ref().map(|llama| llama.reply), Some(LlamaReply::Qwen3Asr));
            assert!(qwen.llama.as_ref().is_some_and(|llama| llama.prompt.is_none()));
            assert_eq!(qwen.languages().len(), 30);
            assert!(qwen.languages().contains(&"ru"));
        }
    }

    /// The catalog is parsed with `validate`, so this restates the rule for the reader.
    #[test]
    fn every_model_comes_from_an_official_source() {
        for model in models() {
            let source = match (&model.sherpa, &model.llama) {
                (Some(_), _) => SHERPA_SOURCE,
                (None, Some(_)) => LLAMA_SOURCE,
                (None, None) => WHISPER_SOURCE,
            };
            assert!(model.url.starts_with(source), "{}: {}", model.id, model.url);
        }
    }

    #[test]
    fn models_go_from_the_most_demanding_to_the_lightest() {
        let order: Vec<(String, Hardware)> =
            models().into_iter().map(|model| (model.id, model.hardware)).collect();
        let gpu = |memory_gb| Hardware::Gpu { memory_gb };
        let expected = [
            ("whisper-large-v3", gpu(6)),
            ("whisper-large-v3-turbo", gpu(4)),
            ("whisper-medium", gpu(4)),
            ("qwen3-asr-1_7b", Hardware::Cpu),
            ("qwen3-asr-0_6b", Hardware::Cpu),
            ("parakeet-tdt-0_6b-v3", Hardware::Cpu),
            ("nemotron-3_5-asr-streaming-0_6b", Hardware::Cpu),
            ("gigaam-v3-e2e-rnnt", Hardware::Cpu),
            ("gigaam-v3-rnnt", Hardware::Cpu),
        ];
        let expected: Vec<(String, Hardware)> =
            expected.into_iter().map(|(id, hardware)| (id.to_string(), hardware)).collect();
        assert_eq!(order, expected);
    }

    #[test]
    fn hardware_reaches_the_interface_with_its_kind() {
        let json = serde_json::to_value(spec()).unwrap();
        assert_eq!(json["hardware"], serde_json::json!({ "kind": "gpu", "memory_gb": 4 }));
        let cpu = ModelSpec { hardware: Hardware::Cpu, ..spec() };
        let json = serde_json::to_value(cpu).unwrap();
        assert_eq!(json["hardware"], serde_json::json!({ "kind": "cpu" }));
    }

    #[test]
    fn languages_follow_the_model() {
        let all = spec().languages();
        assert_eq!(all.len(), languages::all().len());
        assert!(all.contains(&"ru") && all.contains(&"yue"));
        let russian = ModelSpec { languages: Some(vec!["ru".into()]), ..spec() };
        assert_eq!(russian.languages(), vec!["ru"]);
        // An own list keeps the order of the language table.
        let own = ModelSpec { languages: Some(vec!["ru".into(), "en".into()]), ..spec() };
        assert_eq!(own.languages(), vec!["en", "ru"]);
    }

    #[test]
    fn hints_reach_only_the_engines_that_read_them() {
        let whisper = find("whisper-large-v3-turbo").unwrap();
        assert!(whisper.takes_dictionary() && whisper.takes_language());
        let parakeet = find("parakeet-tdt-0_6b-v3").unwrap();
        assert!(!parakeet.takes_dictionary() && !parakeet.takes_language());
        let nemotron = find("nemotron-3_5-asr-streaming-0_6b").unwrap();
        assert!(!nemotron.takes_dictionary() && nemotron.takes_language());
        let gigaam = find("gigaam-v3-e2e-rnnt").unwrap();
        assert!(!gigaam.takes_dictionary() && !gigaam.takes_language());
        let qwen = find("qwen3-asr-0_6b").unwrap();
        assert!(!qwen.takes_dictionary() && !qwen.takes_language());
    }

    #[test]
    fn a_llama_model_downloads_with_its_projector() {
        assert_eq!(llama_spec().download_size(), 15);
        assert_eq!(spec().download_size(), 1);
        let json = serde_json::to_value(llama_spec()).unwrap();
        assert_eq!(json["llama"]["reply"], "qwen3_asr");
        assert!(serde_json::to_value(spec()).unwrap().get("llama").is_none());
    }

    type Change = Box<dyn Fn(&mut ModelSpec)>;

    fn assert_rejected(base: fn() -> ModelSpec, cases: Vec<(&str, Change)>) {
        assert!(validate(&base()).is_ok());
        for (name, change) in cases {
            let mut model = base();
            change(&mut model);
            assert!(validate(&model).is_err(), "{name} accepted");
        }
    }

    #[test]
    fn rejects_bad_entries() {
        let cases: Vec<(&str, Change)> = vec![
            ("id", Box::new(|m| m.id = "../m".into())),
            ("dot in id", Box::new(|m| m.id = "model-0.6b".into())),
            ("no author", Box::new(|m| m.author = " ".into())),
            ("no license", Box::new(|m| m.license = String::new())),
            ("file", Box::new(|m| m.file = "dir/m.bin".into())),
            ("archive", Box::new(|m| m.file = "m.tar.bz2".into())),
            ("http", Box::new(|m| m.url = m.url.replace("https://", "http://"))),
            ("other host", Box::new(|m| m.url = format!("https://example.org/{COMMIT}/m.bin"))),
            ("file in url", Box::new(|m| m.url = m.url.replace("m.bin", "other.bin"))),
            ("branch", Box::new(|m| m.url = format!("{WHISPER_SOURCE}main/m.bin"))),
            ("short commit", Box::new(|m| m.url = format!("{WHISPER_SOURCE}5359861c/m.bin"))),
            ("size", Box::new(|m| m.size = 0)),
            ("hash length", Box::new(|m| m.sha256 = "a".repeat(63))),
            ("hash case", Box::new(|m| m.sha256 = "A".repeat(64))),
            ("card memory", Box::new(|m| m.hardware = Hardware::Gpu { memory_gb: 0 })),
            ("unknown language", Box::new(|m| m.languages = Some(vec!["xx".into()]))),
            ("no languages", Box::new(|m| m.languages = Some(Vec::new()))),
        ];
        assert_rejected(spec, cases);
    }

    #[test]
    fn rejects_bad_sherpa_entries() {
        let inner = |change: fn(&mut SherpaSpec)| -> Change {
            Box::new(move |m| change(m.sherpa.as_mut().unwrap()))
        };
        let cases: Vec<(&str, Change)> = vec![
            ("not an archive", Box::new(|m| m.file = "m.bin".into())),
            ("other source", Box::new(|m| m.url = format!("{WHISPER_SOURCE}{COMMIT}/m.tar.bz2"))),
            ("other tag", Box::new(|m| m.url = m.url.replace("asr-models", "v2.0.0"))),
            ("translates", Box::new(|m| m.translate = true)),
            ("path in archive", inner(|s| s.encoder = "../encoder.onnx".into())),
            ("empty name", inner(|s| s.tokens = String::new())),
        ];
        assert_rejected(sherpa_spec, cases);
    }

    #[test]
    fn rejects_bad_llama_entries() {
        let inner = |change: fn(&mut LlamaSpec)| -> Change {
            Box::new(move |m| change(m.llama.as_mut().unwrap()))
        };
        let other_commit = "a".repeat(40);
        let cases: Vec<(&str, Change)> = vec![
            ("not gguf", Box::new(|m| m.file = "m.bin".into())),
            ("other org", Box::new(|m| m.url = m.url.replace("ggml-org", "someone"))),
            (
                "branch",
                Box::new(|m| m.url = format!("{LLAMA_SOURCE}Qwen3-ASR/resolve/main/{}", m.file)),
            ),
            ("file in url", Box::new(|m| m.url = m.url.replace("/m-Q8_0", "/other"))),
            ("nested", Box::new(|m| m.url = m.url.replace("Qwen3-ASR-0.6B-GGUF", "a/b"))),
            ("translates", Box::new(|m| m.translate = true)),
            ("two engines", Box::new(|m| m.sherpa = sherpa_spec().sherpa)),
            (
                "projector from another commit",
                Box::new(move |m| {
                    let llama = m.llama.as_mut().unwrap();
                    llama.mmproj.url = llama.mmproj.url.replace(LLAMA_COMMIT, &other_commit);
                }),
            ),
            (
                "projector from another repository",
                inner(|l| l.mmproj.url = l.mmproj.url.replace("Qwen3-ASR-0.6B", "Other")),
            ),
            ("projector not gguf", inner(|l| l.mmproj.file = "mmproj.bin".into())),
            ("projector is the model", inner(|l| l.mmproj.file = "m-Q8_0.gguf".into())),
            ("projector size", inner(|l| l.mmproj.size = 0)),
            ("projector hash", inner(|l| l.mmproj.sha256 = "B".repeat(64))),
            ("empty prompt", inner(|l| l.prompt = Some(" ".into()))),
        ];
        assert_rejected(llama_spec, cases);
    }

    #[test]
    fn sherpa_details_stay_off_the_interface_languages() {
        let json = serde_json::to_value(sherpa_spec()).unwrap();
        assert!(json.get("languages").is_none(), "the interface gets the resolved list");
        assert_eq!(json["sherpa"]["recognizer"], "offline_transducer");
        assert!(serde_json::to_value(spec()).unwrap().get("sherpa").is_none());
    }

    #[test]
    fn rejects_duplicates_and_empty() {
        let one = serde_json::to_string(&spec()).unwrap();
        assert!(parse(&format!(r#"{{ "models": [{one}, {one}] }}"#)).is_err());
        assert!(parse(r#"{ "models": [] }"#).is_err());
        assert!(parse(&format!(r#"{{ "models": [{one}] }}"#)).is_ok());
    }

    /// The sherpa-onnx project may replace a file of a release under the same name (it did for
    /// Parakeet): the pinned size and SHA-256 would then refuse every download. Asks GitHub what
    /// each release publishes and compares; run it before a release and when a download fails
    /// verification.
    #[test]
    #[ignore = "needs the network: cargo test -- --ignored pins_match_what_github_publishes"]
    fn pins_match_what_github_publishes() {
        use crate::runtimes::http::agent;

        let agent = agent();
        let mut releases: std::collections::HashMap<String, serde_json::Value> = Default::default();
        for model in models().iter().filter(|model| model.url.starts_with(SHERPA_SOURCE)) {
            let release =
                SHERPA_SOURCE.trim_end_matches('/').rsplit('/').next().unwrap().to_string();
            let json = releases.entry(release.clone()).or_insert_with(|| {
                let url = format!(
                    "https://api.github.com/repos/k2-fsa/sherpa-onnx/releases/tags/{release}"
                );
                let mut response = agent.get(&url).call().expect("GitHub answers");
                serde_json::from_str(&response.body_mut().read_to_string().unwrap()).unwrap()
            });
            let asset = json["assets"]
                .as_array()
                .unwrap()
                .iter()
                .find(|asset| asset["name"] == model.file.as_str())
                .unwrap_or_else(|| panic!("{} is not in the release any more", model.file));
            assert_eq!(asset["size"].as_u64(), Some(model.size), "size of {}", model.id);
            if let Some(digest) = asset["digest"].as_str() {
                assert_eq!(digest, format!("sha256:{}", model.sha256), "SHA-256 of {}", model.id);
            }
        }
    }
}
