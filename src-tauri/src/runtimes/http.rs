//! Shared HTTP agent: rustls with the ring provider, server certificates checked by the OS.

use std::sync::Once;
use std::time::Duration;

use serde::de::DeserializeOwned;
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};

pub fn agent() -> Agent {
    static PROVIDER: Once = Once::new();
    PROVIDER.call_once(|| {
        // Fails only if a provider is already installed, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    let config = Agent::config_builder()
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        .timeout_connect(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build();
    Agent::new_with_config(config)
}

/// A service's JSON answer. The error says where it broke, never what the answer held: it may
/// hold the user's text.
pub fn parse_json<T: DeserializeOwned>(body: &str) -> Result<T, String> {
    serde_json::from_str(body).map_err(|error| {
        format!("{:?} error at line {} column {}", error.classify(), error.line(), error.column())
    })
}
