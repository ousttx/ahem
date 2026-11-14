//! Model files on disk: `<root>/<id>/<file>` plus a `ready.json` marker written after the
//! file was verified. Startup checks compare the marker with the file's size and
//! modification time instead of hashing gigabytes again.
//!
//! A sherpa-onnx model is downloaded as its release archive: once the archive's hash matches,
//! the files the catalog lists are taken out next to it, the marker records them, and the
//! archive is deleted. A llama.cpp model is two files, the model and its projector; the marker
//! records both.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use ureq::Agent;

use super::catalog::{LlamaSpec, ModelSpec, SherpaSpec};
use crate::runtimes::download::{self, DownloadError, Expected};

const MARKER: &str = "ready.json";

/// A downloaded model, ready for its engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyModel {
    /// The Whisper `.bin`, the folder with the sherpa-onnx files, or the llama.cpp model
    /// `.gguf` with its projector next to it.
    pub path: PathBuf,
    /// Present for a sherpa-onnx model.
    pub sherpa: Option<SherpaSpec>,
    /// Present for a llama.cpp model.
    pub llama: Option<LlamaSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelState {
    Missing,
    /// An interrupted download that can be resumed.
    Partial {
        downloaded: u64,
    },
    Ready,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Marker {
    sha256: String,
    size: u64,
    modified_ns: u64,
}

/// The marker of a sherpa-onnx model: the archive's hash and the files taken from it.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ArchiveMarker {
    sha256: String,
    files: Vec<FileMark>,
}

/// The marker of a llama.cpp model: the model file and its projector.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PairMarker {
    model: Marker,
    mmproj: Marker,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FileMark {
    name: String,
    size: u64,
    modified_ns: u64,
}

#[derive(Debug, Clone)]
pub struct ModelStore {
    root: PathBuf,
}

