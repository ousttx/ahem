//! The list of transcripts in `meetings.sqlite3`: meetings, with each one's audio in its own
//! folder `meetings/<date and time it started>/`, and files, whose audio stays where
//! the user keeps it. Kept until the user deletes them, whatever the history
//! settings. Errors keep SQLite result codes only, like the history's.
//!
//! Deleted text does not stay in the file, as in the history: `secure_delete` overwrites what a
//! delete or an update frees with zeros, and temporary tables and sorts stay in memory.
//!
//! The schema version is `PRAGMA user_version`, upgraded step by step in one transaction.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::{fmt, fs, io};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::transcript::Turn;
use crate::storage::migrate_sqlite;

/// Migration `i` upgrades the schema from version `i` to `i + 1`; version 0 is an empty file.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS meetings (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        started_ms INTEGER NOT NULL,
        duration_ms INTEGER NOT NULL DEFAULT 0,
        title TEXT,
        state TEXT NOT NULL,
        problem TEXT,
        turns TEXT,
        preview TEXT
    );",
    // File transcripts in the same list.
    "ALTER TABLE meetings ADD COLUMN kind TEXT NOT NULL DEFAULT 'meeting';
     ALTER TABLE meetings ADD COLUMN source TEXT;
     ALTER TABLE meetings ADD COLUMN output TEXT;
     ALTER TABLE meetings ADD COLUMN processed TEXT;",
    // The name of a meeting's folder: when it started. Older meetings have their number; their
    // folders are renamed when the store opens.
    "ALTER TABLE meetings ADD COLUMN folder TEXT;",
];
const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;
/// The columns of a `MeetingSummary`, with its state as `{state}` (`MeetingStore::summary`).
const SUMMARY: &str =
    "id, started_ms, duration_ms, title, {state}, problem, preview, kind, source, output";
/// The state of a meeting nobody works on: done while it has a transcript (one transcribed
/// again and stopped keeps the old one), else recorded.
const RESTED: &str = "CASE WHEN turns IS NOT NULL THEN 'done' ELSE 'recorded' END";
pub const ME_FILE: &str = "me.opus";
pub const OTHERS_FILE: &str = "others.opus";
/// Both tracks in one, to listen to the call as it was.
pub const CALL_FILE: &str = "call.opus";
/// Characters of the transcript shown in the list.
const PREVIEW_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingState {
    Recording,
    /// Recorded, not transcribed (or the transcription was stopped).
    Recorded,
    Transcribing,
    Done,
    Failed,
}

impl MeetingState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Recorded => "recorded",
            Self::Transcribing => "transcribing",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "recording" => Self::Recording,
            "transcribing" => Self::Transcribing,
            "done" => Self::Done,
            "failed" => Self::Failed,
            _ => Self::Recorded,
        }
    }
}

/// What an entry of the list is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A call recorded by the app, with its audio in its folder.
    Meeting,
    /// The user's audio or video file, transcribed on the Files view.
    File,
}

impl Kind {
    fn parse(text: &str) -> Self {
        match text {
            "file" => Self::File,
            _ => Self::Meeting,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeetingSummary {
    pub id: i64,
    /// When the meeting started, or when the file was transcribed.
    pub started_ms: i64,
    pub duration_ms: i64,
    pub title: Option<String>,
    pub state: MeetingState,
    /// Why the transcription failed; never quotes the transcript.
    pub problem: Option<String>,
    pub preview: Option<String>,
    pub kind: Kind,
    /// A file's path; none for a meeting.
    pub source: Option<String>,
    /// Where a file's result was saved; none for a meeting.
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Meeting {
    #[serde(flatten)]
    pub summary: MeetingSummary,
    pub turns: Vec<Turn>,
    /// The text of the file's mode with a language model.
    pub processed: Option<String>,
}

/// A transcribed file to keep in the list; its audio is not copied.
#[derive(Debug, Clone, PartialEq)]
pub struct FileTranscript {
    pub source: PathBuf,
    /// The file the result was written to; none when it could not be written.
    pub output: Option<PathBuf>,
    pub duration_ms: i64,
    pub turns: Vec<Turn>,
    pub processed: Option<String>,
}

#[derive(Debug)]
pub enum MeetingsError {
    Io(io::Error),
    Sqlite(String),
    NewerVersion { found: u32 },
    NotFound,
}

impl fmt::Display for MeetingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "meeting files: {error}"),
            Self::Sqlite(code) => write!(f, "meetings database error: {code}"),
            Self::NewerVersion { found } => write!(
                f,
                "meetings database has schema version {found}, this app supports up to {SCHEMA_VERSION}"
            ),
            Self::NotFound => write!(f, "no such meeting"),
        }
    }
}

impl std::error::Error for MeetingsError {}

impl From<rusqlite::Error> for MeetingsError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(crate::storage::sqlite_code(&error))
    }
}

