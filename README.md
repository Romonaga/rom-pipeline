# ROM Pipeline

ROM Pipeline is a resumable ROM-processing service with a local configuration
and status screen.

## Direction

- Rust owns configuration, state, validation, orchestration, and system adapters.
- A local web interface configures source contents, output format, paths,
  batch size, and start/stop/resume behavior.
- Each console or media family is implemented as an adapter behind the same
  safety and lifecycle rules.

See [Architecture](docs/ARCHITECTURE.md) and
[Configuration](docs/CONFIGURATION.md).

## Workspace

- `crates/rom-pipeline-core`: configuration and domain rules
- `crates/rom-pipeline-wiiu`: Wii U inventory, extraction, decryption, packaging,
  and validation adapter
- `crates/rom-pipeline-gamecube`: manifest-backed GameCube ISO inventory,
  lossless RVZ compression, and full round-trip validation adapter
- `crates/rom-pipeline-nintendo-3ds`: manifest-backed 3DS ZIP extraction,
  decrypted CCI inspection, installable CIA creation, and content validation
- `crates/rom-pipeline-psp`: PSP ISO identity validation, exact-duplicate
  grouping, CHD creation, and full round-trip validation adapter
- `crates/rom-pipeline-ps2`: manifest-backed PS2 ISO/raw Mode 2 inventory,
  compatibility-first CHD creation, and full round-trip validation adapter
- `crates/rom-pipeline-service`: bounded runner and systemd controls
- `crates/rom-pipeline-web`: loopback configuration and live-status screen
- `crates/rom-pipeline-cli`: command-line entry point
- `config/profiles.example.toml`: example configuration format

## Requirements

- Rust 1.86 or newer
- `7z` for archive extraction
- `chdman` for PSP and PS2 CHD conversion and verification
- `dolphin-tool` for GameCube RVZ conversion and verification
- `3dsconv`, `ctrtool`, Python 3, and `7z` for Nintendo 3DS CIA creation
- Adapter-specific tools such as CDecrypt and ZArchive for Wii U processing

ROM Pipeline does not include game data, console keys, firmware, or third-party
decryption and packaging tools. Use it only with content you are legally
entitled to process.

## Build and install

```bash
cargo build --release --locked
mkdir -p "$HOME/.local/bin" "$HOME/.config/rom-pipeline"
install -m 0755 target/release/rom-pipeline "$HOME/.local/bin/rom-pipeline"
install -m 0644 config/profiles.example.toml \
  "$HOME/.config/rom-pipeline/config.toml"
```

Edit the copied configuration for your storage and installed tools. To install
the optional local web service:

```bash
mkdir -p "$HOME/.config/systemd/user"
install -m 0644 packaging/systemd/rom-pipeline-ui.service \
  "$HOME/.config/systemd/user/rom-pipeline-ui.service"
systemctl --user daemon-reload
systemctl --user enable --now rom-pipeline-ui.service
```

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Installed interface

The user service exposes the screen only on the local machine:

```text
http://127.0.0.1:8787
```

See [Testing](docs/TESTING.md) for the acceptance checklist.

## PSP library lifecycle

Completed PSP CHDs remain in FastDrive staging until explicitly published:

```bash
rom-pipeline publish 5 --profile psp
```

Publication copies through a `.partial` file, validates SHA-256 and CHD
integrity, atomically publishes to `library_dir`, and removes the redundant
staging copy.

Processed source ISOs are retained until a separately confirmed prune after
every remaining PSP job is complete and published:

```bash
rom-pipeline prune 5 --confirm-prune --profile psp
```

The PS2 profile uses the same staged Publish and separately guarded Prune
lifecycle. It accepts logical 2048-byte ISO images and proven single-track raw
Mode 2/2352 BIN images. Compression that saves less than the configured
threshold can preserve the original format instead.

The GameCube profile also uses this lifecycle. It creates lossless RVZ files in
FastDrive staging, verifies them with Dolphin, reconstructs each ISO and
requires its SHA-256 to match the source, then moves the source into `done`.
Publication transfers the RVZ to `library_dir`; the original ISO is retained
until the complete set is published and a separately confirmed prune is run.
