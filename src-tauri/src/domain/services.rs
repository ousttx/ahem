//! The services the interface offers by name (`src/services.ts`): one key per service, used for
//! speech recognition and for language models where the service has both.

use super::modes::{Connection, ProviderKind};

/// OpenAI-compatible addresses of the named services.
pub const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
pub const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";

/// Names are the same in every interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Anthropic,
    OpenAi,
    DeepSeek,
    Groq,
    /// Speech recognition only.
    Deepgram,
}

impl Service {
    /// The id the interface knows the service by.
    pub fn id(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::DeepSeek => "deepseek",
            Self::Groq => "groq",
            Self::Deepgram => "deepgram",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic (Claude)",
            Self::OpenAi => "OpenAI",
            Self::DeepSeek => "DeepSeek",
            Self::Groq => "Groq",
            Self::Deepgram => "Deepgram",
        }
    }

    /// What a connection to the service stores: its provider and base URL (tests check
    /// [`Self::of`] against them).
    #[cfg(test)]
    fn provider(self) -> ProviderKind {
        match self {
            Self::Anthropic => ProviderKind::Anthropic,
            Self::OpenAi => ProviderKind::OpenAi,
            Self::DeepSeek | Self::Groq => ProviderKind::OpenAiCompatible,
            Self::Deepgram => ProviderKind::Deepgram,
        }
    }

    #[cfg(test)]
    fn base_url(self) -> Option<&'static str> {
        match self {
            Self::Anthropic | Self::OpenAi | Self::Deepgram => None,
            Self::DeepSeek => Some(DEEPSEEK_BASE_URL),
            Self::Groq => Some(GROQ_BASE_URL),
        }
    }

    /// The service of a stored connection, if it is one of these.
    pub fn of(connection: &Connection) -> Option<Self> {
        let same = |url: &str| {
            connection.base_url.as_deref().is_some_and(|base| {
                base.trim_end_matches('/').eq_ignore_ascii_case(url.trim_end_matches('/'))
            })
        };
        match connection.provider {
            ProviderKind::Anthropic => Some(Self::Anthropic),
            ProviderKind::OpenAi => Some(Self::OpenAi),
            ProviderKind::Deepgram => Some(Self::Deepgram),
            ProviderKind::OpenAiCompatible if same(DEEPSEEK_BASE_URL) => Some(Self::DeepSeek),
            ProviderKind::OpenAiCompatible if same(GROQ_BASE_URL) => Some(Self::Groq),
            ProviderKind::OpenAiCompatible | ProviderKind::ChatGptAccount => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(provider: ProviderKind, base_url: Option<&str>) -> Connection {
        Connection {
            id: "c".into(),
            name: "c".into(),
            provider,
            base_url: base_url.map(String::from),
            key_hint: None,
            added_at_ms: 0,
            models: Vec::new(),
        }
    }

    #[test]
    fn services_are_recognized_by_provider_and_address() {
        let of = |provider, url| Service::of(&connection(provider, url));
        assert_eq!(of(ProviderKind::Anthropic, None), Some(Service::Anthropic));
        assert_eq!(of(ProviderKind::OpenAi, None), Some(Service::OpenAi));
        assert_eq!(of(ProviderKind::Deepgram, None), Some(Service::Deepgram));
        let compatible = ProviderKind::OpenAiCompatible;
        assert_eq!(of(compatible, Some("https://API.deepseek.com/")), Some(Service::DeepSeek));
        assert_eq!(of(compatible, Some("https://api.groq.com/openai/v1")), Some(Service::Groq));
        assert_eq!(of(compatible, Some("http://localhost:11434/v1")), None);
    }

    #[test]
    fn every_service_is_found_from_what_it_stores() {
        for service in [
            Service::Anthropic,
            Service::OpenAi,
            Service::DeepSeek,
            Service::Groq,
            Service::Deepgram,
        ] {
            let stored = connection(service.provider(), service.base_url());
            assert_eq!(Service::of(&stored), Some(service));
        }
    }
}