impl From<io::Error> for MeetingsError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct MeetingStore {
    connection: Connection,
    folder: PathBuf,
    /// Meetings left going that could not rest (a full disk at the open, or when their work
    /// ended): shown resting, as they will be at a later open, until one is worked on again.
    unrested: Vec<i64>,
}

/// Freed pages stay in the file (zeroed by `secure_delete`, but a crash may leave old text in
/// them): the file is rebuilt without them at the first open after a delete. Only if it can be:
/// on a nearly full disk the rebuild has no room, and the meetings must still open, to be
/// deleted to make room.
fn compact(connection: &Connection) {
    let free = connection.pragma_query_value(None, "freelist_count", |row| row.get::<_, i64>(0));
    if free.is_ok_and(|free| free > 0)
        && let Err(error) = connection.execute_batch("VACUUM")
    {
        log_line!("cannot compact the meetings database: {error}");
    }
}

/// Meetings left recording or transcribing by a crash or an exit rest (`RESTED`). Only if the
/// file can be written: on a full disk the meetings must still open, to be deleted to make room;
/// they rest at a later open. Not again while the app runs: a meeting being recorded would rest.
/// Returns the meetings that could not rest.
fn rest(connection: &Connection) -> Vec<i64> {
    let sql = format!(
        "UPDATE meetings SET state = {RESTED} WHERE state IN ('recording', 'transcribing')"
    );
    let Err(error) = connection.execute(&sql, []) else {
        return Vec::new();
    };
    log_line!("cannot put the meetings left going to rest: {error}");
    connection
        .prepare("SELECT id FROM meetings WHERE state IN ('recording', 'transcribing')")
        .and_then(|mut statement| statement.query_map([], |row| row.get(0))?.collect())
        .unwrap_or_default()
}