impl ModelStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The folder with all downloaded models.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// What is downloaded: the Whisper or llama.cpp model itself, or the sherpa-onnx archive.
    pub fn file(&self, spec: &ModelSpec) -> PathBuf {
        self.dir(spec).join(&spec.file)
    }

    fn dir(&self, spec: &ModelSpec) -> PathBuf {
        self.root.join(&spec.id)
    }

    /// A llama.cpp model's projector, next to the model.
    fn mmproj(&self, spec: &ModelSpec, llama: &LlamaSpec) -> PathBuf {
        self.dir(spec).join(&llama.mmproj.file)
    }

    /// What the engine loads: the Whisper or llama.cpp file or the folder with the sherpa-onnx
    /// files.
    fn engine_path(&self, spec: &ModelSpec) -> PathBuf {
        match spec.sherpa {
            Some(_) => self.dir(spec),
            None => self.file(spec),
        }
    }

    pub fn state(&self, spec: &ModelSpec) -> ModelState {
        if self.is_ready(spec) {
            return ModelState::Ready;
        }
        let part_size = |file: &Path| fs::metadata(download::part_path(file)).map(|m| m.len());
        if let Ok(downloaded) = part_size(&self.file(spec)) {
            return ModelState::Partial { downloaded };
        }
        let model_whole = self.file(spec).is_file();
        // A whole archive without a marker: the disk had no room to unpack it, or it lacks a
        // file. It can be unpacked again or deleted. A whole single-file model without one is
        // verified by its hash when downloaded.
        let Some(llama) = &spec.llama else {
            return if model_whole && spec.sherpa.is_some() {
                ModelState::Partial { downloaded: spec.size }
            } else {
                ModelState::Missing
            };
        };
        // The projector is downloaded after the model: once the model's file is whole, the
        // download is that far even before the projector's first byte.
        match part_size(&self.mmproj(spec, llama)) {
            Ok(downloaded) => ModelState::Partial { downloaded: spec.size + downloaded },
            Err(_) if model_whole => ModelState::Partial { downloaded: spec.size },
            Err(_) => ModelState::Missing,
        }
    }

    pub fn ready(&self, spec: &ModelSpec) -> Option<ReadyModel> {
        self.is_ready(spec).then(|| ReadyModel {
            path: self.engine_path(spec),
            sherpa: spec.sherpa.clone(),
            llama: spec.llama.clone(),
        })
    }

    /// Makes the model ready. A file already in place without a marker (copied by hand) is
    /// verified by its hash instead of being downloaded again. `unpacking` is called once before
    /// a sherpa-onnx archive is unpacked, which takes a while after the download reaches 100 %.
    /// `progress` counts a llama.cpp model and its projector as one download.
    pub fn download(
        &self,
        agent: &Agent,
        spec: &ModelSpec,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, u64),
        unpacking: &mut dyn FnMut(),
    ) -> Result<PathBuf, DownloadError> {
        if self.is_ready(spec) {
            return Ok(self.engine_path(spec));
        }
        // Room for all of it at once, the unpacked files of an archive included: a disk that
        // fills up halfway wastes what was downloaded.
        fs::create_dir_all(self.dir(spec))?;
        let needed = self.space_needed(spec);
        let available = crate::platform::available_space(&self.dir(spec))?;
        if available < needed {
            return Err(DownloadError::NoSpace { needed, available });
        }
        let total = spec.download_size();
        // Where it starts, when something is on the disk already: a whole file in place is only
        // checked and unpacked, and the interface hears of it before that (a past failure goes,
        // the row is busy). Nothing yet, the first bytes tell it: a download that fails at once
        // (no network) never moves the row to the models on this computer and back.
        let on_disk = total - self.left_to_download(spec);
        if on_disk > 0 {
            progress(on_disk, total);
        }
        let file = self.file(spec);
        let expected = Expected { size: spec.size, sha256: &spec.sha256 };
        let mut model = |done: u64, _: u64| progress(done, total);
        fetch(agent, &spec.url, &file, expected, cancel, &mut model)?;
        if let Some(llama) = &spec.llama {
            let mmproj = &llama.mmproj;
            let expected = Expected { size: mmproj.size, sha256: &mmproj.sha256 };
            let mut after_model = |done: u64, _: u64| progress(spec.size + done, total);
            fetch(
                agent,
                &mmproj.url,
                &self.mmproj(spec, llama),
                expected,
                cancel,
                &mut after_model,
            )?;
        }
        if let Some(sherpa) = &spec.sherpa {
            unpacking();
            unpack(&file, &self.dir(spec), sherpa)?;
        }
        self.write_marker(spec)?;
        if spec.sherpa.is_some() {
            // Unpacked and recorded: the archive is no longer needed; a leftover one is harmless.
            let _ = fs::remove_file(&file);
        }
        Ok(self.engine_path(spec))
    }

    /// The bytes of the model's files not on disk yet.
    fn left_to_download(&self, spec: &ModelSpec) -> u64 {
        let left = |file: &Path, size: u64| {
            if fs::metadata(file).is_ok_and(|metadata| metadata.len() == size) {
                return 0;
            }
            let part = fs::metadata(download::part_path(file)).map_or(0, |m| m.len());
            size.saturating_sub(part)
        };
        let mut needed = left(&self.file(spec), spec.size);
        if let Some(llama) = &spec.llama {
            needed += left(&self.mmproj(spec, llama), llama.mmproj.size);
        }
        needed
    }

    /// The bytes a download still writes: what is left of the model's files, and for a
    /// sherpa-onnx archive room for its unpacked files, half as much again as the archive
    /// (GigaAM v3 unpacks to 1.36 times its archive).
    fn space_needed(&self, spec: &ModelSpec) -> u64 {
        let unpacked = if spec.sherpa.is_some() { spec.size * 3 / 2 } else { 0 };
        self.left_to_download(spec) + unpacked
    }

    /// Removes the model's folder: the ready model or what an interrupted download left.
    /// The caller makes sure no engine and no download uses it.
    pub fn delete(&self, spec: &ModelSpec) -> io::Result<()> {
        match fs::remove_dir_all(self.dir(spec)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }

    fn marker(&self, spec: &ModelSpec) -> PathBuf {
        self.dir(spec).join(MARKER)
    }

    fn is_ready(&self, spec: &ModelSpec) -> bool {
        let Ok(text) = fs::read_to_string(self.marker(spec)) else { return false };
        // The file is unchanged since it was verified, and it is the file the catalog pins.
        let unchanged = |file: &Path, marker: &Marker, sha256: &str, size: u64| {
            current_marker(file, sha256).is_ok_and(|actual| actual == *marker)
                && marker.size == size
        };
        match (&spec.sherpa, &spec.llama) {
            (Some(sherpa), _) => {
                let Ok(marker) = serde_json::from_str::<ArchiveMarker>(&text) else {
                    return false;
                };
                marker.sha256 == spec.sha256
                    && file_marks(&self.dir(spec), sherpa).is_ok_and(|files| files == marker.files)
            }
            (None, Some(llama)) => {
                let Ok(marker) = serde_json::from_str::<PairMarker>(&text) else { return false };
                let mmproj = &llama.mmproj;
                unchanged(&self.file(spec), &marker.model, &spec.sha256, spec.size)
                    && unchanged(
                        &self.mmproj(spec, llama),
                        &marker.mmproj,
                        &mmproj.sha256,
                        mmproj.size,
                    )
            }
            (None, None) => {
                let Ok(marker) = serde_json::from_str::<Marker>(&text) else { return false };
                unchanged(&self.file(spec), &marker, &spec.sha256, spec.size)
            }
        }
    }

    fn write_marker(&self, spec: &ModelSpec) -> io::Result<()> {
        let model = || current_marker(&self.file(spec), &spec.sha256);
        let text = match (&spec.sherpa, &spec.llama) {
            (Some(sherpa), _) => serde_json::to_string_pretty(&ArchiveMarker {
                sha256: spec.sha256.clone(),
                files: file_marks(&self.dir(spec), sherpa)?,
            }),
            (None, Some(llama)) => serde_json::to_string_pretty(&PairMarker {
                model: model()?,
                mmproj: current_marker(&self.mmproj(spec, llama), &llama.mmproj.sha256)?,
            }),
            (None, None) => serde_json::to_string_pretty(&model()?),
        }
        .map_err(io::Error::other)?;
        super::write_json(&self.marker(spec), &text)
    }
}

