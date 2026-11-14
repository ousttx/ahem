# Third-party material

ahem is licensed under [GPL-3.0-only](LICENSE), © 2026 ousttx. It contains, runs or downloads the work of others. This file names it, says under which license, and gives the credit some licenses ask for. The full list of libraries with versions is in [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md); the installers also carry the license texts of every library built into the app.

Product and company names below belong to their owners. ahem is an independent project: it is not made, endorsed or sponsored by any of them. See [Trademarks](#trademarks).

## Speech models (downloaded by the app on demand, never included in it)

Each is downloaded from its official source, pinned to a revision, and checked by size and SHA-256 (`src-tauri/resources/models.json`). The license is the one of the model, not of ahem.

| Model | Made by | License | Notes |
|---|---|---|---|
| Whisper Large v3, Large v3 Turbo, Medium | OpenAI | MIT | GGML files of the whisper.cpp project |
| Qwen3-ASR 1.7B, 0.6B | Alibaba Qwen | Apache-2.0 | GGUF files made by the llama.cpp project |
| Parakeet TDT 0.6B v3 | NVIDIA | **CC-BY-4.0** | See the credit below |
| Nemotron 3.5 ASR Streaming 0.6B | NVIDIA | OpenMDW-1.1 | |
| GigaAM v3 (E2E RNN-T and RNN-T) | Sber | MIT | |
| pyannote segmentation 3.0 | CNRS | MIT | Telling voices apart in meetings and files |
| 3D-Speaker ERes2NetV2 | 3D-Speaker (Alibaba) | Apache-2.0 | Telling voices apart in meetings and files |

The sherpa-onnx models (Parakeet, Nemotron, GigaAM, pyannote, 3D-Speaker) are the exports the sherpa-onnx project publishes in its releases.

### Credit required by CC-BY-4.0

**Parakeet TDT 0.6B v3** is © NVIDIA, licensed under [CC-BY-4.0](https://creativecommons.org/licenses/by/4.0/) ([model page](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)). ahem downloads the 8-bit (int8) export of the sherpa-onnx project instead of the original files; that is the change made to the model. NVIDIA does not endorse ahem. The Models page shows the author and the license of every model in the tooltip of its name.

## Speech engines (separate programs, not linked into ahem)

| Engine | License | Source |
|---|---|---|
| whisper.cpp (`whisper-server`) | MIT | github.com/ggml-org/whisper.cpp, built from the pinned official source |
| llama.cpp (`llama-server`) | MIT | github.com/ggml-org/llama.cpp, the official build; on Windows with the LLVM OpenMP runtime (Apache-2.0 with LLVM exception) |

## Libraries linked into ahem

All of the libraries in [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) are under licenses compatible with GPL-3.0 (MIT, Apache-2.0, BSD, ISC, zlib, MPL-2.0, Unicode-3.0 or the like). The ones that ask for a notice or are worth naming:

- **sherpa-onnx** (Apache-2.0) with **ONNX Runtime** (MIT), **kissfft** (BSD-3-Clause), **utf8proc** (MIT with Unicode data) and **OpenFst** (Apache-2.0): speech recognition on the processor.
- **Tauri** (Apache-2.0 or MIT), **React** (MIT), **rustls** and **ring** (ISC, MIT, Apache-2.0 and their notices), **SQLite** (public domain) through `rusqlite` (MIT).
- **Symphonia** (MPL-2.0): decoding audio and video files. Its source is public at github.com/pdeljanov/Symphonia; ahem uses it unmodified.
- **libopus** (BSD-3-Clause): Opus audio.

## Copied data

- The list of Whisper's languages (codes and names) in `src-tauri/src/engines/languages.rs` is copied from whisper.cpp (MIT, © the ggml authors).

## Brand marks

Shown only to name a service on the Services page, in the text color. They are the property of their owners, are not under ahem's license and say nothing about any relationship with them.

| Mark | Source | License of the file |
|---|---|---|
| OpenAI and ChatGPT | OpenAI's "Sign in with ChatGPT" assets | OpenAI's brand asset, used as its guidelines describe |
| Anthropic, Deepgram, DeepSeek | [Simple Icons](https://simpleicons.org) | CC0-1.0 |
| Groq | [LobeHub Icons](https://github.com/lobehub/lobe-icons) | MIT (the icon set) |

Interface icons are drawn for ahem, or taken from [Feather Icons](https://feathericons.com) (MIT, © Cole Bemis) where marked in `src/ui.tsx`.

## Trademarks

OpenAI, ChatGPT and Whisper are trademarks of OpenAI. Claude and Anthropic are trademarks of Anthropic. Groq, Deepgram and DeepSeek are trademarks of their owners. NVIDIA, Parakeet and Nemotron are trademarks of NVIDIA. Qwen is a trademark of Alibaba. macOS is a trademark of Apple; Windows is a trademark of Microsoft. They are named only to say what ahem works with. ahem is not affiliated with, endorsed by or sponsored by them, and the services are used under their own terms with your own accounts and keys.

"Continue with ChatGPT" uses OpenAI's "Sign in with ChatGPT" for open-source and locally run apps. ahem signs in under its own name and does not present itself as OpenAI or as another application.
