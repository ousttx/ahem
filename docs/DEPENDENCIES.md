# Dependencies

Direct dependencies only. ahem is licensed under GPL-3.0-only, so everything built into the app must have a license compatible with it: MIT, Apache-2.0, BSD, ISC, zlib, MPL-2.0, Unicode-3.0 or a similar permissive license. External programs run as separate processes and downloaded models are listed separately.

## Frontend (npm)

Size is the unpacked npm package size.

| Package | Version | License | Purpose | Size |
|---|---|---|---|---|
| react | 19.3.1 | MIT | UI | 0.2 MB |
| react-dom | 19.3.1 | MIT | UI rendering | 8.1 MB |
| @tauri-apps/api | 2.12.4 | Apache-2.0 OR MIT | Calls into the Rust side | 0.9 MB |
| @tauri-apps/plugin-opener | 2.7.2 | MIT OR Apache-2.0 | Opening web links (where a service hands out API keys); "Show in folder" for saved files and meeting audio | 0.01 MB |
| @tauri-apps/plugin-dialog | 2.8.3 | MIT OR Apache-2.0 | Open and save dialogs: files to transcribe, transcripts, replacements | 0.03 MB |
| i18next | 26.5.6 | MIT | Translations | 0.5 MB |
| react-i18next | 17.1.4 | MIT | Translations in React | 1.0 MB |

Development only:

| Package | Version | License | Purpose | Size |
|---|---|---|---|---|
| @tauri-apps/cli | 2.12.4 | Apache-2.0 OR MIT | Tauri build and dev commands | 0.4 MB |
| typescript | 6.0.3 | Apache-2.0 | Type checking (`tsc -b` in `npm run build`); its parser in `src/capabilities.test.ts`, which follows the commands the quick panel and the indicator call | 24.3 MB |
| vite | 8.3.5 | MIT | Frontend bundler and dev server | 2.4 MB |
| @vitejs/plugin-react | 6.1.2 | MIT | React support for Vite | 0.05 MB |
| vitest | 5.0.5 | MIT | Frontend unit tests | 2.8 MB |
| @types/react | 19.3.0 | MIT | React types | 0.4 MB |
| @types/react-dom | 19.3.0 | MIT | React DOM types | 0.03 MB |

## Rust (crates)

Versions as locked in `Cargo.lock`; size: the crate's unpacked source.

