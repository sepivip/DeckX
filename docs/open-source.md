# Publishing a DeckX release

DeckX source is licensed under [MIT](../LICENSE). Bundled media, fonts, and binary dependencies retain their own licenses. [Third-party notices](../THIRD_PARTY_NOTICES.md)

The repository contains source, build instructions, and test fixtures. Keep DeckX profiles, third-party app bundles, downloaded plugins, local screenshots, credentials, and device logs outside Git. The Grok Bot and Codex integrations read assets from locally installed apps; a DeckX build does not ship those app bundles.

For the first public release, review the branches, commit author metadata, pull requests, and Actions logs that will become public. Scan the full Git history and the release contents for credentials and private data. GitHub makes Actions history and logs public when the repository changes visibility. [GitHub visibility guide](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/managing-repository-settings/setting-repository-visibility)

## Release assets

- Build from the release commit on Windows with `cargo build --release --locked`. Include `deckx.exe`, `LICENSE`, the bundled asset notices, and the dependency licenses in the Windows ZIP.
- Upload `DeckX-Demo.mp4` and its music credit to the GitHub release. The README uses a lightweight poster with a link to the release asset.
- Run `cargo fmt --all --check`, `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`. Record the app version and any physical hardware checks separately.
- After making the repository public, enable private vulnerability reporting and confirm that the links for downloads, the demo, deck requests, and private reports work for visitors. [Private reporting setup](https://docs.github.com/en/code-security/how-tos/report-and-fix-vulnerabilities/configure-vulnerability-reporting/configure-for-a-repository)
