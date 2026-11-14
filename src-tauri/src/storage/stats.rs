//! Dictation statistics without text: per local day, the number of inserted dictations, their
//! words and the recording time. Kept in `stats.sqlite3` whether the history is on or off.
//!
//! The file has no text column, so it cannot hold what was said (a test checks the schema).
//! The day is the local date of the moment the dictation ended, as SQLite's `localtime` gives
//! it. Errors keep SQLite result codes only, like the history's.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use std::{fmt, fs, io};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::migrate_sqlite;
use crate::util::now_ms;

/// Migration `i` upgrades the schema from version `i` to `i + 1`; version 0 is an empty file.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE IF NOT EXISTS days (
        day TEXT PRIMARY KEY,
        dictations INTEGER NOT NULL,
        words INTEGER NOT NULL,
        recording_ms INTEGER NOT NULL
    );
"];
const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Words in the inserted text: runs between spaces with at least one letter or digit, so
/// "42" is a word and a lone dash is not. Chinese and Japanese put no spaces between words:
/// there every character is a word, as word processors count them. Thai, Lao, Khmer and the
/// like, also without spaces, still count by spaces.
pub fn count_words(text: &str) -> u64 {
    text.split_whitespace().map(words_in).sum()
}

/// One run between spaces: each Chinese or Japanese character, and each stretch of other
/// characters with a letter or digit ("iPhone" in "iPhone的价格"); the punctuation of those
/// scripts ends a stretch ("Windows、macOS" is two words).
fn words_in(run: &str) -> u64 {
    let mut words = 0;
    let mut in_word = false;
    for c in run.chars() {
        if is_sound_mark(c) {
            continue;
        }
        if is_cjk(c) {
            words += 1 + u64::from(in_word);
            in_word = false;
        } else if is_cjk_punctuation(c) {
            words += u64::from(in_word);
            in_word = false;
        } else if c.is_alphanumeric() {
            in_word = true;
        }
    }
    words + u64::from(in_word)
}

/// Kana and Han ideographs, with the marks that stand for a character (々, 〇, ゝ, ヽ).
fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{3041}'..='\u{3096}' // hiragana
            | '\u{309D}'..='\u{309F}' // its repeat marks
            | '\u{30A1}'..='\u{30FA}' // katakana
            | '\u{30FC}'..='\u{30FF}' // the long vowel mark, repeat marks
            | '\u{31F0}'..='\u{31FF}' // small katakana
            | '\u{FF66}'..='\u{FF9D}' // half-width katakana
            | '\u{3005}'..='\u{3007}' // 々 〆 〇
            | '\u{303B}' // 〻
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{3134F}'
    )
}

/// The voicing marks of kana: part of the character before them (ﾃﾞ is one).
fn is_sound_mark(c: char) -> bool {
    matches!(c, '\u{3099}'..='\u{309C}' | '\u{FF9E}'..='\u{FF9F}')
}

/// Punctuation of Chinese and Japanese, full-width punctuation too.
fn is_cjk_punctuation(c: char) -> bool {
    matches!(
        c,
        '\u{3000}'..='\u{303F}'
            | '\u{30FB}'
            | '\u{FF01}'..='\u{FF0F}'
            | '\u{FF1A}'..='\u{FF20}'
            | '\u{FF3B}'..='\u{FF40}'
            | '\u{FF5B}'..='\u{FF65}'
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    pub dictations: u64,
    pub words: u64,
    pub recording_ms: u64,
}

/// Today, the last 7 days with today, and everything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub today: Totals,
    pub week: Totals,
    pub total: Totals,
}

#[derive(Debug)]
pub enum StatsError {
    Io(io::Error),
    /// SQLite failed; only its result code is kept.
    Sqlite(String),
    NewerVersion {
        found: u32,
    },
}

impl fmt::Display for StatsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "cannot create the statistics folder: {error}"),
            Self::Sqlite(code) => write!(f, "statistics database error: {code}"),
            Self::NewerVersion { found } => write!(
                f,
                "statistics database has schema version {found}, this app supports up to {SCHEMA_VERSION}"
            ),
        }
    }
}

impl std::error::Error for StatsError {}

impl From<rusqlite::Error> for StatsError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(crate::storage::sqlite_code(&error))
    }
}

pub struct StatsStore {
    connection: Connection,
}