| Crate | Version | License | Purpose | Size |
|---|---|---|---|---|
| tauri | 2.12.5 | Apache-2.0 OR MIT | App framework; feature `tray-icon` for the tray (`tray-icon` 0.25.1, `muda` 0.20.0), feature `macos-private-api` for the transparent indicator and quick panel windows on macOS | 1.8 MB |
| tauri-build | 2.7.4 | Apache-2.0 OR MIT | Build script | 0.20 MB |
| tauri-plugin-opener | 2.7.2 | Apache-2.0 OR MIT | Opening web pages (ChatGPT sign-in and usage, the OS permission settings) and the data folders | 0.32 MB |
| serde | 1.0.229 | MIT OR Apache-2.0 | Serialization | 0.62 MB |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | JSON | 0.96 MB |
| sys-locale | 0.3.2 | MIT OR Apache-2.0 | OS locale for the default interface language | 0.09 MB |
| ureq | 3.5.0 | MIT OR Apache-2.0 | HTTP: cloud speech recognition, language-model services, ChatGPT sign-in and requests, model and speaker-model downloads, the local engines (`whisper-server`, `llama-server`) | 0.86 MB |
| rustls | 0.23.46 | Apache-2.0 OR ISC OR MIT | TLS with the `ring` provider | 2.0 MB |
| tauri-plugin-updater | 2.14.1 | Apache-2.0 OR MIT | Checks GitHub for a new version, downloads it, verifies its signature and replaces the program (`updates.rs`); built with `default-features = false` and the system's TLS (`native-tls`: Security on macOS, Schannel on Windows), so it brings no bundled certificates. Also brings `reqwest`, `hyper-tls`, `native-tls` (MIT OR Apache-2.0), `minisign-verify` (MIT) and, on Linux only, `openssl` (Apache-2.0) | 0.4 MB |
| sha2 | 0.10.9 | MIT OR Apache-2.0 | SHA-256 of downloads; the PKCE challenge of the ChatGPT sign-in | 0.19 MB |
| getrandom | 0.3.4 | MIT OR Apache-2.0 | Random bytes from the OS: the local engines' request path and key, multipart boundaries, connection ids, the ChatGPT sign-in (PKCE verifier, state, nonce, the installation's id); on macOS the Keychain key and the AES-GCM nonces | 0.27 MB |
| fs4 | 1.1.0 | MIT OR Apache-2.0 | Free disk space before downloads (`platform::available_space`): on Windows and Linux, and on macOS when `NSURL` gives no answer | 0.34 MB |
| tauri-plugin-global-shortcut | 2.4.1 | Apache-2.0 OR MIT | Global hotkeys with press and release (`global-hotkey` 0.8.0): Windows, macOS, X11 | 0.22 MB |
| cpal | 0.18.2 | Apache-2.0 | Microphone capture on every OS; the computer's sound for meetings (WASAPI loopback on Windows, the tap's aggregate device on macOS) | 1.2 MB |
| windows (Windows only) | 0.62.2 | MIT OR Apache-2.0 | `SendInput`, focused window and its program file, output mute (`IAudioEndpointVolume`), UI Automation, DPAPI, the clipboard (its text; every format saved and put back around a copied selection), the taskbar's theme from the registry, standard error to the app's log (`SetStdHandle`) | 110.4 MB |
| rusqlite | 0.40.2 | MIT | The history, the statistics and the transcripts in SQLite; feature `bundled` compiles SQLite 3.53.2 from `libsqlite3-sys` 0.38.2 (MIT; the SQLite source itself is in the public domain) | 0.94 MB |
| tauri-plugin-single-instance | 2.5.4 | Apache-2.0 OR MIT | A second launch shows the running app | 0.22 MB |
| tauri-plugin-autostart | 2.7.0 | Apache-2.0 OR MIT | Start at login (`auto-launch` 0.6.0, MIT: the `Run` registry key on Windows) | 0.20 MB |
| symphonia | 0.6.1 | MPL-2.0 | Decoding audio and video files in Rust: WAV, AIFF, MP3, MP4/M4A (AAC, ALAC), FLAC, Ogg Vorbis, MKV/WebM. No Opus (see the next row), AC-3, WMA | 0.09 MB |
| symphonia-adapter-libopus | 0.3.0 | MIT OR Apache-2.0 | Opus (mono, stereo) for symphonia through the reference libopus: Telegram and WhatsApp voice messages, WebM. `opusic-sys` 0.7.5 (BSD-3-Clause) compiles the bundled libopus 1.6.1 with CMake (`cmake` crate 0.1.58, MIT OR Apache-2.0); its `opus.patch` changes only how CMake picks the MSVC runtime and one define of the optional OSCE feature, which is off. On Windows `tools/with-rust.cmd` takes CMake from Build Tools | 0.05 MB |
| opusic-sys | 0.7.5 | BSD-3-Clause | The libopus encoder (the same build as `symphonia-adapter-libopus` above): meeting recordings as Ogg Opus and Opus files for tests | 29.0 MB |
| tauri-plugin-dialog | 2.8.3 | Apache-2.0 OR MIT | Open and save dialogs (`rfd` 0.16.1, `tauri-plugin-fs` 2.7.1) | 0.39 MB |
| sherpa-onnx | 2.0.0 | Apache-2.0 | Second speech engine, run in a worker process of the app, repository k2-fsa/sherpa-onnx; `sherpa-onnx-sys` 2.0.0 (Apache-2.0) links the official prebuilt libraries (see below). Its build script also brings build-only crates that never reach the app: `ureq` 2.12.1 with `webpki-roots` 1.0.11 (CDLA-Permissive-2.0; unused, the archive comes from `SHERPA_ONNX_ARCHIVE_DIR`), `zip`, `bzip2` 0.4 / `bzip2-sys` | 0.30 MB |
| sherpa-onnx-sys | 2.0.0 | Apache-2.0 | The sherpa-onnx C API directly where the `sherpa-onnx` crate does not pass something on: the speaker diarization's progress (`meetings/speakers.rs`). The same crate `sherpa-onnx` already links; adds nothing to the build | 0.22 MB |
| chrono | 0.4.45 | MIT OR Apache-2.0 | The local date and time that names a meeting's folder; only the `clock` feature, which brings `iana-time-zone` 0.1.65 (MIT OR Apache-2.0; `core-foundation-sys` on macOS, `windows-core` on Windows) and `num-traits` (MIT OR Apache-2.0). Small, already in the lock file | 1.4 MB |
| tar | 0.4.46 | MIT OR Apache-2.0 | Unpacking sherpa-onnx model archives; no default features | 0.38 MB |
| bzip2 | 0.6.1 | MIT OR Apache-2.0 | `.bz2` decompression for model archives through `libbz2-rs-sys` 0.2.5 — **license `bzip2-1.0.6`** (a permissive BSD-style license) | 0.14 MB |
| block2 (macOS only) | 0.6.2 | MIT | The block that receives the system's answer to the microphone request (`platform::macos`); already in the tree through `objc2-foundation` | 0.1 MB |
| objc2-core-foundation (macOS only) | 0.3.2 | Zlib OR Apache-2.0 OR MIT | Core Foundation strings, dictionaries and booleans for the system calls below; already in the tree through Tauri | 1.6 MB |
| objc2-core-graphics (macOS only) | 0.3.2 | Zlib OR Apache-2.0 OR MIT | Typing text with `CGEvent`, modifier state; already in the tree through Tauri | 0.90 MB |
| objc2-application-services (macOS only) | 0.3.2 | Zlib OR Apache-2.0 OR MIT | Accessibility: focused window and field, selected text, the permission check and prompt. The only crate this adds to the macOS tree | 0.55 MB |
| objc2-app-kit (macOS only) | 0.3.2 | Zlib OR Apache-2.0 OR MIT | The frontmost application (`NSWorkspace`) without the Accessibility permission, its bundle identifier and name (`NSRunningApplication`), the clipboard's text (`NSPasteboard`), the Dock icon when run without a bundle (`NSApplication`, `NSImage`), the capsule and the quick panel as panels that do not activate ahem, over full-screen programs too (`NSWindow`), the focus given back to the program in front before the quick panel (`NSRunningApplication`); already in the tree through Tauri | 5.1 MB |
| objc2 (macOS only) | 0.6.5 | MIT | Objective-C protocol objects, to put saved items back on the clipboard after a copied selection; the answer of the capsule and the quick panel in place of NSWindow's `_isNonactivatingPanel` (`method_setImplementation`); already in the tree through Tauri | 1.4 MB |
| objc2-foundation (macOS only) | 0.3.2 | MIT | Arrays and data of the saved clipboard items, the main bundle's path (`NSBundle`); the dictionaries, numbers and UUIDs of the meeting tap (`NSDictionary`, `NSValue`, `NSUUID`) and the macOS version it needs (`NSProcessInfo`); free disk space as Finder counts it (`NSURL`, `NSError`); already in the tree through Tauri | 3.3 MB |
| objc2-core-audio (macOS only) | 0.3.2 | Zlib OR Apache-2.0 OR MIT | Muting the default output device; the tap of every process and its private aggregate device that record the computer's sound for a meeting (features `objc2`, `objc2-foundation`); already in the tree through cpal | 0.28 MB |
| security-framework (macOS only) | 3.7.0 | MIT OR Apache-2.0 | The key that seals API keys, in the login Keychain; already in the tree through `rustls-platform-verifier` | 0.51 MB |
| ring | 0.17.14 | Apache-2.0 AND ISC | AES-256-GCM for sealing API keys on macOS; RS256 checks of ChatGPT's ID tokens; the same crate rustls uses on every OS | 8.2 MB |
| rsa (development only) | 0.9.10 | MIT OR Apache-2.0 | Makes the key that signs the test ID token in the ChatGPT sign-in tests; never built into the app | 0.3 MB |

