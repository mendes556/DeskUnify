# Contributing to DeskUnify

DeskUnify is an Alpha fork of Lan Mouse. Keep changes focused and preserve upstream license/copyright notices. Include the operating system and architecture in reports involving input or clipboard behavior; never attach passwords, private device certificates, or unredacted personal configuration.

## Development checks

Run from the repository root. macOS/Windows core and egui development do not need GTK:

```sh
cargo fmt --all -- --check
cargo test --workspace --exclude lan-mouse-gtk --exclude lan-mouse-egui --no-default-features --locked
cargo test -p lan-mouse-egui --locked
cargo clippy --workspace --exclude lan-mouse-gtk --exclude lan-mouse-egui --no-default-features --all-targets --locked -- -D warnings
cargo clippy -p lan-mouse -p lan-mouse-egui --no-default-features --features egui --all-targets --locked -- -D warnings
```

macOS isolated acceptance (dummy input, temporary identities/configuration):

```sh
cargo build -p lan-mouse --no-default-features --features egui --locked
python3 scripts/test-cli.py target/debug/lan-mouse
python3 scripts/test-desktop.py target/debug/lan-mouse
python3 scripts/test-files.py target/debug/lan-mouse
```

File URL tests use private system pasteboards. Platform tests may require a graphical login session and local networking; record that limitation if running headlessly. These tests cannot replace physical keyboard/mouse, Finder/Explorer paste or sleep/wake verification. Real input tests require explicit system permissions and a controlled receiving machine; `dummy` readiness is not real input readiness.

Full upstream/GTK checks require platform GTK/native libraries. Product code keeps existing configuration, certificate and protocol identifiers for compatibility; do not rename them along with display text. Serialization changes require versioning. Update DOC.md/README.md when public commands or platform behavior change.

There is no automated public release on a source push. The active workflow checks core/egui on Windows and both macOS architectures; archived upstream workflows in `docs/upstream` are reference material. Avoid publishing device-specific launchers, keys or local acceptance folders.