/// Size and modification time of each listed file.
fn file_marks(dir: &Path, sherpa: &SherpaSpec) -> io::Result<Vec<FileMark>> {
    sherpa
        .files()
        .into_iter()
        .map(|name| {
            let (size, modified_ns) = size_and_time(&dir.join(name))?;
            Ok(FileMark { name: name.to_string(), size, modified_ns })
        })
        .collect()
}

/// Takes the listed files out of a sherpa-onnx release archive into `dir`, by their own names.
fn unpack(archive: &Path, dir: &Path, sherpa: &SherpaSpec) -> Result<(), DownloadError> {
    download::unpack_named(archive, &sherpa.files(), |name| dir.join(name))
}

fn size_and_time(file: &Path) -> io::Result<(u64, u64)> {
    let metadata = fs::metadata(file)?;
    let modified = metadata.modified()?.duration_since(UNIX_EPOCH).map_err(io::Error::other)?;
    Ok((metadata.len(), modified.as_nanos() as u64))
}

fn current_marker(file: &Path, sha256: &str) -> io::Result<Marker> {
    let (size, modified_ns) = size_and_time(file)?;
    Ok(Marker { sha256: sha256.to_string(), size, modified_ns })
}

/// Downloads `url` to `file` unless a verified copy is already there.
fn fetch(
    agent: &Agent,
    url: &str,
    file: &Path,
    expected: Expected,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), DownloadError> {
    let in_place = fs::metadata(file).is_ok_and(|metadata| metadata.len() == expected.size);
    if !in_place || download::sha256_file(file)? != expected.sha256 {
        download::download(agent, url, file, expected, cancel, progress)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::runtimes::http::agent;
    use crate::runtimes::test_server::{Response, TestServer};
    use crate::storage::catalog::{Hardware, LlamaReply, ModelFile, Recognizer};

    fn payload() -> Vec<u8> {
        b"model weights".repeat(1000)
    }

    fn test_dir() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name =
            format!("models-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("test-artifacts").join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn setup() -> (ModelStore, ModelSpec, TestServer) {
        let full = payload();
        let server = TestServer::start(move |_| Response::new(200, full.clone()));
        let sha256 = Sha256::digest(payload()).iter().map(|b| format!("{b:02x}")).collect();
        let spec = ModelSpec {
            id: "test-model".into(),
            name: "Test".into(),
            author: "A".into(),
            license: "MIT".into(),
            file: "test.bin".into(),
            url: format!("{}/rev/test.bin", server.base),
            size: payload().len() as u64,
            sha256,
            translate: true,
            languages: None,
            hardware: Hardware::Gpu { memory_gb: 4 },
            sherpa: None,
            llama: None,
        };
        (ModelStore::new(test_dir()), spec, server)
    }

    fn projector() -> Vec<u8> {
        b"audio projector".repeat(100)
    }

    /// A llama.cpp model: the model at `/rev/model.gguf`, its projector at `/rev/mmproj.gguf`.
    fn llama_setup() -> (ModelStore, ModelSpec, TestServer) {
        let (store, spec, _) = setup();
        let server = TestServer::start(|request| match request.path.as_str() {
            "/rev/model.gguf" => Response::new(200, payload()),
            "/rev/mmproj.gguf" => Response::new(200, projector()),
            _ => Response::new(404, Vec::new()),
        });
        let spec = ModelSpec {
            file: "model.gguf".into(),
            url: format!("{}/rev/model.gguf", server.base),
            translate: false,
            llama: Some(LlamaSpec {
                mmproj: ModelFile {
                    file: "mmproj.gguf".into(),
                    url: format!("{}/rev/mmproj.gguf", server.base),
                    size: projector().len() as u64,
                    sha256: sha(&projector()),
                },
                reply: LlamaReply::Qwen3Asr,
                prompt: None,
            }),
            ..spec
        };
        (store, spec, server)
    }

    fn get(store: &ModelStore, spec: &ModelSpec) -> Result<PathBuf, DownloadError> {
        get_counting(store, spec).0
    }

    /// The download's result and how many times it reported unpacking.
    fn get_counting(store: &ModelStore, spec: &ModelSpec) -> (Result<PathBuf, DownloadError>, u32) {
        let mut unpacking = 0;
        let never = AtomicBool::new(false);
        let result = store.download(&agent(), spec, &never, &mut |_, _| {}, &mut || unpacking += 1);
        (result, unpacking)
    }

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A `.tar.bz2` like the sherpa-onnx releases: files in one top folder.
    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    const MODEL_FILES: [(&str, &[u8]); 4] = [
        ("model/encoder.int8.onnx", b"encoder"),
        ("model/decoder.int8.onnx", b"decoder"),
        ("model/joiner.int8.onnx", b"joiner"),
        ("model/tokens.txt", b"tokens"),
    ];

    fn sherpa_setup(files: &[(&str, &[u8])]) -> (ModelStore, ModelSpec, TestServer) {
        let bytes = archive(files);
        let (store, spec, _) = setup();
        let served = bytes.clone();
        let server = TestServer::start(move |_| Response::new(200, served.clone()));
        let spec = ModelSpec {
            file: "model.tar.bz2".into(),
            url: format!("{}/asr-models/model.tar.bz2", server.base),
            size: bytes.len() as u64,
            sha256: sha(&bytes),
            translate: false,
            hardware: Hardware::Cpu,
            sherpa: Some(SherpaSpec {
                recognizer: Recognizer::OfflineTransducer,
                model_type: Some("nemo_transducer".into()),
                encoder: "encoder.int8.onnx".into(),
                decoder: "decoder.int8.onnx".into(),
                joiner: "joiner.int8.onnx".into(),
                tokens: "tokens.txt".into(),
            }),
            ..spec
        };
        (store, spec, server)
    }

    #[test]
    fn download_makes_the_model_ready() {
        let (store, spec, _server) = setup();
        assert_eq!(store.state(&spec), ModelState::Missing);
        assert_eq!(store.ready(&spec), None);
        let (file, unpacking) = get_counting(&store, &spec);
        let file = file.unwrap();
        assert_eq!(unpacking, 0, "a Whisper model is not unpacked");
        assert_eq!(store.state(&spec), ModelState::Ready);
        let ready = ReadyModel { path: file.clone(), sherpa: None, llama: None };
        assert_eq!(store.ready(&spec), Some(ready));
        assert_eq!(fs::read(file).unwrap(), payload());
    }

    #[test]
    fn llama_model_comes_with_its_projector() {
        let (store, spec, _server) = llama_setup();
        let total = spec.download_size();
        let mut seen = Vec::new();
        let never = AtomicBool::new(false);
        let file = store
            .download(&agent(), &spec, &never, &mut |done, all| seen.push((done, all)), &mut || {})
            .unwrap();
        assert_eq!(store.state(&spec), ModelState::Ready);
        let ready = ReadyModel { path: file.clone(), sherpa: None, llama: spec.llama.clone() };
        assert_eq!(store.ready(&spec), Some(ready));
        assert_eq!(fs::read(&file).unwrap(), payload());
        assert_eq!(fs::read(file.with_file_name("mmproj.gguf")).unwrap(), projector());
        // One download for the interface: the projector's bytes follow the model's.
        assert!(seen.iter().all(|(done, all)| *all == total && *done <= total));
        assert_eq!(seen.last(), Some(&(total, total)));
        assert!(seen.windows(2).all(|pair| pair[0].0 <= pair[1].0));
    }

    /// Nothing on the disk and no server to answer (no network): the download fails without a
    /// word of progress, so the interface never shows it started.
    #[test]
    fn a_download_that_cannot_start_reports_no_progress() {
        let (store, spec, _server) = setup();
        // Port 1 of this computer: nothing listens there.
        let spec = ModelSpec { url: "http://127.0.0.1:1/rev/test.bin".into(), ..spec };
        let mut seen = Vec::new();
        let never = AtomicBool::new(false);
        let result = store.download(
            &agent(),
            &spec,
            &never,
            &mut |done, all| seen.push((done, all)),
            &mut || {},
        );
        assert!(result.is_err());
        assert!(seen.is_empty(), "{seen:?}");
    }

    #[test]
    fn llama_model_needs_both_files_unchanged() {
        let (store, spec, _server) = llama_setup();
        let file = get(&store, &spec).unwrap();
        fs::write(file.with_file_name("mmproj.gguf"), b"tampered").unwrap();
        // Not ready; the model's file is kept, so only the projector is left to download.
        assert_eq!(store.state(&spec), ModelState::Partial { downloaded: spec.size });
        get(&store, &spec).unwrap();
        assert_eq!(store.state(&spec), ModelState::Ready, "the projector is downloaded again");
    }

    #[test]
    fn interrupted_projector_counts_after_the_model() {
        let (store, spec, _server) = llama_setup();
        let file = store.file(&spec);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, payload()).unwrap();
        // The model whole, the projector not begun: as far as the model.
        assert_eq!(store.state(&spec), ModelState::Partial { downloaded: spec.size });
        fs::write(download::part_path(&file.with_file_name("mmproj.gguf")), b"proj").unwrap();
        assert_eq!(store.state(&spec), ModelState::Partial { downloaded: spec.size + 4 });
    }

    #[test]
    fn sherpa_archive_is_unpacked_and_removed() {
        let mut files = MODEL_FILES.to_vec();
        // A same-named file deeper inside must not replace the real one.
        files.insert(0, ("model/test_wavs/tokens.txt", b"decoy"));
        files.push(("model/test_wavs/en.wav", b"wave"));
        files.push(("model/README.md", b"readme"));
        let (store, spec, _server) = sherpa_setup(&files);
        let (dir, unpacking) = get_counting(&store, &spec);
        let dir = dir.unwrap();
        assert_eq!(unpacking, 1, "the interface hears about unpacking once");
        assert_eq!(get_counting(&store, &spec).1, 0, "a ready model is not unpacked again");
        assert_eq!(store.state(&spec), ModelState::Ready);
        assert_eq!(
            store.ready(&spec),
            Some(ReadyModel { path: dir.clone(), sherpa: spec.sherpa.clone(), llama: None })
        );
        assert_eq!(fs::read(dir.join("tokens.txt")).unwrap(), b"tokens");
        assert_eq!(fs::read(dir.join("encoder.int8.onnx")).unwrap(), b"encoder");
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let expected = [
            "decoder.int8.onnx",
            "encoder.int8.onnx",
            "joiner.int8.onnx",
            "ready.json",
            "tokens.txt",
        ];
        assert_eq!(names, expected, "only the listed files, the archive is gone");
    }

    #[test]
    fn changed_sherpa_file_is_no_longer_ready() {
        let (store, spec, _server) = sherpa_setup(&MODEL_FILES);
        let dir = get(&store, &spec).unwrap();
        fs::write(dir.join("joiner.int8.onnx"), b"tampered joiner").unwrap();
        assert_eq!(store.state(&spec), ModelState::Missing);
        fs::remove_file(dir.join("joiner.int8.onnx")).unwrap();
        assert_eq!(store.ready(&spec), None);
    }

    #[test]
    fn archive_without_a_listed_file_fails() {
        let (store, spec, _server) = sherpa_setup(&MODEL_FILES[..3]);
        let error = get(&store, &spec).unwrap_err();
        assert!(matches!(&error, DownloadError::Archive(detail) if detail.contains("tokens.txt")));
        // The archive stays, to be deleted.
        assert_eq!(store.state(&spec), ModelState::Partial { downloaded: spec.size });
    }

    #[test]
    fn copied_archive_is_unpacked_without_downloading() {
        let (store, spec, server) = sherpa_setup(&MODEL_FILES);
        let file = store.file(&spec);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, archive(&MODEL_FILES)).unwrap();
        // The interface hears where it starts, before the archive is checked and unpacked.
        let mut heard = Vec::new();
        let never = AtomicBool::new(false);
        store
            .download(
                &agent(),
                &spec,
                &never,
                &mut |done, total| heard.push((done, total)),
                &mut || {},
            )
            .unwrap();
        assert_eq!(heard.first(), Some(&(spec.size, spec.size)));
        assert_eq!(store.state(&spec), ModelState::Ready);
        assert!(server.requests().is_empty());
    }

    #[test]
    fn interrupted_download_is_partial() {
        let (store, spec, _server) = setup();
        let file = store.file(&spec);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(download::part_path(&file), b"model").unwrap();
        assert_eq!(store.state(&spec), ModelState::Partial { downloaded: 5 });
    }

    /// An archive downloaded whole but not unpacked (the disk was full) can be finished or
    /// deleted: it is no missing model.
    #[test]
    fn an_archive_not_unpacked_is_partial() {
        let (store, spec, _server) = sherpa_setup(&MODEL_FILES);
        let file = store.file(&spec);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, vec![0; spec.size as usize]).unwrap();
        assert_eq!(store.state(&spec), ModelState::Partial { downloaded: spec.size });
    }

    /// The space a download needs: what is left of its files, and room to unpack an archive.
    #[test]
    fn space_is_needed_for_what_is_left_and_the_unpacked_files() {
        let (store, spec, _server) = setup();
        assert_eq!(store.space_needed(&spec), spec.size);
        let file = store.file(&spec);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(download::part_path(&file), b"model").unwrap();
        assert_eq!(store.space_needed(&spec), spec.size - 5);
        fs::write(&file, payload()).unwrap();
        assert_eq!(store.space_needed(&spec), 0);
        let (store, spec, _server) = sherpa_setup(&MODEL_FILES);
        assert_eq!(store.space_needed(&spec), spec.size + spec.size * 3 / 2, "and its files");
    }

    #[test]
    fn deleted_model_is_missing() {
        let (store, spec, _server) = setup();
        let file = get(&store, &spec).unwrap();
        store.delete(&spec).unwrap();
        assert_eq!(store.state(&spec), ModelState::Missing);
        assert!(!file.parent().unwrap().exists(), "the whole folder is gone");
        store.delete(&spec).expect("deleting a missing model is fine");

        let (store, spec, _server) = sherpa_setup(&MODEL_FILES);
        get(&store, &spec).unwrap();
        store.delete(&spec).unwrap();
        assert_eq!(store.state(&spec), ModelState::Missing);
    }

    #[test]
    fn deleted_partial_download_is_missing() {
        let (store, spec, _server) = setup();
        let file = store.file(&spec);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(download::part_path(&file), b"model").unwrap();
        store.delete(&spec).unwrap();
        assert_eq!(store.state(&spec), ModelState::Missing);
    }

    #[test]
    fn changed_file_is_no_longer_ready() {
        let (store, spec, _server) = setup();
        let file = get(&store, &spec).unwrap();
        fs::write(&file, b"tampered").unwrap();
        assert_eq!(store.state(&spec), ModelState::Missing);
    }

    #[test]
    fn copied_file_is_verified_without_downloading() {
        let (store, spec, server) = setup();
        let file = store.file(&spec);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, payload()).unwrap();
        assert_eq!(store.state(&spec), ModelState::Missing, "not verified yet");
        get(&store, &spec).unwrap();
        assert_eq!(store.state(&spec), ModelState::Ready);
        assert!(server.requests().is_empty());
    }

    /// Real download from the catalog source into the development cache `runtimes/models`;
    /// `AHEM_MODEL_ID` selects another catalog model (a sherpa-onnx archive is unpacked there).
    #[test]
    #[ignore = "downloads 1.6 GB: cargo test -- --ignored downloads_turbo"]
    fn downloads_turbo_from_hugging_face() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("runtimes").join("models");
        let store = ModelStore::new(root);
        let id = std::env::var("AHEM_MODEL_ID").unwrap_or_else(|_| "whisper-large-v3-turbo".into());
        let spec = crate::storage::catalog::find(&id).unwrap();
        let mut reported = 0;
        let started = std::time::Instant::now();
        let mut progress = |done, total| {
            let percent = done * 100 / total;
            if percent >= reported + 10 {
                reported = percent;
                println!("{percent}%");
            }
        };
        let mut unpacking = || println!("unpacking after {:.1} s", started.elapsed().as_secs_f64());
        let file = store
            .download(&agent(), &spec, &AtomicBool::new(false), &mut progress, &mut unpacking)
            .unwrap();
        println!("done after {:.1} s", started.elapsed().as_secs_f64());
        assert_eq!(store.state(&spec), ModelState::Ready);
        println!("ready: {}", file.display());
    }

    #[test]
    fn wrong_copied_file_is_replaced() {
        let (store, spec, server) = setup();
        let file = store.file(&spec);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, vec![0; payload().len()]).unwrap();
        get(&store, &spec).unwrap();
        assert_eq!(fs::read(&file).unwrap(), payload());
        assert_eq!(server.requests().len(), 1);
    }
}
