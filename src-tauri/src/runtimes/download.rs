//! Resumable, verified downloads.
//!
//! Data goes to `<dest>.part`; an existing part is resumed with an HTTP `Range` request.
//! Only a file with the expected size and SHA-256 is renamed to `dest`. A part that cannot
//! become the expected file is deleted; an interrupted or cancelled one is kept for resuming.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};
use ureq::Agent;

use crate::domain::events::DownloadFailure;
use crate::util::hex;

const CHUNK: usize = 1 << 16;

#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    pub size: u64,
    pub sha256: &'a str,
}

#[derive(Debug)]
pub enum DownloadError {
    Cancelled,
    NoSpace {
        needed: u64,
        available: u64,
    },
    Http(String),
    /// The connection ended early; the part is kept for resuming.
    Incomplete {
        received: u64,
        expected: u64,
    },
    TooLarge {
        expected: u64,
    },
    /// The server's file has another size than the pinned one (it was replaced): the part is
    /// deleted, it can never become the expected file.
    WrongSize {
        expected: u64,
    },
    HashMismatch,
    /// A verified model archive lacks a file the catalog expects in it.
    Archive(String),
    Io(io::Error),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "download cancelled"),
            Self::NoSpace { needed, available } => {
                write!(f, "not enough disk space: need {needed} bytes, {available} available")
            }
            Self::Http(detail) => write!(f, "download failed: {detail}"),
            Self::Incomplete { received, expected } => {
                write!(f, "download interrupted at {received} of {expected} bytes")
            }
            Self::TooLarge { expected } => write!(f, "server sent more than {expected} bytes"),
            Self::WrongSize { expected } => {
                write!(f, "the server's file is not the expected {expected} bytes")
            }
            Self::HashMismatch => write!(f, "downloaded file does not match its SHA-256"),
            Self::Archive(detail) => write!(f, "model archive problem: {detail}"),
            Self::Io(error) => write!(f, "download file error: {error}"),
        }
    }
}

impl std::error::Error for DownloadError {}

impl DownloadError {
    pub fn failure(&self) -> DownloadFailure {
        match self {
            Self::Cancelled => DownloadFailure::Cancelled,
            Self::NoSpace { .. } => DownloadFailure::NoSpace,
            Self::Http(_) | Self::Incomplete { .. } => DownloadFailure::Network,
            Self::TooLarge { .. }
            | Self::WrongSize { .. }
            | Self::HashMismatch
            | Self::Archive(_) => DownloadFailure::Verification,
            // The disk filled up while writing (unpacking, another program meanwhile).
            Self::Io(error) if error.kind() == io::ErrorKind::StorageFull => {
                DownloadFailure::NoSpace
            }
            Self::Io(_) => DownloadFailure::Disk,
        }
    }
}

impl From<io::Error> for DownloadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// Writes what `source` holds to `dest` through its part, so a half-written file is never taken
/// for a whole one: a file taken out of a verified archive.
pub fn write_through_part(source: &mut impl Read, dest: &Path) -> io::Result<()> {
    let part = part_path(dest);
    let written = File::create(&part).and_then(|mut out| {
        io::copy(source, &mut out)?;
        out.sync_all()
    });
    // A part cut short (a full disk) is no use: it would only take the space.
    if let Err(error) = written {
        let _ = fs::remove_file(&part);
        return Err(error);
    }
    fs::rename(&part, dest)
}

/// Takes the `wanted` files out of a `.tar.bz2` by their own names, each to where `target`
/// puts it. Only files at the archive's top level or in its single top folder count, so a
/// same-named file deeper inside (e.g. among test recordings) cannot stand in for one; the first
/// of a name wins.
pub fn unpack_named(
    archive: &Path,
    wanted: &[&str],
    target: impl Fn(&str) -> PathBuf,
) -> Result<(), DownloadError> {
    let mut found: Vec<&str> = Vec::new();
    let decoder = bzip2::read::BzDecoder::new(io::BufReader::new(File::open(archive)?));
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries()? {
        if found.len() == wanted.len() {
            break;
        }
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else { continue };
        let Some(name) = wanted.iter().find(|wanted| **wanted == name) else { continue };
        if path.components().count() > 2 || found.contains(name) {
            continue;
        }
        write_through_part(&mut entry, &target(name))?;
        found.push(name);
    }
    match wanted.iter().find(|name| !found.contains(name)) {
        Some(missing) => Err(DownloadError::Archive(format!("{missing} is not in the archive"))),
        None => Ok(()),
    }
}

/// How long the server may send nothing before the download gives up. The part stays, so the
/// next try goes on from it.
const STALL: Duration = Duration::from_secs(60);

