# Contributing

Thanks for wanting to help. ahem is a small project made by one person, so please start with an issue (a bug, an idea or a question) before a large change.

## Before a pull request

- Run the checks from the [README](README.md#development): `npm test`, `npm run build`, `cargo test`, `cargo clippy --all-targets` and `cargo fmt`.
- A change of behavior comes with a test.
- Every text of the interface goes through `src/i18n/locales/*.json`, English first; all languages keep the same keys (a test checks it).
- Code for one operating system lives only in `src-tauri/src/platform/`.
- A new direct dependency is added to [docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) with its license, purpose and size, and its license must be compatible with GPL-3.0.
- Keep keys and dictated text out of logs.
- Architecture notes are in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## License

By submitting a contribution you agree that it is licensed under the [GPL-3.0-only](LICENSE) like the rest of the project, and that you have the right to submit it.

## Security problems

Please report them privately: see [SECURITY.md](SECURITY.md).
