//! Optional local history of dictations in SQLite (compiled into the app, no system SQLite).
//!
//! History is off by default; the caller opens the store only when it is on and adds only
//! finished dictations, not selected-text rewrites or files. Entries are ordered by
//! insertion (`id`), not by `created_at_ms`, so a clock change neither reorders them nor
//! trims the wrong ones. Ids are never reused, even after `clear`.
//!
//! Deleted text does not stay in the file: `secure_delete` overwrites deleted content with
//! zeros on every delete, and `clear` also runs `VACUUM`, which rebuilds and shrinks the file.
//! Temporary tables and sorts stay in memory. The rollback journal (`<file>-journal`) holds
//! the old pages of a write only until it commits and is then deleted.
//!
//! The schema version is `PRAGMA user_version`, upgraded step by step in one transaction.
//! A file written by a newer version is refused and left untouched. Errors keep SQLite
//! result codes only, never messages, so they cannot quote stored text.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::{fmt, fs, io};

use rusqlite::{Connection, Row, params};
use serde::Serialize;

/// Migration `i` upgrades the schema from version `i` to `i + 1`; version 0 is an empty file.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE entries (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        created_at_ms INTEGER NOT NULL,
        mode_id TEXT NOT NULL,
        language TEXT,
        transcript TEXT NOT NULL,
        processed TEXT,
        inserted INTEGER NOT NULL CHECK (inserted IN (0, 1))
    );
"];

pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// How many entries the history keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// The newest `n` entries; `Last(0)` keeps none.
    Last(u32),
    Unlimited,
}

/// A finished dictation: the fields of `DictationResult` plus the time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEntry {
    /// Unix time in milliseconds.
    pub created_at_ms: i64,
    pub mode_id: String,
    pub language: Option<String>,
    pub transcript: String,
    /// Language-model output; `None` without processing or when it failed.
    pub processed: Option<String>,
    pub inserted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub id: i64,
    pub created_at_ms: i64,
    pub mode_id: String,
    pub language: Option<String>,
    pub transcript: String,
    pub processed: Option<String>,
    pub inserted: bool,
}

#[derive(Debug)]
pub enum HistoryError {
    /// The folder for the database could not be created.
    Io(io::Error),
    /// The history file could not be deleted.
    Remove(io::Error),
    /// SQLite failed; only its result code is kept.
    Sqlite(rusqlite::ffi::Error),
    /// A stored value has an unexpected type, or the path cannot be used; says which, never the value.
    Unexpected(&'static str),
    /// Written by a newer version of the app; the file is left untouched.
    NewerVersion { found: u32 },
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "cannot create the history folder: {error}"),
            Self::Remove(error) => write!(f, "cannot delete the history file: {error}"),
            Self::Sqlite(error) => write!(f, "history database error: {error}"),
            Self::Unexpected(what) => write!(f, "history database error: {what}"),
            Self::NewerVersion { found } => write!(
                f,
                "history database has schema version {found}, this app supports up to {SCHEMA_VERSION}"
            ),
        }
    }
}

impl std::error::Error for HistoryError {}

impl From<rusqlite::Error> for HistoryError {
    /// Drops SQLite's messages, the SQL text and conversion details: any of them could quote data.
    fn from(error: rusqlite::Error) -> Self {
        use rusqlite::Error as E;
        match error {
            E::SqliteFailure(error, _) | E::SqlInputError { error, .. } => Self::Sqlite(error),
            E::FromSqlConversionFailure(..)
            | E::IntegralValueOutOfRange(..)
            | E::InvalidColumnType(..)
            | E::Utf8Error(..) => Self::Unexpected("unexpected value in the database"),
            E::InvalidPath(_) | E::NulError(_) => Self::Unexpected("unusable database path"),
            _ => Self::Unexpected("unexpected database error"),
        }
    }
}

/// One connection; `Send` but not `Sync`, so shared use needs a `Mutex`.
#[derive(Debug)]
pub struct HistoryStore {
    connection: Connection,
}

