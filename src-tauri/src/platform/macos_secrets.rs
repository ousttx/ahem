//! Sealing API keys on macOS: a random 256-bit key lives in the
//! user's login Keychain, and each secret is sealed with AES-256-GCM under it. `secrets.json`
//! keeps the same shape as on Windows, where DPAPI does the sealing.

use std::io;
use std::sync::Mutex;

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

/// `errSecItemNotFound`.
const ITEM_NOT_FOUND: i32 = -25300;

/// The Keychain item: the app identifier (`tauri.conf.json`) as its service.
const SERVICE: &str = "com.useahem.ahem";
/// The identifier before: its key moves over once (`moved_key`).
const OLD_SERVICE: &str = "com.useahem.app";
const ACCOUNT: &str = "secrets-key";
const KEY_LEN: usize = 32;
/// First byte of a sealed secret, so another scheme can follow later.
const FORMAT: u8 = 1;

pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    seal(&key()?, data, crate::util::random_bytes::<NONCE_LEN>()?)
}

pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    open(&key()?, data)
}

/// The Keychain key, read once per run; created on first use.
fn key() -> io::Result<[u8; KEY_LEN]> {
    static KEY: Mutex<Option<[u8; KEY_LEN]>> = Mutex::new(None);
    let mut cached = KEY.lock().map_err(|_| io::Error::other("secret key lock"))?;
    if let Some(key) = *cached {
        return Ok(key);
    }
    let key = match get_generic_password(SERVICE, ACCOUNT) {
        Ok(bytes) => <[u8; KEY_LEN]>::try_from(bytes.as_slice())
            .map_err(|_| io::Error::other("the Keychain secret key is damaged"))?,
        Err(error) if error.code() == ITEM_NOT_FOUND => {
            // An old item that cannot be read (access denied) is left: a new key starts over,
            // instead of asking for the old one at every use.
            let moved = moved_key().unwrap_or_else(|error| {
                log_line!("the key of the earlier identifier stays where it is: {error}");
                None
            });
            let key = match moved {
                Some(key) => key,
                None => crate::util::random_bytes::<KEY_LEN>()?,
            };
            set_generic_password(SERVICE, ACCOUNT, &key).map_err(keychain)?;
            if moved.is_some() {
                let _ = delete_generic_password(OLD_SERVICE, ACCOUNT);
            }
            key
        }
        Err(error) => return Err(keychain(error)),
    };
    *cached = Some(key);
    Ok(key)
}

/// The key kept under the identifier before, if there is one.
fn moved_key() -> io::Result<Option<[u8; KEY_LEN]>> {
    match get_generic_password(OLD_SERVICE, ACCOUNT) {
        Ok(bytes) => <[u8; KEY_LEN]>::try_from(bytes.as_slice())
            .map(Some)
            .map_err(|_| io::Error::other("the Keychain secret key is damaged")),
        Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
        Err(error) => Err(keychain(error)),
    }
}

fn keychain(error: security_framework::base::Error) -> io::Error {
    io::Error::other(format!("Keychain error {}", error.code()))
}

fn cipher(key: &[u8; KEY_LEN]) -> io::Result<LessSafeKey> {
    let key = UnboundKey::new(&AES_256_GCM, key).map_err(|_| io::Error::other("bad secret key"))?;
    Ok(LessSafeKey::new(key))
}

/// `FORMAT`, the nonce, then the ciphertext with its tag.
fn seal(key: &[u8; KEY_LEN], data: &[u8], nonce: [u8; NONCE_LEN]) -> io::Result<Vec<u8>> {
    let mut sealed = data.to_vec();
    cipher(key)?
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut sealed)
        .map_err(|_| io::Error::other("cannot seal the secret"))?;
    let mut out = Vec::with_capacity(1 + NONCE_LEN + sealed.len());
    out.push(FORMAT);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

fn open(key: &[u8; KEY_LEN], data: &[u8]) -> io::Result<Vec<u8>> {
    let damaged = || io::Error::other("damaged secret");
    let (&format, rest) = data.split_first().ok_or_else(damaged)?;
    if format != FORMAT || rest.len() < NONCE_LEN {
        return Err(damaged());
    }
    let (nonce, sealed) = rest.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| damaged())?;
    let mut sealed = sealed.to_vec();
    let plain =
        cipher(key)?.open_in_place(nonce, Aad::empty(), &mut sealed).map_err(|_| damaged())?;
    Ok(plain.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; KEY_LEN] = [7; KEY_LEN];

    #[test]
    fn keychain_service_is_the_app_identifier() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        assert_eq!(config["identifier"], SERVICE);
    }

    #[test]
    fn sealed_secrets_open_with_the_same_key() {
        let sealed = seal(&KEY, b"sk-secret-key-42", [1; NONCE_LEN]).unwrap();
        assert_eq!(sealed[0], FORMAT);
        assert!(!sealed.windows(16).any(|part| part == b"sk-secret-key-42"), "never in plain text");
        assert_eq!(open(&KEY, &sealed).unwrap(), b"sk-secret-key-42");
    }

    #[test]
    fn another_key_or_a_changed_byte_does_not_open() {
        let sealed = seal(&KEY, b"secret", [2; NONCE_LEN]).unwrap();
        assert!(open(&[8; KEY_LEN], &sealed).is_err());
        let mut changed = sealed.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(open(&KEY, &changed).is_err());
    }

    #[test]
    fn short_or_unknown_data_is_damaged() {
        assert!(open(&KEY, &[]).is_err());
        assert!(open(&KEY, &[FORMAT, 1, 2]).is_err());
        let mut sealed = seal(&KEY, b"secret", [3; NONCE_LEN]).unwrap();
        sealed[0] = 9;
        assert!(open(&KEY, &sealed).is_err());
    }

    /// Creates the app's key in the real login Keychain if it is missing.
    #[test]
    #[ignore = "uses the login Keychain: cargo test -- --ignored keychain"]
    fn keychain_round_trip() {
        let sealed = protect(b"sk-secret").unwrap();
        assert_eq!(unprotect(&sealed).unwrap(), b"sk-secret");
    }
}
