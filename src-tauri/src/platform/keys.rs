//! Text as a sequence of key strokes, independent of the OS.

// Typing into other programs exists on Windows and macOS only.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// One UTF-16 code unit typed as Unicode input; surrogate pairs come as two units.
    Unicode(u16),
    /// A line break is Shift+Enter, not U+000A: many editors ignore a typed line feed, and in
    /// chats a plain Enter sends the message.
    Enter,
}

#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn key_plan(text: &str) -> Vec<Key> {
    let mut keys = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                keys.push(Key::Enter);
            }
            '\n' => keys.push(Key::Enter),
            c => {
                let mut units = [0u16; 2];
                keys.extend(c.encode_utf16(&mut units).iter().map(|unit| Key::Unicode(*unit)));
            }
        }
    }
    keys
}

/// How many bytes of `text` the first `sent` keys of its [`key_plan`] typed: whole characters
/// only, a line break being one key.
// Insertion that can stop partway exists on Windows and macOS only.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub fn typed_len(text: &str, sent: usize) -> usize {
    let mut keys = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let mut end = at + c.len_utf8();
        let needs = match c {
            '\r' => {
                if let Some((next, '\n')) = chars.peek().copied() {
                    chars.next();
                    end = next + 1;
                }
                1
            }
            '\n' => 1,
            c => c.len_utf16(),
        };
        if keys + needs > sent {
            return at;
        }
        keys += needs;
        if keys == sent {
            return end;
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<Key> {
        text.encode_utf16().map(Key::Unicode).collect()
    }

    #[test]
    fn letters_are_unicode_units() {
        assert_eq!(key_plan("Привет"), units("Привет"));
    }

    #[test]
    fn line_breaks_are_enter() {
        let mut expected = units("a");
        expected.push(Key::Enter);
        expected.extend(units("b"));
        expected.push(Key::Enter);
        expected.extend(units("c"));
        expected.push(Key::Enter);
        assert_eq!(key_plan("a\nb\r\nc\r"), expected);
    }

    #[test]
    fn emoji_is_a_surrogate_pair() {
        assert_eq!(key_plan("😀"), vec![Key::Unicode(0xD83D), Key::Unicode(0xDE00)]);
    }

    #[test]
    fn the_typed_part_counts_whole_characters() {
        let text = "Да\r\nок😀!";
        assert_eq!(typed_len(text, 0), 0);
        assert_eq!(&text[..typed_len(text, 2)], "Да");
        assert_eq!(&text[..typed_len(text, 3)], "Да\r\n", "one key for a line break");
        // Half of a surrogate pair is not a typed character.
        assert_eq!(&text[..typed_len(text, 6)], "Да\r\nок");
        assert_eq!(&text[..typed_len(text, 7)], "Да\r\nок😀");
        assert_eq!(typed_len(text, key_plan(text).len()), text.len());
        assert_eq!(typed_len(text, 100), text.len());
    }
}
