//! Persistent data: settings, sealed keys, history, statistics, the model catalog and the
//! downloaded models; the file and SQLite helpers they share.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use rusqlite::{Connection, TransactionBehavior};

pub mod catalog;
pub mod history;
pub mod models;
pub mod secrets;
pub mod settings;
pub mod stats;

/// The schema version of an SQLite file: `PRAGMA user_version`.
pub fn user_version(connection: &Connection) -> rusqlite::Result<u32> {
    connection.pragma_query_value(None, "user_version", |row| row.get(0))
}

/// What a store tells of an SQLite error: its code only. SQLite's messages and the SQL text could
/// quote the data.
pub fn sqlite_code(error: &rusqlite::Error) -> String {
    match error.sqlite_error_code() {
        Some(code) => format!("{code:?}"),
        None => "unexpected database error".into(),
    }
}

/// Upgrades an SQLite file step by step in one transaction: migration `i` takes the schema from
/// version `i` to `i + 1`. Reads the version before writing anything, so a file of a newer
/// version is never touched: `newer` names it.
pub fn migrate_sqlite<E: From<rusqlite::Error>>(
    connection: &mut Connection,
    migrations: &[&str],
    newer: impl Fn(u32) -> E,
) -> Result<(), E> {
    let current = migrations.len() as u32;
    let found = user_version(connection)?;
    if found > current {
        return Err(newer(found));
    }
    if found == current {
        return Ok(());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Read again under the write lock in case another process upgraded the file meanwhile.
    let found = user_version(&transaction)?;
    if found > current {
        return Err(newer(found));
    }
    for migration in &migrations[found as usize..] {
        transaction.execute_batch(migration)?;
    }
    transaction.pragma_update(None, "user_version", current)?;
    transaction.commit()?;
    Ok(())
}

/// Deletes an SQLite file with its side files; a file that is not there is fine.
pub fn remove_sqlite_files(path: &Path) -> io::Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        match fs::remove_file(std::path::PathBuf::from(name)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(())
}

/// The app's identifier before `com.useahem.ahem`.
pub const OLD_IDENTIFIER: &str = "com.useahem.app";

/// Moves what the app kept under its earlier identifier into `dir`, once: each item that `dir`
/// does not have yet (the web view may already have made its own); the old folder goes when it
/// is empty. Returns what could not move, for the log, which opens only after the move.
pub fn move_from_old_identifier(dir: &Path) -> Vec<String> {
    let old = dir.with_file_name(OLD_IDENTIFIER);
    let Ok(entries) = fs::read_dir(&old) else { return Vec::new() };
    if let Err(error) = fs::create_dir_all(dir) {
        return vec![format!("cannot make the new data folder {}: {error}", dir.display())];
    }
    let mut problems = Vec::new();
    for entry in entries.flatten() {
        let target = dir.join(entry.file_name());
        if !target.exists()
            && let Err(error) = fs::rename(entry.path(), &target)
        {
            problems.push(format!(
                "cannot move {} to the new data folder: {error}",
                entry.path().display()
            ));
        }
    }
    let _ = fs::remove_dir(&old);
    problems
}

/// [`move_from_old_identifier`] for the settings' folder and the data's, once for a folder that
/// is both (macOS): what could not move is said once.
pub fn move_folders_from_old_identifier(config: &Path, data: &Path) -> Vec<String> {
    let mut problems = move_from_old_identifier(config);
    if data != config {
        problems.extend(move_from_old_identifier(data));
    }
    problems
}

/// Writes a JSON file of the app's own with [`write_whole`]; creates the missing folders.
fn write_json(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    write_whole(path, text)
}

/// Writes a file through a temporary one next to it, flushed to the disk before it takes the
/// old one's place, so a full disk, a crash or a power cut never leaves it half-written and the
/// old file stays until the new one is complete.
pub fn write_whole(path: &Path, text: &str) -> io::Result<()> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".ahem-part");
    let temp = std::path::PathBuf::from(temp);
    let written = File::create(&temp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    let replaced = written.and_then(|()| fs::rename(&temp, path));
    if replaced.is_err() {
        let _ = fs::remove_file(&temp);
    }
    replaced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_of_the_old_identifier_moves_over_once() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("identifier-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let old = root.join(OLD_IDENTIFIER);
        let new = root.join("com.useahem.ahem");
        fs::create_dir_all(old.join("models")).unwrap();
        fs::write(old.join("settings.json"), "old").unwrap();
        fs::write(old.join("web"), "old").unwrap();
        // The web view made its own data before.
        fs::create_dir_all(&new).unwrap();
        fs::write(new.join("web"), "new").unwrap();
        assert_eq!(move_from_old_identifier(&new), Vec::<String>::new());
        assert!(new.join("models").is_dir());
        assert_eq!(fs::read_to_string(new.join("settings.json")).unwrap(), "old");
        assert_eq!(fs::read_to_string(new.join("web")).unwrap(), "new", "kept");
        assert!(old.join("web").exists(), "the old folder stays with what did not move");
        // Again: nothing more to do.
        fs::remove_file(old.join("web")).unwrap();
        assert!(move_from_old_identifier(&new).is_empty());
        assert!(!old.exists());
        let _ = fs::remove_dir_all(&root);
    }

    /// What cannot move is given back for the log, and stays where it was.
    #[cfg(unix)]
    #[test]
    fn what_cannot_move_is_given_back() {
        use std::os::unix::fs::PermissionsExt;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("identifier-stuck-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let old = root.join(OLD_IDENTIFIER);
        let new = root.join("com.useahem.ahem");
        fs::create_dir_all(old.join("models")).unwrap();
        // A folder whose entries cannot be taken out of it.
        fs::set_permissions(&old, fs::Permissions::from_mode(0o555)).unwrap();
        // One folder for the settings and the data (macOS): said once.
        let problems = move_folders_from_old_identifier(&new, &new);
        fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("models"), "{problems:?}");
        assert!(old.join("models").is_dir());
        let _ = fs::remove_dir_all(&root);
    }
}