impl StatsStore {
    pub fn open(path: &Path) -> Result<Self, StatsError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(StatsError::Io)?;
        }
        let mut connection = Connection::open(path)?;
        migrate_sqlite(&mut connection, MIGRATIONS, |found| StatsError::NewerVersion { found })?;
        Ok(Self { connection })
    }

    /// The local date (`YYYY-MM-DD`) of a Unix time in milliseconds.
    pub fn local_day(&self, at_ms: i64) -> Result<String, StatsError> {
        let day = self.connection.query_row(
            "SELECT date(?1 / 1000, 'unixepoch', 'localtime')",
            [at_ms],
            |row| row.get(0),
        )?;
        Ok(day)
    }

    pub fn add(&mut self, day: &str, words: u64, recording: Duration) -> Result<(), StatsError> {
        let recording_ms = i64::try_from(recording.as_millis()).unwrap_or(i64::MAX);
        let words = i64::try_from(words).unwrap_or(i64::MAX);
        self.connection.execute(
            "INSERT INTO days (day, dictations, words, recording_ms) VALUES (?1, 1, ?2, ?3)
             ON CONFLICT (day) DO UPDATE SET
                 dictations = dictations + 1,
                 words = words + excluded.words,
                 recording_ms = recording_ms + excluded.recording_ms",
            params![day, words, recording_ms],
        )?;
        Ok(())
    }

    pub fn summary(&self, today: &str) -> Result<Summary, StatsError> {
        let totals = |filter: &str, args: &[&dyn rusqlite::ToSql]| -> Result<Totals, StatsError> {
            let sql = format!(
                "SELECT COALESCE(SUM(dictations), 0), COALESCE(SUM(words), 0),
                        COALESCE(SUM(recording_ms), 0) FROM days {filter}"
            );
            let row = self
                .connection
                .query_row(&sql, args, |row| {
                    Ok(Totals {
                        dictations: row.get::<_, i64>(0)?.max(0) as u64,
                        words: row.get::<_, i64>(1)?.max(0) as u64,
                        recording_ms: row.get::<_, i64>(2)?.max(0) as u64,
                    })
                })
                .optional()?;
            Ok(row.unwrap_or_default())
        };
        Ok(Summary {
            today: totals("WHERE day = ?1", &[&today])?,
            week: totals("WHERE day > date(?1, '-7 days') AND day <= ?1", &[&today])?,
            total: totals("", &[])?,
        })
    }

    pub fn reset(&mut self) -> Result<(), StatsError> {
        self.connection.execute("DELETE FROM days", [])?;
        Ok(())
    }
}

/// The statistics file, opened on first use and shared by the orchestrator and the commands.
/// Reading never creates it: a missing file is all zeros.
pub struct SharedStats {
    path: PathBuf,
    store: Mutex<Option<StatsStore>>,
}

impl SharedStats {
    pub fn new(path: PathBuf) -> Self {
        Self { path, store: Mutex::new(None) }
    }

    /// Counts a dictation that ended now.
    pub fn add(&self, words: u64, recording: Duration) -> Result<(), StatsError> {
        self.with(true, |store| {
            let day = store.local_day(now_ms())?;
            store.add(&day, words, recording)
        })
        .map(drop)
    }

    pub fn summary(&self) -> Result<Summary, StatsError> {
        self.with(false, |store| {
            let today = store.local_day(now_ms())?;
            store.summary(&today)
        })
        .map(Option::unwrap_or_default)
    }

    /// Starts the statistics over. A file that cannot be opened or cleared (damaged, or of a
    /// newer version) is removed: a reset always helps.
    pub fn reset(&self) -> Result<(), StatsError> {
        if self.with(false, StatsStore::reset).is_ok() {
            return Ok(());
        }
        let mut store = self.store.lock().expect("statistics lock");
        // Closed first: Windows does not remove an open file.
        *store = None;
        super::remove_sqlite_files(&self.path).map_err(StatsError::Io)
    }

