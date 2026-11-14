//! API keys, kept apart from the settings file and encrypted by the OS for the current user
//! (`platform::protect`). A key is never sent back to the interface after it is saved.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::platform;
use crate::util::hex;

/// Writes from different threads (a ChatGPT token renewal, a new key) wait for each other.
static WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone)]
pub struct SecretStore {
    path: PathBuf,
}

impl SecretStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn get(&self, id: &str) -> io::Result<Option<String>> {
        let Some(hex) = self.read()?.remove(id) else { return Ok(None) };
        let sealed = from_hex(&hex).ok_or_else(|| io::Error::other("damaged secret"))?;
        let plain = platform::unprotect(&sealed)?;
        String::from_utf8(plain).map(Some).map_err(|_| io::Error::other("damaged secret"))
    }

    pub fn set(&self, id: &str, secret: &str) -> io::Result<()> {
        let _one = WRITE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut all = self.read_for_writing()?;
        all.insert(id.to_string(), hex(&platform::protect(secret.as_bytes())?));
        self.write(&all)
    }

    pub fn delete(&self, id: &str) -> io::Result<()> {
        let _one = WRITE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut all = self.read_for_writing()?;
        if all.remove(id).is_some() {
            self.write(&all)?;
        }
        Ok(())
    }

    /// The keys by id. A file that is not JSON is damaged (`InvalidData`); JSON of another shape
    /// was written by a newer version (`Unsupported`): a version field or keys of another form.
    fn read(&self) -> io::Result<BTreeMap<String, String>> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(error) => return Err(error),
        };
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        serde_json::from_value(value).map_err(|_| {
            io::Error::new(io::ErrorKind::Unsupported, "the key file is from a newer version")
        })
    }

    /// The stored keys before a change. A damaged file holds no key that could still be read:
    /// it is started over, or no key could ever be added again. A newer version's file is kept:
    /// the change fails instead.
    fn read_for_writing(&self) -> io::Result<BTreeMap<String, String>> {
        match self.read() {
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                log_line!("the key file is damaged and starts over");
                Ok(BTreeMap::new())
            }
            read => read,
        }
    }

    fn write(&self, all: &BTreeMap<String, String>) -> io::Result<()> {
        super::write_json(&self.path, &serde_json::to_string_pretty(all).map_err(io::Error::other)?)
    }
}

/// Shown in the interface instead of the key: `••••abcd`. A short key shows nothing, since four
/// characters would be most of it.
pub fn hint(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < MIN_HINTED_CHARS {
        return "••••".into();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••{tail}")
}

const MIN_HINTED_CHARS: usize = 16;

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn store() -> SecretStore {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name =
            format!("secrets-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts").join(name);
        let _ = fs::remove_dir_all(&dir);
        SecretStore::new(dir.join("secrets.json"))
    }

    #[test]
    fn a_damaged_key_file_starts_over_when_a_key_is_saved() {
        let store = store();
        fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        fs::write(&store.path, "not json").unwrap();
        assert_eq!(store.read().unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert!(store.read_for_writing().unwrap().is_empty());
    }

    /// A newer version's key file (after going back from a beta) is never written over: its
    /// keys would be lost.
    #[test]
    fn a_newer_key_file_is_not_written_over() {
        let store = store();
        fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        let newer = r#"{"version": 2, "keys": {"openai": "00ff"}}"#;
        fs::write(&store.path, newer).unwrap();
        assert_eq!(store.read().unwrap_err().kind(), io::ErrorKind::Unsupported);
        assert!(store.set("anthropic", "sk-ant-other").is_err());
        assert!(store.delete("openai").is_err());
        assert_eq!(fs::read_to_string(&store.path).unwrap(), newer);
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(from_hex(&hex(&[0, 15, 255])), Some(vec![0, 15, 255]));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
    }

    #[test]
    fn hint_shows_only_the_last_characters() {
        assert_eq!(hint("sk-ant-1234567890abcd"), "••••abcd");
        assert_eq!(hint("short-key-12"), "••••");
        assert_eq!(hint("ab"), "••••");
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "uses the login Keychain: cargo test -- --ignored keys_are_stored_encrypted"
    )]
    fn keys_are_stored_encrypted() {
        let store = store();
        assert_eq!(store.get("openai").unwrap(), None);
        store.set("openai", "sk-secret-key-42").unwrap();
        store.set("anthropic", "sk-ant-other").unwrap();
        assert_eq!(store.get("openai").unwrap().as_deref(), Some("sk-secret-key-42"));
        let file = fs::read_to_string(&store.path).unwrap();
        assert!(!file.contains("sk-secret-key-42"), "never in plain text");
        store.delete("openai").unwrap();
        assert_eq!(store.get("openai").unwrap(), None);
        assert_eq!(store.get("anthropic").unwrap().as_deref(), Some("sk-ant-other"));
    }
}
