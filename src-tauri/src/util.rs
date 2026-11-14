//! Small helpers the whole app shares: a line in the log, the time as the stores keep it, hex,
//! base64, random bytes.

use std::io;

/// A line on standard error, which a release build keeps in `logs/app.log`. Unlike `eprintln!`
/// it never panics: a full disk must not take down the thread that only wanted to say so.
macro_rules! log_line {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr(), $($arg)*);
    }};
}
use std::time::{SystemTime, UNIX_EPOCH};

/// Unix time in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Standard base64 with padding (RFC 4648 §4).
pub fn base64(bytes: &[u8]) -> String {
    encode_base64(bytes, BASE64, true)
}

/// Base64url without padding (RFC 4648 §5), as PKCE and JWTs use it.
pub fn base64url(bytes: &[u8]) -> String {
    encode_base64(bytes, BASE64URL, false)
}

fn encode_base64(bytes: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let bits = chunk
            .iter()
            .enumerate()
            .fold(0u32, |bits, (i, byte)| bits | (u32::from(*byte) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            text.push(char::from(alphabet[(bits >> (18 - 6 * i)) as usize & 63]));
        }
        if pad {
            text.extend(std::iter::repeat_n('=', 3 - chunk.len()));
        }
    }
    text
}

/// Base64url, with or without padding; `None` for any other character.
pub fn from_base64url(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0);
    for byte in text.bytes() {
        let value = BASE64URL.iter().position(|c| *c == byte)? as u32;
        bits = bits << 6 | value;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    (count < 6).then_some(out)
}

/// `N` random bytes from the OS.
pub fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(bytes)
}

/// 16 random bytes as 32 hex characters: request paths, keys and ids nobody can guess.
pub fn random_hex() -> io::Result<String> {
    Ok(hex(&random_bytes::<16>()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_with_two_digits_a_byte() {
        assert_eq!(hex(&[0, 15, 171, 255]), "000fabff");
    }

    #[test]
    fn base64_matches_the_standard() {
        let cases =
            [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")];
        for (plain, encoded) in cases {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain:?}");
        }
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }

    #[test]
    fn base64url_round_trips_without_padding() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        for text in [&b""[..], b"f", b"fo", b"foo", b"foob", &[0xfb, 0xff, 0x00]] {
            assert_eq!(from_base64url(&base64url(text)).as_deref(), Some(text));
        }
        assert_eq!(from_base64url("Zg=="), Some(b"f".to_vec()));
        assert_eq!(from_base64url("Z!"), None);
    }

    /// A line in the log does not panic when standard error cannot be written (a full disk
    /// under `logs/app.log`), where `eprintln!` does. In a process of its own whose standard
    /// error is a pipe nobody reads.
    #[test]
    fn a_line_in_the_log_survives_a_standard_error_that_fails() {
        if let Some(how) = std::env::var_os("AHEM_TEST_FAILING_STDERR") {
            if how == "eprintln" {
                #[allow(clippy::disallowed_macros)]
                {
                    eprintln!("lost");
                }
            } else {
                log_line!("lost");
            }
            return;
        }
        let run = |how: &str| {
            let (reader, writer) = std::io::pipe().unwrap();
            drop(reader);
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "util::tests::a_line_in_the_log_survives_a_standard_error_that_fails",
                    "--exact",
                    "--nocapture",
                ])
                .env("AHEM_TEST_FAILING_STDERR", how)
                .stdout(std::process::Stdio::null())
                .stderr(writer)
                .status()
                .unwrap()
        };
        assert!(run("log_line").success());
        assert!(!run("eprintln").success(), "the test reaches a failing standard error");
    }

    #[test]
    fn random_hex_is_new_each_time() {
        let a = random_hex().unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, random_hex().unwrap());
    }
}