    fn with<T>(
        &self,
        create: bool,
        action: impl FnOnce(&mut StatsStore) -> Result<T, StatsError>,
    ) -> Result<Option<T>, StatsError> {
        let mut store = self.store.lock().expect("statistics lock");
        if store.is_none() {
            if !create && !self.path.exists() {
                return Ok(None);
            }
            *store = Some(StatsStore::open(&self.path)?);
        }
        action(store.as_mut().expect("opened above")).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn test_path() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("stats-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = fs::remove_dir_all(&dir);
        dir.join("stats.sqlite3")
    }

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn a_damaged_statistics_file_is_reset_all_the_same() {
        let path = test_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not a database").unwrap();
        let stats = SharedStats::new(path.clone());
        assert!(stats.summary().is_err());
        stats.reset().unwrap();
        assert!(!path.exists());
        assert!(stats.summary().is_ok(), "a new start");
    }

    #[test]
    fn counts_words_with_and_without_spaces() {
        assert_eq!(count_words("Привет, как дела?"), 3);
        assert_eq!(count_words("Hello, world. It's e-mail time"), 5);
        assert_eq!(count_words("Встреча в 15:30, 3 человека — 42 минуты"), 7);
        assert_eq!(count_words("  \n "), 0);
        assert_eq!(count_words("— … !"), 0);
        // Chinese and Japanese: a word per character, punctuation not counted.
        assert_eq!(count_words("今日は天気がいいですね。明日も晴れるでしょう。"), 21);
        assert_eq!(count_words("我们明天见"), 5);
        assert_eq!(count_words("iPhone的价格"), 4);
        assert_eq!(count_words("我用iPhone 15"), 4);
        assert_eq!(count_words("コーヒー"), 4);
        // Voicing marks, repeat marks and the punctuation of those scripts.
        assert_eq!(count_words("ﾃﾞｰﾀ"), 3);
        assert_eq!(count_words("ｶﾞｽ"), 2);
        assert_eq!(count_words("支持Windows、macOS和Linux"), 6);
        assert_eq!(count_words("〇〇さん"), 4);
        assert_eq!(count_words("人々iPhone"), 3);
        // Korean puts spaces between words.
        assert_eq!(count_words("안녕하세요 여러분"), 2);
    }

    #[test]
    fn a_day_sums_its_dictations() {
        let mut store = StatsStore::open(&test_path()).unwrap();
        store.add("2026-11-08", 10, secs(6)).unwrap();
        store.add("2026-11-08", 5, secs(4)).unwrap();
        let today = Totals { dictations: 2, words: 15, recording_ms: 10_000 };
        assert_eq!(store.summary("2026-11-08").unwrap().today, today);
    }

    #[test]
    fn a_new_day_starts_from_zero_and_the_week_keeps_seven_days() {
        let mut store = StatsStore::open(&test_path()).unwrap();
        store.add("2026-11-02", 100, secs(60)).unwrap(); // 7 days before: out of the week
        store.add("2026-11-03", 20, secs(10)).unwrap(); // the first of the 7 days
        store.add("2026-11-08", 7, secs(3)).unwrap(); // late on the 8th
        store.add("2026-11-09", 3, secs(2)).unwrap(); // after midnight
        let summary = store.summary("2026-11-09").unwrap();
        assert_eq!(summary.today, Totals { dictations: 1, words: 3, recording_ms: 2_000 });
        assert_eq!(summary.week, Totals { dictations: 3, words: 30, recording_ms: 15_000 });
        assert_eq!(summary.total, Totals { dictations: 4, words: 130, recording_ms: 75_000 });
        assert_eq!(store.summary("2026-11-10").unwrap().today, Totals::default());
    }

    #[test]
    fn numbers_survive_a_restart_and_reset_clears_them() {
        let path = test_path();
        let shared = SharedStats::new(path.clone());
        assert_eq!(shared.summary().unwrap(), Summary::default(), "no file yet");
        assert!(!path.exists(), "reading does not create the file");
        shared.add(4, secs(2)).unwrap();
        drop(shared);
        let reopened = SharedStats::new(path);
        let today = Totals { dictations: 1, words: 4, recording_ms: 2_000 };
        assert_eq!(reopened.summary().unwrap().today, today);
        assert_eq!(reopened.summary().unwrap().total, today);
        reopened.reset().unwrap();
        assert_eq!(reopened.summary().unwrap(), Summary::default());
    }

    /// Only numbers and the date: there is nowhere to put the text of a dictation.
    #[test]
    fn the_file_holds_no_text() {
        let store = StatsStore::open(&test_path()).unwrap();
        let mut statement =
            store.connection.prepare("SELECT name, type FROM pragma_table_info('days')").unwrap();
        let columns: Vec<(String, String)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let expected = [
            ("day", "TEXT"),
            ("dictations", "INTEGER"),
            ("words", "INTEGER"),
            ("recording_ms", "INTEGER"),
        ];
        assert_eq!(
            columns,
            expected.map(|(name, kind)| (name.to_string(), kind.to_string())).to_vec()
        );
        let tables: Vec<String> = store
            .connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(tables, ["days"]);
    }

    #[test]
    fn the_local_day_is_a_date() {
        let store = StatsStore::open(&test_path()).unwrap();
        let day = store.local_day(now_ms()).unwrap();
        assert_eq!(day.len(), 10);
        assert_eq!(&day[4..5], "-");
    }
}