/// Downloads `url` to `dest`. `progress` receives (bytes on disk, total bytes).
pub fn download(
    agent: &Agent,
    url: &str,
    dest: &Path,
    expected: Expected,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), DownloadError> {
    download_stalling(agent, url, dest, expected, cancel, progress, STALL)
}

/// [`download`] with the stall limit given: a link that goes quiet must not hang the download
/// for ever, where even Cancel is not seen.
fn download_stalling(
    agent: &Agent,
    url: &str,
    dest: &Path,
    expected: Expected,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
    stall: Duration,
) -> Result<(), DownloadError> {
    let dir = dest.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let part = part_path(dest);
    let mut hasher = Sha256::new();
    let mut received = resume_point(&part, expected.size, &mut hasher)?;

    let needed = expected.size - received;
    let available = crate::platform::available_space(dir)?;
    if available < needed {
        return Err(DownloadError::NoSpace { needed, available });
    }

    let mut request = agent.get(url).config().timeout_per_read(Some(stall)).build();
    if received > 0 {
        request = request.header("Range", &format!("bytes={received}-"));
    }
    let mut response = request.call().map_err(|error| DownloadError::Http(error.to_string()))?;
    let status = response.status().as_u16();
    let header = |name| response.headers().get(name).and_then(|v| v.to_str().ok());
    let wrong_size = || -> Result<(), DownloadError> {
        match fs::remove_file(&part) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
            _ => Err(DownloadError::WrongSize { expected: expected.size }),
        }
    };
    let append = match status {
        206 => {
            let range = header("content-range");
            if !range.is_some_and(|range| range.starts_with(&format!("bytes {received}-"))) {
                return Err(DownloadError::Http(format!("unexpected Content-Range {range:?}")));
            }
            // `bytes 100-199/200`: the whole file's size after the slash.
            let total = range.and_then(|range| range.rsplit_once('/')).map(|(_, total)| total);
            if total.and_then(|total| total.parse::<u64>().ok()).is_some_and(|t| t != expected.size)
            {
                return wrong_size();
            }
            true
        }
        200 => {
            let length = header("content-length").and_then(|length| length.parse::<u64>().ok());
            if length.is_some_and(|length| length != expected.size) {
                return wrong_size();
            }
            // The server ignored the range: start over.
            hasher = Sha256::new();
            received = 0;
            false
        }
        416 if received == expected.size => return finish(&part, dest, hasher, expected),
        // The part runs past the end of the server's file: that file is shorter.
        416 => return wrong_size(),
        status => return Err(DownloadError::Http(format!("HTTP {status}"))),
    };

    let mut file =
        OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&part)?;
    let mut body = response.body_mut().as_reader();
    let mut buffer = vec![0; CHUNK];
    progress(received, expected.size);
    loop {
        if cancel.load(Ordering::Relaxed) {
            file.sync_all()?;
            return Err(DownloadError::Cancelled);
        }
        let read = match body.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                file.sync_all()?;
                return Err(DownloadError::Http(error.to_string()));
            }
        };
        if received + read as u64 > expected.size {
            drop(file);
            fs::remove_file(&part)?;
            return Err(DownloadError::TooLarge { expected: expected.size });
        }
        file.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        received += read as u64;
        progress(received, expected.size);
    }
    file.sync_all()?;
    drop(file);
    if received < expected.size {
        return Err(DownloadError::Incomplete { received, expected: expected.size });
    }
    finish(&part, dest, hasher, expected)
}

/// Hashes an existing part so the download can continue; drops a part that is too large.
fn resume_point(part: &Path, size: u64, hasher: &mut Sha256) -> Result<u64, DownloadError> {
    let length = match fs::metadata(part) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    if length > size {
        fs::remove_file(part)?;
        return Ok(0);
    }
    hash_into(part, hasher)?;
    Ok(length)
}

fn finish(
    part: &Path,
    dest: &Path,
    hasher: Sha256,
    expected: Expected,
) -> Result<(), DownloadError> {
    if hex(&hasher.finalize()) != expected.sha256 {
        fs::remove_file(part)?;
        return Err(DownloadError::HashMismatch);
    }
    fs::rename(part, dest)?;
    Ok(())
}

fn hash_into(path: &Path, hasher: &mut Sha256) -> io::Result<()> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0; CHUNK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        hasher.update(&buffer[..read]);
    }
}