impl MeetingStore {
    /// Opens the database; meetings left recording or transcribing by a crash or an exit
    /// rest (`RESTED`: done with a transcript, else recorded), their audio is kept.
    pub fn open(path: &Path, folder: PathBuf) -> Result<Self, MeetingsError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut connection = Connection::open(path)?;
        // Connection settings, not stored in the file.
        connection.execute_batch("PRAGMA secure_delete = ON; PRAGMA temp_store = MEMORY;")?;
        migrate_sqlite(&mut connection, MIGRATIONS, |found| MeetingsError::NewerVersion { found })?;
        compact(&connection);
        let unrested = rest(&connection);
        let mut store = Self { connection, folder, unrested };
        if let Err(error) = store.name_numbered_folders() {
            log_line!("cannot name the meeting folders: {error}");
        }
        Ok(store)
    }

    /// The audio folder of a meeting: named by when it started, or by its number if older.
    /// A stored name that is not one plain folder name (a damaged or edited file) is not
    /// trusted: the number is used, so nothing outside the meetings folder is ever named.
    pub fn folder(&self, id: i64) -> Result<PathBuf, MeetingsError> {
        let name: Option<String> = self
            .connection
            .query_row("SELECT folder FROM meetings WHERE id = ?1", [id], |row| row.get(0))
            .optional()?
            .flatten();
        let name = name.filter(|name| plain_name(name)).unwrap_or_else(|| id.to_string());
        Ok(self.folder.join(name))
    }

    /// A transcription that was stopped: the meeting is as it was before it.
    pub fn rest(&mut self, id: i64) -> Result<(), MeetingsError> {
        let written = self.update(
            &format!("UPDATE meetings SET state = {RESTED}, problem = NULL WHERE id = ?1"),
            [id],
        );
        self.ended(id, written)
    }

    /// A new meeting being recorded, with its empty folder named by when it started.
    pub fn create(&mut self, started_ms: i64) -> Result<i64, MeetingsError> {
        fs::create_dir_all(&self.folder)?;
        let name = self.free_name(&folder_name(started_ms))?;
        fs::create_dir(self.folder.join(&name))?;
        let inserted = self.connection.execute(
            "INSERT INTO meetings (started_ms, state, folder) VALUES (?1, 'recording', ?2)",
            params![started_ms, name],
        );
        if let Err(error) = inserted {
            let _ = fs::remove_dir_all(self.folder.join(&name));
            return Err(error.into());
        }
        Ok(self.connection.last_insert_rowid())
    }

    /// `name`, or `name (2)`, `name (3)` … where a folder of that name is already there or
    /// belongs to a meeting (its folder deleted by hand: two meetings must not share one).
    fn free_name(&self, name: &str) -> Result<String, MeetingsError> {
        let mut candidate = name.to_string();
        let mut number = 2;
        loop {
            let listed = self
                .connection
                .query_row("SELECT 1 FROM meetings WHERE folder = ?1", [&candidate], |_| Ok(()))
                .optional()?
                .is_some();
            if !listed && !self.folder.join(&candidate).exists() {
                return Ok(candidate);
            }
            candidate = format!("{name} ({number})");
            number += 1;
        }
    }

    /// Meetings recorded before folders had dates: each folder `<id>` gets the name of when
    /// its meeting started. A folder that cannot be renamed keeps its number and is tried again
    /// next time. The database learns the name first: one that cannot be written (a full disk)
    /// leaves the folder where its meeting finds it.
    fn name_numbered_folders(&mut self) -> Result<(), MeetingsError> {
        let numbered: Vec<(i64, i64)> = {
            let mut statement = self.connection.prepare(
                "SELECT id, started_ms FROM meetings WHERE kind = 'meeting' AND folder IS NULL",
            )?;
            let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        for (id, started_ms) in numbered {
            let old = self.folder.join(id.to_string());
            if !old.is_dir() {
                continue;
            }
            let name = self.free_name(&folder_name(started_ms))?;
            self.connection
                .execute("UPDATE meetings SET folder = ?2 WHERE id = ?1", params![id, name])?;
            if let Err(error) = fs::rename(&old, self.folder.join(&name)) {
                log_line!("meeting {id}: folder not renamed: {:?}", error.kind());
                self.connection
                    .execute("UPDATE meetings SET folder = NULL WHERE id = ?1", params![id])?;
            }
        }
        Ok(())
    }

    pub fn set_duration(&mut self, id: i64, duration_ms: i64) -> Result<(), MeetingsError> {
        self.update("UPDATE meetings SET duration_ms = ?2 WHERE id = ?1", params![id, duration_ms])
    }

    /// A recording that ended: its length and the state "recorded" in one write, which, failing
    /// (a full disk), still shows the meeting resting (`ended`).
    pub fn set_recorded(&mut self, id: i64, duration_ms: i64) -> Result<(), MeetingsError> {
        let written = self.update(
            "UPDATE meetings SET duration_ms = ?2, state = 'recorded', problem = NULL WHERE id = ?1",
            params![id, duration_ms],
        );
        self.ended(id, written)
    }

    pub fn set_state(
        &mut self,
        id: i64,
        state: MeetingState,
        problem: Option<&str>,
    ) -> Result<(), MeetingsError> {
        let written = self.update(
            "UPDATE meetings SET state = ?2, problem = ?3 WHERE id = ?1",
            params![id, state.as_str(), problem],
        );
        match state {
            MeetingState::Recording | MeetingState::Transcribing => {
                written?;
                self.unrested.retain(|left| *left != id);
                Ok(())
            }
            _ => self.ended(id, written),
        }
    }

    /// The write that ends a meeting's work: written, it is what the list shows; not written (a
    /// full disk), the meeting is shown resting all the same, as the next open puts it, so it
    /// can be transcribed again without a restart.
    fn ended(&mut self, id: i64, written: Result<(), MeetingsError>) -> Result<(), MeetingsError> {
        self.unrested.retain(|left| *left != id);
        if written.as_ref().is_err_and(|error| !matches!(error, MeetingsError::NotFound)) {
            self.unrested.push(id);
        }
        written
    }

    pub fn set_transcript(&mut self, id: i64, turns: &[Turn]) -> Result<(), MeetingsError> {
        let (stored, preview) = stored_turns(turns);
        let written = self.update(
            "UPDATE meetings SET state = 'done', problem = NULL, turns = ?2, preview = ?3
             WHERE id = ?1",
            params![id, stored, preview],
        );
        self.ended(id, written)
    }

    /// A transcribed file as a finished entry, with no folder.
    pub fn add_file(
        &mut self,
        transcribed_ms: i64,
        file: &FileTranscript,
    ) -> Result<i64, MeetingsError> {
        let (turns, preview) = stored_turns(&file.turns);
        self.connection.execute(
            "INSERT INTO meetings
                 (started_ms, duration_ms, state, turns, preview, kind, source, output, processed)
             VALUES (?1, ?2, 'done', ?3, ?4, 'file', ?5, ?6, ?7)",
            params![
                transcribed_ms,
                file.duration_ms,
                turns,
                preview,
                file.source.display().to_string(),
                file.output.as_ref().map(|output| output.display().to_string()),
                file.processed,
            ],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// A file entry whose result could not be written got it saved elsewhere: that is its
    /// saved file now.
    pub fn set_output(&mut self, id: i64, output: &Path) -> Result<(), MeetingsError> {
        self.update(
            "UPDATE meetings SET output = ?2 WHERE id = ?1 AND kind = 'file'",
            params![id, output.display().to_string()],
        )
    }

    pub fn rename(&mut self, id: i64, title: Option<&str>) -> Result<(), MeetingsError> {
        let title = title.map(str::trim).filter(|title| !title.is_empty());
        self.update("UPDATE meetings SET title = ?2 WHERE id = ?1", params![id, title])
    }

    /// The summary's columns: a meeting that could not rest shows the state it rests in.
    fn summary(&self) -> String {
        let state = if self.unrested.is_empty() {
            "state".to_string()
        } else {
            let ids = self.unrested.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
            format!("CASE WHEN id IN ({ids}) THEN {RESTED} ELSE state END")
        };
        SUMMARY.replace("{state}", &state)
    }

    /// Newest first.
    pub fn list(&self) -> Result<Vec<MeetingSummary>, MeetingsError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {} FROM meetings ORDER BY started_ms DESC, id DESC",
            self.summary()
        ))?;
        let rows = statement.query_map([], summary)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get(&self, id: i64) -> Result<Meeting, MeetingsError> {
        let found = self
            .connection
            .query_row(
                &format!("SELECT {}, turns, processed FROM meetings WHERE id = ?1", self.summary()),
                [id],
                |row| {
                    Ok((
                        summary(row)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                    ))
                },
            )
            .optional()?;
        let (summary, turns, processed) = found.ok_or(MeetingsError::NotFound)?;
        let turns = turns.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        Ok(Meeting { summary, turns, processed })
    }

    /// Removes the entry and a meeting's audio; a file's source and result stay.
    pub fn delete(&mut self, id: i64) -> Result<(), MeetingsError> {
        let meeting: Option<bool> = self
            .connection
            .query_row("SELECT kind = 'meeting' FROM meetings WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        // Only a meeting has a folder of its own; a file entry never deletes anything.
        if meeting == Some(true) {
            let folder = self.folder(id)?;
            if folder.exists() {
                fs::remove_dir_all(&folder)?;
            }
        }
        self.connection.execute("DELETE FROM meetings WHERE id = ?1", [id])?;
        Ok(())
    }

    fn update(&mut self, sql: &str, params: impl rusqlite::Params) -> Result<(), MeetingsError> {
        match self.connection.execute(sql, params)? {
            0 => Err(MeetingsError::NotFound),
            _ => Ok(()),
        }
    }
}

fn summary(row: &rusqlite::Row) -> rusqlite::Result<MeetingSummary> {
    Ok(MeetingSummary {
        id: row.get(0)?,
        started_ms: row.get(1)?,
        duration_ms: row.get(2)?,
        title: row.get(3)?,
        state: MeetingState::parse(&row.get::<_, String>(4)?),
        problem: row.get(5)?,
        preview: row.get(6)?,
        kind: Kind::parse(&row.get::<_, String>(7)?),
        source: row.get(8)?,
        output: row.get(9)?,
    })
}

/// The turns as stored, and the beginning of their text for the list.
fn stored_turns(turns: &[Turn]) -> (String, String) {
    let stored = serde_json::to_string(turns).unwrap_or_else(|_| "[]".into());
    let joined = turns.iter().map(Turn::text).collect::<Vec<_>>().join(" ");
    (stored, joined.chars().take(PREVIEW_CHARS).collect())
}

/// The store, opened on first use and shared by the commands and the transcription.
pub struct SharedMeetings {
    path: PathBuf,
    folder: PathBuf,
    store: Mutex<Option<MeetingStore>>,
}

impl SharedMeetings {
    pub fn new(path: PathBuf, folder: PathBuf) -> Self {
        Self { path, folder, store: Mutex::new(None) }
    }

    pub fn with<T>(
        &self,
        action: impl FnOnce(&mut MeetingStore) -> Result<T, MeetingsError>,
    ) -> Result<T, MeetingsError> {
        let mut store = self.store.lock().expect("meetings lock");
        if store.is_none() {
            *store = Some(MeetingStore::open(&self.path, self.folder.clone())?);
        }
        action(store.as_mut().expect("opened above"))
    }
}

/// Whether `name` is one plain folder name: not empty, no separators, not `.` or `..`.
fn plain_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!((parts.next(), parts.next()), (Some(std::path::Component::Normal(_)), None))
}

