# Contributing to DeckX

DeckX welcomes fixes, design improvements, and support for more programmable decks. Hardware owners can help by documenting a model, testing a prototype, or implementing its backend.

## Contribute files on GitHub

1. Select **Fork** on the DeckX repository and create a branch from `main` in your fork.
2. Open the destination folder. Choose **Add file → Upload files** to submit a file from your computer, or **Add file → Create new file** to write one in the browser. To edit an existing file, open it and select the pencil icon.
3. Commit the changes to your branch and open a pull request with `sepivip/DeckX` as the base repository and `main` as the base branch. Describe what you changed, how you checked it, and link any related issue. [GitHub’s guide to pull requests from forks](https://docs.github.com/en/pull-requests/how-tos/create-pull-requests/creating-a-pull-request-from-a-fork)

Put device notes and control maps in `docs/` and code and its tests in `src/`. Put media shipped with DeckX in `assets/` and include its source and license in the pull request.

### Work from a local checkout

Clone your fork and create a branch for your changes. This example adds `docs/my-deck.md`.

```bash
git clone https://github.com/YOUR-USERNAME/DeckX.git
cd DeckX
git switch -c document-my-deck
# Create docs/my-deck.md in your editor.
git add docs/my-deck.md
git commit -m "Document my deck"
git push -u origin document-my-deck
```

Open a pull request from that branch into `sepivip/DeckX:main`.

### Share a file in a deck support issue

If you have a deck photo, a control diagram, or a small set of example USB reports, drag the file into a comment on the deck support issue. Include the exact deck model and say what the file shows. Use material you can redistribute and remove personal data before uploading. Keep personal workspaces, credentials, vendor app bundles, and raw logs out of contributions. [GitHub’s guide to attaching files](https://docs.github.com/en/get-started/writing-on-github/working-with-advanced-formatting/attaching-files)

## Add support for a deck

1. Open a [deck support issue](https://github.com/sepivip/DeckX/issues/new?template=deck-support.yml) with the exact model, connection type, USB vendor and product IDs if available, and the controls the device exposes. Say which hardware and operating system you can test.
2. Use [`src/deck_model.rs`](src/deck_model.rs) for the device face, LCD layout, and control geometry. Keep report parsing, image uploads, and connection handling in a device backend. The existing USB HID implementation is in [`src/device.rs`](src/device.rs).
3. Reuse the shared deck preview and editors. If the device has a different number of controls, make the saved layout model-aware and verify that existing AKP03E workspaces and agent layouts still load. [Architecture notes](docs/architecture.md)
4. Add tests for input decoding and image encoding. In the pull request, report the real hardware checks you ran for connection and reconnect, key presses, dial turns, image orientation, and brightness. Include the OS and hardware revision where known.

The AKP03E notes show how to document a USB protocol and the results of hardware testing. [AKP03E protocol notes](docs/reverse-engineering.md)

## Build and test

Use Windows with Rust 1.93 or later and the MSVC toolchain. The regular test suite runs without a deck or companion apps.

```powershell
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

For UI changes, include screenshots at the default and minimum window sizes. For hardware changes, start with `cargo run --locked -- --probe` to check the current AKP03E vendor interface without writing reports. Use the relevant model’s transport tests and real hardware before marking it supported.

By submitting your own code and documentation, you agree to license it under the project’s [MIT License](LICENSE).