impl HistoryStore {
    /// Opens or creates the database, creating missing folders, and upgrades its schema.
    pub fn open(path: &Path) -> Result<Self, HistoryError> {
        open_with(path, MIGRATIONS)
    }

    /// Adds the entry and trims the history to `retention` in one transaction; returns its id.
    pub fn add(&mut self, entry: NewEntry, retention: Retention) -> Result<i64, HistoryError> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO entries (created_at_ms, mode_id, language, transcript, processed, inserted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.created_at_ms,
                entry.mode_id,
                entry.language,
                entry.transcript,
                entry.processed,
                entry.inserted
            ],
        )?;
        let id = transaction.last_insert_rowid();
        trim_rows(&transaction, retention)?;
        transaction.commit()?;
        Ok(id)
    }

    /// The newest entries first; at most `limit` of them (the quick panel shows a few).
    pub fn list(&self, limit: Option<usize>) -> Result<Vec<Entry>, HistoryError> {
        let mut statement = self.connection.prepare(
            "SELECT id, created_at_ms, mode_id, language, transcript, processed, inserted
             FROM entries ORDER BY id DESC LIMIT ?1",
        )?;
        // SQLite: a negative limit is none.
        let limit = limit.map_or(-1, |limit| i64::try_from(limit).unwrap_or(i64::MAX));
        let entries = statement.query_map([limit], entry_from_row)?.collect::<Result<_, _>>()?;
        Ok(entries)
    }

    /// An id that is not there is not an error: the entry is gone either way.
    pub fn delete(&mut self, id: i64) -> Result<(), HistoryError> {
        self.connection.execute("DELETE FROM entries WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Deletes every entry, then `VACUUM` rebuilds the file without free pages.
    /// If `VACUUM` fails, the entries are already deleted and their text zeroed.
    pub fn clear(&mut self) -> Result<(), HistoryError> {
        self.connection.execute("DELETE FROM entries", [])?;
        self.connection.execute_batch("VACUUM")?;
        Ok(())
    }

    /// Applies a new retention to the stored entries, e.g. after the setting changed.
    pub fn trim(&mut self, retention: Retention) -> Result<(), HistoryError> {
        trim_rows(&self.connection, retention)?;
        Ok(())
    }
}

impl Retention {
    /// `history_limit` of the settings.
    pub fn from_limit(limit: Option<u32>) -> Self {
        limit.map_or(Self::Unlimited, Self::Last)
    }
}

/// The history file, opened on first use and shared by the orchestrator and the commands.
/// Reading or deleting never creates the file: a missing file is an empty history.
pub struct SharedHistory {
    path: PathBuf,
    store: Mutex<Option<HistoryStore>>,
}

impl SharedHistory {
    pub fn new(path: PathBuf) -> Self {
        Self { path, store: Mutex::new(None) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether there is a file to show or delete: none before the first saved dictation and
    /// after the file is deleted.
    pub fn file_exists(&self) -> bool {
        self.path.exists()
    }

    pub fn add(&self, entry: NewEntry, retention: Retention) -> Result<i64, HistoryError> {
        self.with(true, |store| store.add(entry, retention)).map(Option::unwrap_or_default)
    }

    pub fn list(&self, limit: Option<usize>) -> Result<Vec<Entry>, HistoryError> {
        self.with(false, |store| store.list(limit)).map(Option::unwrap_or_default)
    }

    pub fn delete(&self, id: i64) -> Result<(), HistoryError> {
        self.with(false, |store| store.delete(id)).map(drop)
    }

    pub fn clear(&self) -> Result<(), HistoryError> {
        self.with(false, HistoryStore::clear).map(drop)
    }

    pub fn trim(&self, retention: Retention) -> Result<(), HistoryError> {
        self.with(false, |store| store.trim(retention)).map(drop)
    }

    /// Closes the file and deletes it with SQLite's side files; the next dictation saved to
    /// the history creates a new one.
    pub fn remove_file(&self) -> Result<(), HistoryError> {
        let mut store = self.store.lock().expect("history lock");
        *store = None;
        super::remove_sqlite_files(&self.path).map_err(HistoryError::Remove)
    }

    /// `None` when the file does not exist and `create` is false.
    fn with<T>(
        &self,
        create: bool,
        action: impl FnOnce(&mut HistoryStore) -> Result<T, HistoryError>,
    ) -> Result<Option<T>, HistoryError> {
        let mut store = self.store.lock().expect("history lock");
        if store.is_none() {
            if !create && !self.path.exists() {
                return Ok(None);
            }
            *store = Some(HistoryStore::open(&self.path)?);
        }
        action(store.as_mut().expect("opened above")).map(Some)
    }
}

fn open_with(path: &Path, migrations: &[&str]) -> Result<HistoryStore, HistoryError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(HistoryError::Io)?;
    }
    let mut connection = Connection::open(path)?;
    // Connection settings, not stored in the file.
    connection.execute_batch("PRAGMA secure_delete = ON; PRAGMA temp_store = MEMORY;")?;
    super::migrate_sqlite(&mut connection, migrations, |found| HistoryError::NewerVersion {
        found,
    })?;
    Ok(HistoryStore { connection })
}

fn trim_rows(connection: &Connection, retention: Retention) -> rusqlite::Result<()> {
    if let Retention::Last(count) = retention {
        connection.execute(
            "DELETE FROM entries WHERE id NOT IN (SELECT id FROM entries ORDER BY id DESC LIMIT ?1)",
            [count],
        )?;
    }
    Ok(())
}

fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        id: row.get(0)?,
        created_at_ms: row.get(1)?,
        mode_id: row.get(2)?,
        language: row.get(3)?,
        transcript: row.get(4)?,
        processed: row.get(5)?,
        inserted: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use rusqlite::{ErrorCode, ffi};
    use serde_json::json;

    use super::*;
    use crate::storage::user_version;

    const SECRET: &str = "secret dictation text";

    /// A fresh directory inside the project's target folder, never the user's data.
    fn test_dir() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("test-artifacts").join(
            format!("history-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)),
        );
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn db_path() -> PathBuf {
        test_dir().join("history.sqlite3")
    }

    fn entry(transcript: &str) -> NewEntry {
        NewEntry {
            created_at_ms: 1_700_000_000_000,
            mode_id: "plain".into(),
            language: Some("ru".into()),
            transcript: transcript.into(),
            processed: None,
            inserted: true,
        }
    }

    fn transcripts(store: &HistoryStore) -> Vec<String> {
        store.list(None).unwrap().into_iter().map(|entry| entry.transcript).collect()
    }

    /// The newest `count` of the transcripts "0", "1", … that were added in order.
    fn newest(added: u32, count: u32) -> Vec<String> {
        (added.saturating_sub(count)..added).rev().map(|i| i.to_string()).collect()
    }

    fn in_file(path: &Path, text: &str) -> bool {
        let bytes = fs::read(path).unwrap();
        bytes.windows(text.len()).any(|window| window == text.as_bytes())
    }

    /// Text long enough to spill into overflow pages, with numbered markers along it.
    fn long_text(tag: &str) -> String {
        (0..2000).map(|i| format!("{tag}{i:04};")).collect()
    }

    fn markers(tag: &str) -> [String; 3] {
        [format!("{tag}0000;"), format!("{tag}1000;"), format!("{tag}1999;")]
    }

    fn file_version(path: &Path) -> u32 {
        user_version(&Connection::open(path).unwrap()).unwrap()
    }

    #[test]
    fn new_file_gets_the_current_schema() {
        let path = test_dir().join("nested").join("history.sqlite3");
        let store = HistoryStore::open(&path).expect("opened");
        assert!(store.list(None).unwrap().is_empty());
        let pragma = |name| {
            store.connection.pragma_query_value(None, name, |row| row.get::<_, i64>(0)).unwrap()
        };
        assert_eq!(pragma("secure_delete"), 1);
        assert_eq!(pragma("temp_store"), 2, "memory");
        drop(store);
        assert_eq!(file_version(&path), SCHEMA_VERSION);
    }

    #[test]
    fn entries_come_back_newest_first() {
        let path = db_path();
        let mut store = HistoryStore::open(&path).unwrap();
        let processed = NewEntry {
            created_at_ms: 1_700_000_005_000,
            mode_id: "polish".into(),
            language: None,
            transcript: "raw".into(),
            processed: Some("clean".into()),
            inserted: false,
        };
        // The clock went back before the third dictation: order still follows insertion.
        let late = NewEntry { created_at_ms: 1_600_000_000_000, ..entry("third") };
        let ids = [
            store.add(entry("first"), Retention::Unlimited).unwrap(),
            store.add(processed.clone(), Retention::Unlimited).unwrap(),
            store.add(late.clone(), Retention::Unlimited).unwrap(),
        ];
        assert!(ids[0] < ids[1] && ids[1] < ids[2]);

        let expected = |id, new: NewEntry| Entry {
            id,
            created_at_ms: new.created_at_ms,
            mode_id: new.mode_id,
            language: new.language,
            transcript: new.transcript,
            processed: new.processed,
            inserted: new.inserted,
        };
        let all = vec![
            expected(ids[2], late),
            expected(ids[1], processed),
            expected(ids[0], entry("first")),
        ];
        assert_eq!(store.list(None).unwrap(), all);
        assert_eq!(store.list(Some(2)).unwrap(), all[..2], "the newest, at most two");
        drop(store);
        assert_eq!(HistoryStore::open(&path).unwrap().list(None).unwrap(), all);
    }

    #[test]
    fn retention_keeps_the_newest_entries() {
        for (retention, keep) in [
            (Retention::Last(3), 3),
            (Retention::Last(4), 4),
            (Retention::Last(5), 5),
            (Retention::Last(10), 10),
            (Retention::Unlimited, 12),
        ] {
            let mut store = HistoryStore::open(&db_path()).unwrap();
            for i in 0..12 {
                store.add(entry(&i.to_string()), retention).unwrap();
                assert_eq!(transcripts(&store), newest(i + 1, keep), "{retention:?} after {i}");
            }
        }
    }

    #[test]
    fn trim_applies_a_new_retention() {
        let mut store = HistoryStore::open(&db_path()).unwrap();
        for i in 0..12 {
            store.add(entry(&i.to_string()), Retention::Unlimited).unwrap();
        }
        store.trim(Retention::Unlimited).unwrap();
        assert_eq!(transcripts(&store), newest(12, 12));
        store.trim(Retention::Last(10)).unwrap();
        assert_eq!(transcripts(&store), newest(12, 10));
        store.trim(Retention::Last(3)).unwrap();
        assert_eq!(transcripts(&store), newest(12, 3));
        store.trim(Retention::Last(0)).unwrap();
        assert!(transcripts(&store).is_empty());
    }

    #[test]
    fn delete_removes_one_entry_and_its_text() {
        let path = db_path();
        let mut store = HistoryStore::open(&path).unwrap();
        store.add(entry("keep-first"), Retention::Unlimited).unwrap();
        let gone = NewEntry { processed: Some(long_text("done")), ..entry(&long_text("gone")) };
        let id = store.add(gone, Retention::Unlimited).unwrap();
        store.add(entry("keep-last"), Retention::Unlimited).unwrap();
        for marker in markers("gone").iter().chain(&markers("done")) {
            assert!(in_file(&path, marker), "{marker} is written before the delete");
        }

        store.delete(id).unwrap();
        assert_eq!(transcripts(&store), ["keep-last", "keep-first"]);
        store.delete(id).unwrap();
        store.delete(9999).unwrap();
        assert_eq!(transcripts(&store), ["keep-last", "keep-first"]);
        drop(store);

        for marker in markers("gone").iter().chain(&markers("done")) {
            assert!(!in_file(&path, marker), "{marker} stayed in the file");
        }
        assert!(in_file(&path, "keep-first") && in_file(&path, "keep-last"));
    }

    #[test]
    fn clear_removes_all_text_from_the_file() {
        let path = db_path();
        let mut store = HistoryStore::open(&path).unwrap();
        let cyrillic = "секретная диктовка";
        store.add(entry(SECRET), Retention::Unlimited).unwrap();
        let processed = NewEntry { processed: Some(cyrillic.into()), ..entry(&long_text("long")) };
        let last_id = store.add(processed, Retention::Unlimited).unwrap();
        let before = fs::metadata(&path).unwrap().len();
        assert!(in_file(&path, SECRET) && in_file(&path, cyrillic));

        store.clear().unwrap();
        assert!(store.list(None).unwrap().is_empty());
        for text in [SECRET, cyrillic].into_iter().chain(markers("long").iter().map(String::as_str))
        {
            assert!(!in_file(&path, text), "{text} stayed in the file");
        }
        assert!(fs::metadata(&path).unwrap().len() < before, "VACUUM shrinks the file");

        let next_id = store.add(entry("after"), Retention::Unlimited).unwrap();
        assert!(next_id > last_id, "ids are not reused");
        assert_eq!(transcripts(&store), ["after"]);
    }

    #[test]
    fn unicode_text_round_trips() {
        let path = db_path();
        let texts = [
            "Привет, мир! Ёлка и щётка.",
            "emoji 👩‍💻🎙️ and flags 🇷🇺🇬🇧",
            "中文语音识别，日本語のテキスト",
            "مرحبا بالعالم",
            "combining: e\u{301} a\u{308} — and “quotes”",
            "lines\nand\ttabs\r\nend",
            "'; DROP TABLE entries; --",
        ];
        let mut store = HistoryStore::open(&path).unwrap();
        for text in texts {
            let new = NewEntry { processed: Some(format!("→ {text}")), ..entry(text) };
            store.add(new, Retention::Unlimited).unwrap();
        }
        drop(store);
        let entries = HistoryStore::open(&path).unwrap().list(None).unwrap();
        let stored: Vec<_> = entries.iter().rev().map(|entry| entry.transcript.as_str()).collect();
        assert_eq!(stored, texts);
        for entry in &entries {
            assert_eq!(entry.processed, Some(format!("→ {}", entry.transcript)));
        }
    }

    #[test]
    fn older_schema_is_upgraded() {
        const ADD_NOTE: &str = "ALTER TABLE entries ADD COLUMN note TEXT NOT NULL DEFAULT '';";
        let path = db_path();
        let mut v1 = open_with(&path, &MIGRATIONS[..1]).unwrap();
        v1.add(entry("kept"), Retention::Unlimited).unwrap();
        drop(v1);
        assert_eq!(file_version(&path), 1);

        let v2 = open_with(&path, &[MIGRATIONS[0], ADD_NOTE]).unwrap();
        assert_eq!(transcripts(&v2), ["kept"]);
        let note: String =
            v2.connection.query_row("SELECT note FROM entries", [], |row| row.get(0)).unwrap();
        assert_eq!(note, "");
        drop(v2);
        assert_eq!(file_version(&path), 2);
    }

    #[test]
    fn newer_schema_is_refused_untouched() {
        let path = db_path();
        HistoryStore::open(&path).unwrap().add(entry("kept"), Retention::Unlimited).unwrap();
        let newer = SCHEMA_VERSION + 1;
        Connection::open(&path).unwrap().pragma_update(None, "user_version", newer).unwrap();
        let bytes = fs::read(&path).unwrap();

        let error = HistoryStore::open(&path).unwrap_err();
        assert!(matches!(error, HistoryError::NewerVersion { found } if found == newer));
        assert!(error.to_string().contains(&newer.to_string()));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!path.with_extension("sqlite3-journal").exists());
    }

    #[test]
    fn not_a_database_is_refused_untouched() {
        let path = db_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not a database }").unwrap();
        let error = HistoryStore::open(&path).unwrap_err();
        assert!(
            matches!(error, HistoryError::Sqlite(error) if error.code == ErrorCode::NotADatabase),
            "{error:?}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ not a database }");
    }

    #[test]
    fn errors_never_quote_stored_text() {
        let errors = [
            rusqlite::Error::SqliteFailure(
                ffi::Error::new(ffi::SQLITE_CONSTRAINT_CHECK),
                Some(format!("CHECK constraint failed: {SECRET}")),
            ),
            rusqlite::Error::SqlInputError {
                error: ffi::Error::new(ffi::SQLITE_ERROR),
                msg: SECRET.into(),
                sql: SECRET.into(),
                offset: 0,
            },
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                format!("bad value {SECRET}").into(),
            ),
            rusqlite::Error::InvalidColumnType(4, SECRET.into(), rusqlite::types::Type::Text),
        ];
        for error in errors {
            let error = HistoryError::from(error);
            assert!(!error.to_string().contains(SECRET), "{error}");
            assert!(!format!("{error:?}").contains(SECRET), "{error:?}");
        }
        let error = HistoryError::from(rusqlite::Error::SqliteFailure(
            ffi::Error::new(ffi::SQLITE_CONSTRAINT_CHECK),
            None,
        ));
        assert!(
            matches!(error, HistoryError::Sqlite(error) if error.code == ErrorCode::ConstraintViolation)
        );
    }

    #[test]
    fn shared_history_removes_its_file_and_starts_anew() {
        let path = db_path();
        let history = SharedHistory::new(path.clone());
        assert!(!history.file_exists());
        history.remove_file().unwrap();
        assert!(!path.exists(), "removing a missing file is fine");

        history.add(entry(SECRET), Retention::from_limit(None)).unwrap();
        assert!(history.file_exists());
        history.remove_file().unwrap();
        assert!(!history.file_exists());
        assert!(history.list(None).unwrap().is_empty());
        assert!(!history.file_exists(), "reading after the removal does not create the file");

        history.add(entry("again"), Retention::from_limit(None)).unwrap();
        let kept: Vec<_> = history.list(None).unwrap().into_iter().map(|e| e.transcript).collect();
        assert_eq!(kept, vec!["again".to_string()]);
    }

    #[test]
    fn shared_history_creates_the_file_only_to_add() {
        let path = db_path();
        let history = SharedHistory::new(path.clone());
        assert!(history.list(None).unwrap().is_empty());
        history.delete(1).unwrap();
        history.clear().unwrap();
        history.trim(Retention::Last(3)).unwrap();
        assert!(!path.exists(), "reading and deleting do not create the file");

        for i in 0..4 {
            history.add(entry(&i.to_string()), Retention::from_limit(None)).unwrap();
        }
        history.trim(Retention::from_limit(Some(3))).unwrap();
        let kept: Vec<_> = history.list(None).unwrap().into_iter().map(|e| e.transcript).collect();
        assert_eq!(kept, newest(4, 3));
        history.clear().unwrap();
        assert!(history.list(None).unwrap().is_empty());
    }

    #[test]
    fn entry_json_matches_the_frontend_type() {
        let entry = Entry {
            id: 7,
            created_at_ms: 1_700_000_000_123,
            mode_id: "polish".into(),
            language: Some("ru".into()),
            transcript: "сырой текст".into(),
            processed: None,
            inserted: false,
        };
        assert_eq!(
            serde_json::to_value(entry).unwrap(),
            json!({
                "id": 7,
                "created_at_ms": 1_700_000_000_123_i64,
                "mode_id": "polish",
                "language": "ru",
                "transcript": "сырой текст",
                "processed": null,
                "inserted": false
            })
        );
    }
}