/// A folder name for a meeting that started at `started_ms`: the local date and time, as
/// `2026-11-10 15-26` (no colons, which Windows does not allow in names).
pub fn folder_name(started_ms: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(started_ms).earliest() {
        Some(time) => time.format("%Y-%m-%d %H-%M").to_string(),
        None => started_ms.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::files::Word;
    use crate::meetings::transcript::{Speaker, turns};

    fn test_dir() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts").join(
            format!("meetings-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)),
        );
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn open(dir: &Path) -> MeetingStore {
        MeetingStore::open(&dir.join("meetings.sqlite3"), dir.join("meetings")).unwrap()
    }

    fn word(start: f64, text: &str) -> Word {
        Word { start, end: start + 0.5, text: format!(" {text}") }
    }

    /// A rebuild that cannot run (here a statement in progress, as no room on a full disk) leaves
    /// the database open and usable.
    #[test]
    fn a_failed_rebuild_does_not_stop_the_open() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE t (x BLOB); INSERT INTO t VALUES (zeroblob(100000)); DELETE FROM t;",
            )
            .unwrap();
        let free: i64 =
            connection.pragma_query_value(None, "freelist_count", |row| row.get(0)).unwrap();
        assert!(free > 0);
        let mut busy = connection.prepare("SELECT 1 UNION ALL SELECT 2").unwrap();
        let mut rows = busy.query([]).unwrap();
        rows.next().unwrap();
        compact(&connection);
        drop(rows);
        drop(busy);
        assert!(
            connection
                .pragma_query_value(None, "freelist_count", |row| row.get::<_, i64>(0))
                .unwrap()
                > 0,
            "not rebuilt"
        );
        compact(&connection);
        let free: i64 =
            connection.pragma_query_value(None, "freelist_count", |row| row.get(0)).unwrap();
        assert_eq!(free, 0, "rebuilt once it can be");
    }

    /// A database that cannot be written (here a read-only file, as a full disk) still opens,
    /// with the meeting left recording as it was.
    #[test]
    fn meetings_open_when_the_file_cannot_be_written() {
        let dir = test_dir();
        let path = dir.join("meetings.sqlite3");
        let id = {
            let mut store = open(&dir);
            store.create(1_000).unwrap()
        };
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions.clone()).unwrap();
        let opened = MeetingStore::open(&path, dir.join("meetings"));
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
        let store = opened.expect("opens all the same");
        // Shown as it will rest: it is not going.
        assert_eq!(store.get(id).unwrap().summary.state, MeetingState::Recorded);
        assert_eq!(store.list().unwrap()[0].state, MeetingState::Recorded);
        drop(store);
        assert_eq!(
            open(&dir).get(id).unwrap().summary.state,
            MeetingState::Recorded,
            "rests later"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A meeting that could not rest shows the state it rests in, until it is worked on again.
    #[test]
    fn an_unrested_meeting_shows_its_rest_until_worked_on() {
        let dir = test_dir();
        let mut store = open(&dir);
        let id = store.create(1_000).unwrap();
        store.unrested = vec![id];
        assert_eq!(store.get(id).unwrap().summary.state, MeetingState::Recorded);
        store.set_state(id, MeetingState::Transcribing, None).unwrap();
        assert_eq!(store.get(id).unwrap().summary.state, MeetingState::Transcribing);
        assert_eq!(store.list().unwrap()[0].state, MeetingState::Transcribing);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A file entry whose result could not be written gets the file it was saved to later;
    /// a meeting gets none.
    #[test]
    fn an_unsaved_file_gets_its_saved_file() {
        let dir = test_dir();
        let mut store = open(&dir);
        let file = FileTranscript {
            source: dir.join("talk.mp3"),
            output: None,
            duration_ms: 1_000,
            turns: Vec::new(),
            processed: None,
        };
        let id = store.add_file(1, &file).unwrap();
        assert_eq!(store.get(id).unwrap().summary.output, None);
        let saved = dir.join("elsewhere.md");
        store.set_output(id, &saved).unwrap();
        assert_eq!(store.get(id).unwrap().summary.output, Some(saved.display().to_string()));
        let meeting = store.create(2_000).unwrap();
        assert!(store.set_output(meeting, &saved).is_err(), "not a file entry");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A meeting whose work ends while the file cannot be written (a full disk) is shown
    /// resting, not left transcribing or recording, and can be transcribed again.
    #[test]
    fn a_meeting_whose_end_cannot_be_written_shows_its_rest() {
        let dir = test_dir();
        let mut store = open(&dir);
        let recorded = store.create(1_000).unwrap();
        let transcribed = store.create(2_000).unwrap();
        store.set_state(transcribed, MeetingState::Transcribing, None).unwrap();
        store.connection.execute_batch("PRAGMA query_only = 1").unwrap();
        assert!(store.set_recorded(recorded, 5_000).is_err());
        assert!(store.set_transcript(transcribed, &[]).is_err());
        assert!(store.set_state(transcribed, MeetingState::Failed, Some("save")).is_err());
        assert_eq!(store.get(recorded).unwrap().summary.state, MeetingState::Recorded);
        assert_eq!(store.get(transcribed).unwrap().summary.state, MeetingState::Recorded);
        store.connection.execute_batch("PRAGMA query_only = 0").unwrap();
        store.set_recorded(recorded, 5_000).unwrap();
        let summary = store.get(recorded).unwrap().summary;
        assert_eq!((summary.state, summary.duration_ms), (MeetingState::Recorded, 5_000));
        store.set_state(transcribed, MeetingState::Transcribing, None).unwrap();
        assert_eq!(store.get(transcribed).unwrap().summary.state, MeetingState::Transcribing);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A numbered folder and a database that cannot be written: the folder keeps its number,
    /// where its meeting finds it.
    #[test]
    fn a_numbered_folder_stays_while_the_file_cannot_be_written() {
        let dir = test_dir();
        let path = dir.join("meetings.sqlite3");
        let id = {
            let mut store = open(&dir);
            let id = store.create(1_000).unwrap();
            fs::rename(store.folder(id).unwrap(), dir.join("meetings").join(id.to_string()))
                .unwrap();
            store
                .connection
                .execute("UPDATE meetings SET folder = NULL WHERE id = ?1", [id])
                .unwrap();
            id
        };
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions.clone()).unwrap();
        let opened = MeetingStore::open(&path, dir.join("meetings"));
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&path, permissions).unwrap();
        let store = opened.expect("opens all the same");
        let numbered = dir.join("meetings").join(id.to_string());
        assert_eq!(store.folder(id).unwrap(), numbered);
        assert!(numbered.is_dir());
        drop(store);
        // Named later, when it can be.
        assert_ne!(open(&dir).folder(id).unwrap(), numbered);
        assert!(!numbered.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_meeting_keeps_its_transcript_until_deleted() {
        let dir = test_dir();
        let mut store = open(&dir);
        let id = store.create(1_000).unwrap();
        let folder = store.folder(id).unwrap();
        fs::write(folder.join(ME_FILE), b"audio").unwrap();
        store.set_duration(id, 65_000).unwrap();
        let turns = turns(vec![word(0.0, "hello")], vec![(Speaker::Person(1), word(2.0, "hi"))]);
        store.set_transcript(id, &turns).unwrap();
        drop(store);

        let mut store = open(&dir);
        let meeting = store.get(id).unwrap();
        assert_eq!(meeting.summary.state, MeetingState::Done);
        assert_eq!(meeting.summary.duration_ms, 65_000);
        assert_eq!(meeting.summary.preview.as_deref(), Some("hello hi"));
        assert_eq!(meeting.turns, turns);
        store.delete(id).unwrap();
        assert!(!folder.exists());
        assert!(matches!(store.get(id), Err(MeetingsError::NotFound)));
    }

    #[test]
    fn an_interrupted_meeting_becomes_recorded() {
        let dir = test_dir();
        let mut store = open(&dir);
        let recording = store.create(1).unwrap();
        let transcribing = store.create(2).unwrap();
        store.set_state(transcribing, MeetingState::Transcribing, None).unwrap();
        drop(store);
        let store = open(&dir);
        for id in [recording, transcribing] {
            assert_eq!(store.get(id).unwrap().summary.state, MeetingState::Recorded);
            assert!(store.folder(id).unwrap().is_dir());
        }
    }

    #[test]
    fn the_list_is_newest_first_with_titles() {
        let dir = test_dir();
        let mut store = open(&dir);
        let first = store.create(10).unwrap();
        let second = store.create(20).unwrap();
        store.rename(first, Some("  Planning  ")).unwrap();
        store.set_state(second, MeetingState::Failed, Some("no speech")).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.iter().map(|m| m.id).collect::<Vec<_>>(), [second, first]);
        assert_eq!(list[1].title.as_deref(), Some("Planning"));
        assert_eq!(list[0].problem.as_deref(), Some("no speech"));
        store.rename(first, Some(" ")).unwrap();
        assert_eq!(store.get(first).unwrap().summary.title, None);
        assert!(matches!(store.rename(999, None), Err(MeetingsError::NotFound)));
    }

    #[test]
    fn a_file_is_kept_next_to_the_meetings_without_its_audio() {
        let dir = test_dir();
        let mut store = open(&dir);
        let meeting = store.create(10).unwrap();
        // The user's files, which the entry only points to.
        fs::create_dir_all(&dir).unwrap();
        let (source, output) = (dir.join("talk.mp3"), dir.join("talk.md"));
        fs::write(&source, b"audio").unwrap();
        fs::write(&output, b"text").unwrap();
        let file = FileTranscript {
            source: source.clone(),
            output: Some(output.clone()),
            duration_ms: 40_000,
            turns: turns(Vec::new(), vec![(Speaker::Others, word(1.0, "hello"))]),
            processed: Some("Dear team".into()),
        };
        let id = store.add_file(20, &file).unwrap();

        let list = store.list().unwrap();
        assert_eq!(
            list.iter().map(|m| (m.id, m.kind)).collect::<Vec<_>>(),
            [(id, Kind::File), (meeting, Kind::Meeting)]
        );
        let entry = store.get(id).unwrap();
        assert_eq!(entry.summary.state, MeetingState::Done);
        assert_eq!(entry.summary.duration_ms, 40_000);
        assert_eq!(entry.summary.source.as_deref(), Some(source.display().to_string().as_str()));
        assert_eq!(entry.summary.output.as_deref(), Some(output.display().to_string().as_str()));
        assert_eq!(entry.summary.preview.as_deref(), Some("hello"));
        assert_eq!(entry.turns, file.turns);
        assert_eq!(entry.processed.as_deref(), Some("Dear team"));
        assert!(!store.folder(id).unwrap().exists(), "a file has no folder of its own");
        assert_eq!(store.get(meeting).unwrap().processed, None);

        store.delete(id).unwrap();
        assert!(matches!(store.get(id), Err(MeetingsError::NotFound)));
        assert!(source.is_file() && output.is_file(), "the user's files stay");
    }

    #[test]
    fn meetings_of_the_first_version_are_upgraded() {
        let dir = test_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("meetings.sqlite3");
        let old = Connection::open(&path).unwrap();
        old.execute_batch(MIGRATIONS[0]).unwrap();
        old.execute(
            "INSERT INTO meetings (started_ms, title, state) VALUES (5, 'Planning', 'done')",
            [],
        )
        .unwrap();
        old.pragma_update(None, "user_version", 1).unwrap();
        drop(old);

        let store = open(&dir);
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].kind, Kind::Meeting);
        assert_eq!(list[0].title.as_deref(), Some("Planning"));
        assert_eq!(list[0].source, None);
        drop(store);
        let version: u32 = Connection::open(&path)
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn a_deleted_transcript_leaves_no_text_in_the_file() {
        let dir = test_dir();
        let mut store = open(&dir);
        let path = dir.join("meetings.sqlite3");
        let in_file = |text: &str| {
            let bytes = fs::read(&path).unwrap();
            bytes.windows(text.len()).any(|window| window == text.as_bytes())
        };
        let said = |marker: &str| {
            let words: Vec<Word> =
                (0..400).map(|i| word(f64::from(i), &format!("{marker}{i:03}"))).collect();
            turns(Vec::new(), words.into_iter().map(|w| (Speaker::Others, w)).collect())
        };
        let kept = store.create(1).unwrap();
        store.set_transcript(kept, &said("keptword")).unwrap();
        let gone = store.create(2).unwrap();
        store.set_transcript(gone, &said("secretword")).unwrap();
        store.rename(gone, Some("Secret title")).unwrap();
        let file = store
            .add_file(
                3,
                &FileTranscript {
                    source: dir.join("private-source.mp3"),
                    output: Some(dir.join("private-output.txt")),
                    duration_ms: 1,
                    turns: said("filesecret"),
                    processed: Some("processedsecret".into()),
                },
            )
            .unwrap();
        assert!(in_file("secretword200") && in_file("filesecret200"));

        store.delete(gone).unwrap();
        store.delete(file).unwrap();
        for text in
            ["secretword000", "secretword399", "Secret title", "filesecret200", "processedsecret"]
        {
            assert!(!in_file(text), "{text} stays in the file");
        }
        assert!(!in_file("private-source.mp3"), "a file's path stays in the file");
        assert!(in_file("keptword200"));
    }

    #[test]
    fn text_deleted_before_secure_delete_is_cleared_on_open() {
        let dir = test_dir();
        drop(open(&dir));
        let path = dir.join("meetings.sqlite3");
        let old = Connection::open(&path).unwrap();
        let text: String = (0..2000).map(|i| format!("olddeleted{i:04};")).collect();
        old.execute(
            "INSERT INTO meetings (started_ms, state, turns) VALUES (1, 'done', ?1)",
            [&text],
        )
        .unwrap();
        old.execute("DELETE FROM meetings", []).unwrap();
        drop(old);
        let in_file = |text: &str| {
            let bytes = fs::read(&path).unwrap();
            bytes.windows(text.len()).any(|window| window == text.as_bytes())
        };
        assert!(in_file("olddeleted1000;"), "without secure_delete the text stays");
        drop(open(&dir));
        assert!(!in_file("olddeleted1000;"));
    }

    #[test]
    fn a_stored_folder_name_never_leads_out_of_the_meetings_folder() {
        let dir = test_dir();
        let mut store = open(&dir);
        let meetings = dir.join("meetings");
        let precious = dir.join("precious");
        fs::create_dir_all(&precious).unwrap();
        for bad in ["", "..", "../precious", "a/b", precious.to_str().unwrap()] {
            let id = store.create(7).unwrap();
            let own = store.folder(id).unwrap();
            store
                .connection
                .execute("UPDATE meetings SET folder = ?2 WHERE id = ?1", params![id, bad])
                .unwrap();
            assert_eq!(store.folder(id).unwrap(), meetings.join(id.to_string()), "{bad:?}");
            store.delete(id).unwrap();
            assert!(precious.is_dir() && meetings.is_dir(), "{bad:?}");
            let _ = fs::remove_dir_all(own);
        }
        // A file entry has no folder: deleting it leaves a folder that happens to bear its number.
        let file = store
            .add_file(
                3,
                &FileTranscript {
                    source: dir.join("a.mp3"),
                    output: Some(dir.join("a.txt")),
                    duration_ms: 1,
                    turns: Vec::new(),
                    processed: None,
                },
            )
            .unwrap();
        fs::create_dir_all(meetings.join(file.to_string())).unwrap();
        store.delete(file).unwrap();
        assert!(meetings.join(file.to_string()).is_dir());
    }

    #[test]
    fn a_stopped_second_transcription_keeps_the_meeting_done() {
        let dir = test_dir();
        let mut store = open(&dir);
        let done = store.create(1).unwrap();
        let said = turns(Vec::new(), vec![(Speaker::Others, word(0.0, "Hello"))]);
        store.set_transcript(done, &said).unwrap();
        store.set_state(done, MeetingState::Transcribing, None).unwrap();
        store.rest(done).unwrap();
        assert_eq!(store.get(done).unwrap().summary.state, MeetingState::Done);
        let fresh = store.create(2).unwrap();
        store.set_state(fresh, MeetingState::Transcribing, None).unwrap();
        store.rest(fresh).unwrap();
        assert_eq!(store.get(fresh).unwrap().summary.state, MeetingState::Recorded);
        // The same after a crash in the middle of it.
        store.set_state(done, MeetingState::Transcribing, None).unwrap();
        drop(store);
        assert_eq!(open(&dir).get(done).unwrap().summary.state, MeetingState::Done);
    }

    #[test]
    fn a_meeting_folder_is_named_by_when_it_started() {
        let dir = test_dir();
        let mut store = open(&dir);
        // 2026-11-10 12:26:48 UTC; the name is in local time.
        let started = 1_794_317_208_333;
        let name = folder_name(started);
        assert_eq!(name.len(), "2026-11-10 15-26".len());
        assert!(!name.contains(':'), "Windows allows no colons in names");
        let first = store.create(started).unwrap();
        let second = store.create(started + 1_000).unwrap();
        assert_eq!(store.folder(first).unwrap(), dir.join("meetings").join(&name));
        assert_eq!(store.folder(second).unwrap(), dir.join("meetings").join(format!("{name} (2)")));
        assert!(store.folder(second).unwrap().is_dir());
        // A folder deleted by hand is not given to a new meeting while its meeting is listed.
        fs::remove_dir_all(store.folder(second).unwrap()).unwrap();
        let third = store.create(started + 2_000).unwrap();
        assert!(store.folder(third).unwrap().ends_with(format!("{name} (3)")));

        let folder = store.folder(first).unwrap();
        store.delete(first).unwrap();
        assert!(!folder.exists(), "a deleted meeting takes its folder with it");
        // Reopened, the store finds the folder by its name.
        drop(store);
        let store = open(&dir);
        assert!(store.folder(second).unwrap().ends_with(format!("{name} (2)")));
    }

    #[test]
    fn numbered_folders_get_their_dates() {
        let dir = test_dir();
        fs::create_dir_all(dir.join("meetings/9")).unwrap();
        fs::write(dir.join("meetings/9").join(ME_FILE), b"audio").unwrap();
        let path = dir.join("meetings.sqlite3");
        let old = Connection::open(&path).unwrap();
        old.execute_batch(MIGRATIONS[0]).unwrap();
        old.execute_batch(MIGRATIONS[1]).unwrap();
        old.execute(
            "INSERT INTO meetings (id, started_ms, state) VALUES (9, 1794317208333, 'done')",
            [],
        )
        .unwrap();
        // A meeting whose folder is gone stays as it is.
        old.execute("INSERT INTO meetings (id, started_ms, state) VALUES (4, 5, 'done')", [])
            .unwrap();
        old.pragma_update(None, "user_version", 2).unwrap();
        drop(old);

        let mut store = open(&dir);
        let folder = dir.join("meetings").join(folder_name(1_794_317_208_333));
        assert_eq!(store.folder(9).unwrap(), folder);
        assert!(folder.join(ME_FILE).is_file());
        assert!(!dir.join("meetings/9").exists());
        assert_eq!(store.folder(4).unwrap(), dir.join("meetings/4"));
        store.delete(9).unwrap();
        assert!(!folder.exists());
    }

    #[test]
    fn a_newer_database_is_refused_untouched() {
        let dir = test_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("meetings.sqlite3");
        Connection::open(&path).unwrap().pragma_update(None, "user_version", 99).unwrap();
        let opened = MeetingStore::open(&path, dir.join("meetings"));
        assert!(matches!(opened, Err(MeetingsError::NewerVersion { found: 99 })));
    }
}