/// SHA-256 of a whole file as lowercase hex.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    hash_into(path, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use super::*;
    use crate::runtimes::http::agent;
    use crate::runtimes::test_server::{Response, TestServer};

    const SIZE: usize = 200_000;

    /// Gives some bytes, then fails, as an archive cut short or a full disk.
    struct Breaks(usize);

    impl Read for Breaks {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.0 == 0 {
                return Err(io::Error::other("no room"));
            }
            let n = buf.len().min(self.0);
            buf[..n].fill(7);
            self.0 -= n;
            Ok(n)
        }
    }

    /// A file written whole is in place; one cut short leaves neither it nor its part.
    #[test]
    fn a_file_is_written_whole_or_not_at_all() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-artifacts")
            .join(format!("write-through-part-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("model.onnx");
        write_through_part(&mut &b"whole"[..], &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"whole");
        let cut = dir.join("cut.onnx");
        assert!(write_through_part(&mut Breaks(10_000), &cut).is_err());
        assert!(!cut.exists());
        assert!(!part_path(&cut).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    fn payload() -> Vec<u8> {
        (0..SIZE as u32).map(|i| (i % 251) as u8).collect()
    }

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    fn test_dir() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name =
            format!("download-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("test-artifacts").join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Serves `payload`, honouring `Range: bytes=N-`.
    fn ranged(payload: Vec<u8>) -> TestServer {
        TestServer::start(move |request| {
            let start = request
                .header("range")
                .and_then(|range| range.strip_prefix("bytes="))
                .and_then(|range| range.strip_suffix('-'))
                .and_then(|start| start.parse::<usize>().ok());
            match start {
                Some(start) if start >= payload.len() => Response::new(416, Vec::new()),
                Some(start) => Response::new(206, payload[start..].to_vec()).header(
                    "Content-Range",
                    &format!("bytes {start}-{}/{}", payload.len() - 1, payload.len()),
                ),
                None => Response::new(200, payload.clone()),
            }
        })
    }

    fn fetch(server: &TestServer, dest: &Path, sha256: &str) -> Result<(), DownloadError> {
        let never = AtomicBool::new(false);
        let expected = Expected { size: SIZE as u64, sha256 };
        download(
            &agent(),
            &format!("{}/m.bin", server.base),
            dest,
            expected,
            &never,
            &mut |_, _| {},
        )
    }

    #[test]
    fn a_link_that_goes_quiet_ends_the_download_and_keeps_the_part() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/m.bin", listener.local_addr().unwrap());
        // Sends the head and 1000 bytes, then holds the connection open in silence.
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.read(&mut [0; 4096]);
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {SIZE}\r\n\r\n");
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&payload()[..1000]).unwrap();
            stream.flush().unwrap();
            std::thread::sleep(Duration::from_secs(3));
        });
        let dest = test_dir().join("m.bin");
        let never = AtomicBool::new(false);
        let expected = Expected { size: SIZE as u64, sha256: "unused" };
        let started = std::time::Instant::now();
        let result = download_stalling(
            &agent(),
            &url,
            &dest,
            expected,
            &never,
            &mut |_, _| {},
            Duration::from_millis(300),
        );
        assert!(matches!(result, Err(DownloadError::Http(_))), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        assert_eq!(fs::read(part_path(&dest)).unwrap(), &payload()[..1000]);
        server.join().unwrap();
    }

    #[test]
    fn downloads_and_verifies() {
        let dest = test_dir().join("m.bin");
        let server = ranged(payload());
        fetch(&server, &dest, &sha(&payload())).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), payload());
        assert!(!part_path(&dest).exists());
        assert_eq!(server.requests()[0].header("range"), None);
    }

    #[test]
    fn resumes_a_partial_download() {
        let dest = test_dir().join("m.bin");
        fs::write(part_path(&dest), &payload()[..50_000]).unwrap();
        let server = ranged(payload());
        fetch(&server, &dest, &sha(&payload())).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), payload());
        assert_eq!(server.requests()[0].header("range"), Some("bytes=50000-"));
    }

    #[test]
    fn restarts_when_the_server_ignores_ranges() {
        let dest = test_dir().join("m.bin");
        fs::write(part_path(&dest), &payload()[..50_000]).unwrap();
        let full = payload();
        let server = TestServer::start(move |_| Response::new(200, full.clone()));
        fetch(&server, &dest, &sha(&payload())).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), payload());
    }

    #[test]
    fn complete_part_is_verified_without_downloading() {
        let dest = test_dir().join("m.bin");
        fs::write(part_path(&dest), payload()).unwrap();
        fetch(&ranged(payload()), &dest, &sha(&payload())).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), payload());
    }

    /// The server's file was replaced by one of another size: the part is deleted at once
    /// instead of being resumed against it for ever.
    #[test]
    fn a_file_of_another_size_deletes_the_part() {
        let shorter = payload()[..SIZE - 1000].to_vec();
        // A new download: 200 with another length.
        let dest = test_dir().join("m.bin");
        let result = fetch(&ranged(shorter.clone()), &dest, &sha(&payload()));
        assert!(matches!(result, Err(DownloadError::WrongSize { .. })), "{result:?}");
        assert!(!part_path(&dest).exists());
        // A part past the end of the shorter file: 416.
        let dest = test_dir().join("m.bin");
        fs::write(part_path(&dest), &payload()[..SIZE - 500]).unwrap();
        let result = fetch(&ranged(shorter.clone()), &dest, &sha(&payload()));
        assert!(matches!(result, Err(DownloadError::WrongSize { .. })), "{result:?}");
        assert!(!part_path(&dest).exists());
        // A part inside it: 206 names the whole file's size.
        let dest = test_dir().join("m.bin");
        fs::write(part_path(&dest), &payload()[..50_000]).unwrap();
        let result = fetch(&ranged(shorter), &dest, &sha(&payload()));
        assert!(matches!(result, Err(DownloadError::WrongSize { .. })), "{result:?}");
        assert!(!part_path(&dest).exists());
        assert_eq!(result.unwrap_err().failure(), DownloadFailure::Verification);
    }

    #[test]
    fn a_full_disk_is_no_space() {
        let full = DownloadError::Io(io::Error::from(io::ErrorKind::StorageFull));
        assert_eq!(full.failure(), DownloadFailure::NoSpace);
        let denied = DownloadError::Io(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(denied.failure(), DownloadFailure::Disk);
    }

    #[test]
    fn wrong_hash_deletes_the_part() {
        let dest = test_dir().join("m.bin");
        let result = fetch(&ranged(payload()), &dest, &sha(b"other"));
        assert!(matches!(result, Err(DownloadError::HashMismatch)));
        assert!(!dest.exists() && !part_path(&dest).exists());
    }

    #[test]
    fn interrupted_download_resumes_later() {
        let dest = test_dir().join("m.bin");
        let full = payload();
        let cut = TestServer::start(move |_| {
            let mut response = Response::new(200, full.clone());
            response.cut_after = Some(70_000);
            response
        });
        assert!(fetch(&cut, &dest, &sha(&payload())).is_err());
        assert_eq!(fs::metadata(part_path(&dest)).unwrap().len(), 70_000);
        fetch(&ranged(payload()), &dest, &sha(&payload())).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), payload());
    }

    #[test]
    fn oversized_response_is_rejected() {
        let dest = test_dir().join("m.bin");
        let mut bigger = payload();
        bigger.extend_from_slice(b"extra");
        let body = bigger.clone();
        let server = TestServer::start(move |_| Response::new(200, body.clone()));
        let result = fetch(&server, &dest, &sha(&payload()));
        assert!(matches!(result, Err(DownloadError::WrongSize { .. })), "by its length");
        assert!(!part_path(&dest).exists());
        // Without a length: by the bytes that came.
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/m.bin", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.read(&mut [0; 4096]);
            stream.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n").unwrap();
            let _ = stream.write_all(&bigger);
        });
        let never = AtomicBool::new(false);
        let expected = Expected { size: SIZE as u64, sha256: "unused" };
        let result = download(&agent(), &url, &dest, expected, &never, &mut |_, _| {});
        assert!(matches!(result, Err(DownloadError::TooLarge { .. })), "{result:?}");
        assert!(!part_path(&dest).exists());
        server.join().unwrap();
    }

    #[test]
    fn cancel_keeps_the_part() {
        let dest = test_dir().join("m.bin");
        let server = ranged(payload());
        let cancel = AtomicBool::new(false);
        let expected = Expected { size: SIZE as u64, sha256: &sha(&payload()) };
        let url = format!("{}/m.bin", server.base);
        let result = download(&agent(), &url, &dest, expected, &cancel, &mut |received, _| {
            if received > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert!(matches!(result, Err(DownloadError::Cancelled)));
        let kept = fs::metadata(part_path(&dest)).unwrap().len();
        assert!(kept > 0 && kept < SIZE as u64);
    }

    #[test]
    fn http_errors_are_reported() {
        let dest = test_dir().join("m.bin");
        let server = TestServer::start(|_| Response::new(404, b"missing".to_vec()));
        let result = fetch(&server, &dest, &sha(&payload()));
        assert!(matches!(result, Err(DownloadError::Http(detail)) if detail == "HTTP 404"));
    }
}