Transitive dependencies were checked with `cargo metadata` for Windows, Linux and macOS on 2026-10-24 after sherpa-onnx (351, 437 and 360 crates): the only licenses not named at the top are those of `libbz2-rs-sys` (see `bzip2` above), the build-only `webpki-roots` of `sherpa-onnx-sys` (see above) and `target-lexicon` (below). The check on 2026-10-23 (458 crates after Opus) found everything else within the named licenses (including `ring`: Apache-2.0 AND ISC; about twenty Unicode-3.0 crates — `icu_*`, `zerovec`, `yoke`, `tinystr`, … — that Tauri pulls in for URL handling). One exception to note: `target-lexicon` 0.12.16 is `Apache-2.0 WITH LLVM-exception` — Apache-2.0 with an extra permission; a build-time dependency of the GTK crates on Linux only, not shipped in the app. On 2026-11-05, after the macOS platform layer, `cargo metadata` for `aarch64-apple-darwin` lists 361 crates; outside the named licenses are again only `libbz2-rs-sys` and the build-only `webpki-roots` (0.26.11 and 1.0.11). On 2026-11-12 the license of every crate in `Cargo.lock` (593 from crates.io, all targets) was read from its manifest: besides those above, two more never reach the app — `ppmd-rust` 1.5.2 (CC0-1.0 OR MIT-0), a default feature of `zip` in the build script of `sherpa-onnx-sys`, and `webpki-root-certs` 1.0.11 (CDLA-Permissive-2.0), which `rustls-platform-verifier` uses only on wasm32 and in its own tests.

## External programs and data

