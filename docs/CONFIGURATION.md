# Configuration model

A profile answers four questions:

1. What system is this?
2. What does the source folder contain?
3. Where should validated output and completed source files go?
4. How many titles should one run complete?

The TOML representation is demonstrated in
`config/profiles.example.toml`. The configuration screen exposes the same
fields with format choices supplied by installed system adapters.

## Common fields

- `id`: stable internal profile name
- `system`: console or media family
- `source_format`: what the source folder contains
- `source_dir`: input folder
- `done_dir`: destination for successfully processed source material
- `work_dir`: temporary extraction and validation space
- `output_dir`: final output folder
- `library_dir`: optional final library folder after output is transferred from
  staging
- `output_format`: requested result
- `batch_limit`: completed titles per run, default `5`

## Adapter-specific settings

Wii U needs the Archive.org manifest, CDecrypt, and ZArchive locations.

Nintendo 3DS uses `normalize_crypto_flags = true`. This adapter only normalizes
images whose decrypted main content can already be proven by internal hashes;
it does not accept or store console keys.

PSP uses an absolute `chdman` path, the `zstd` codec, 2048-byte hunks, and full
round-trip verification. Source ISOs may remain on NAS storage while work,
state, logs, and CHD output live on a faster local drive.

For PSP, `output_dir` is fast local staging and `library_dir` is the final
library home. Publish and Prune are independent bounded actions and use the
same exclusive profile lock and graceful-stop control as conversion.

PS2 will need an explicit emulator compatibility target and a policy for ISO,
raw BIN/CUE, CHD, or ZSO. Compression savings thresholds belong to that adapter,
not the common service.