| Component | Version | License | Purpose | Source |
|---|---|---|---|---|
| whisper.cpp (`whisper-server` and libraries) | 1.9.5 (commit `d1be6fde`) | MIT | Speech engine, built by `tools/build-engine.ps1` (Windows, Vulkan + CPU) and `tools/build-engine.sh` (macOS, Metal + Accelerate, static, one file per program) | Official source archive, SHA-256 pinned in the scripts |
| Whisper Large v3, Large v3 Turbo, Medium (GGML) | HF revision `5359861c` | MIT | Speech models, downloaded on demand | `huggingface.co/ggerganov/whisper.cpp`, size and SHA-256 in `resources/models.json` |
| sherpa-onnx prebuilt libraries (`sherpa-onnx-v2.0.0-win-x64-static-MT-Release-lib.tar.bz2`; on macOS `…-osx-arm64-static-lib.tar.bz2` or `…-osx-x64-static-lib.tar.bz2`), linked into the app | 2.0.0 | Apache-2.0; includes ONNX Runtime (MIT, with its ThirdPartyNotices), kissfft (BSD-3-Clause) and utf8proc (MIT; its Unicode data under the Unicode license) | The sherpa-onnx engine, fetched by `tools/fetch-sherpa-onnx.ps1` / `.sh` | GitHub release `k2-fsa/sherpa-onnx` v2.0.0, SHA-256 pinned in the scripts |
| Parakeet TDT 0.6B v3, int8 (sherpa-onnx export) | `asr-models` release asset | **CC-BY-4.0** (NVIDIA; attribution required) | Speech model for the processor, 25 European languages | `github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/`, size and SHA-256 in `resources/models.json` |
| Nemotron 3.5 ASR Streaming 0.6B, 560 ms, int8 (sherpa-onnx export) | `asr-models` release asset, model 2026-06-11 | **OpenMDW-1.1** (NVIDIA) | Streaming speech model for the processor | `github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/`, size and SHA-256 in `resources/models.json` |
| GigaAM v3 E2E RNN-T and RNN-T, encoder int8 (sherpa-onnx export by its CI from Sber's `gigaam` package) | `asr-models` release assets, 2025-12-16 | MIT (Sber) | Russian speech models for the processor, with and without punctuation | `github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/`, size and SHA-256 in `resources/models.json` |
| pyannote segmentation 3.0, int8 (sherpa-onnx export, `sherpa-onnx-pyannote-segmentation-3-0.tar.bz2`) | `speaker-segmentation-models` release asset | MIT (CNRS) | Meetings: where each voice of the computer's sound speaks, downloaded on demand | `github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/`, 6 958 444 bytes, SHA-256 pinned in `meetings/speakers.rs` |
| 3D-Speaker ERes2NetV2 zh-cn common (`3dspeaker_speech_eres2netv2_sv_zh-cn_16k-common.onnx`) | `speaker-recongition-models` release asset | Apache-2.0 (3D-Speaker; ModelScope `iic/speech_eres2netv2_sv_zh-cn_16k-common`) | Meetings: telling the voices apart, downloaded on demand | `github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/`, 71 441 526 bytes, SHA-256 pinned in `meetings/speakers.rs` |
| llama.cpp (`llama-server` and libraries, Windows x64 Vulkan build; macOS arm64 Metal and x64 builds) | b11871 | MIT; its `libomp.dll` (Windows) is the LLVM OpenMP runtime (Apache-2.0 WITH LLVM-exception) | Speech engine for models with an audio projector, fetched by `tools/fetch-llama-cpp.ps1` / `.sh` | GitHub release `ggml-org/llama.cpp` b11871, SHA-256 pinned in the scripts |
| Qwen3-ASR 1.7B and 0.6B, Q8_0 with Q8_0 projectors (GGUF converted by the llama.cpp project with `convert_hf_to_gguf.py`) | HF revisions `36a67868` (1.7B) and `928ab958` (0.6B) | Apache-2.0 (Alibaba Qwen) | Speech models for llama.cpp, 30 languages | `huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF` and `-0.6B-GGUF`, sizes and SHA-256 in `resources/models.json` |

## Brand marks

Shown only to name a service on the Services page and in the list of services of the mode editor (`src/logos.ts`), in the text color; trademarks of their owners.

| Mark | Source | License |
|---|---|---|
| OpenAI / ChatGPT | OpenAI's "Sign in with ChatGPT" assets, `developers.openai.com/assets/siwc/sign-in-buttons/chatgpt-logo-white.svg` (also `src/assets/` for the sign-in button) | OpenAI brand asset, used as OpenAI's guidelines describe |
| Anthropic, Deepgram, DeepSeek | Simple Icons 16.36.0 (`simple-icons` on npm) | CC0-1.0 |
| Groq | Its "g" from LobeHub Icons 1.102.0 (`@lobehub/icons-static-svg` on npm) | MIT (the icon set); the mark is Groq's |
