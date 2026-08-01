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

GameCube uses `source_format = "iso"` and `output_format = "rvz"`. Its settings
require the downloader's tab-separated size/name `manifest`, an absolute
`dolphin_tool` path, RVZ block size, compression codec and level, and full
round-trip verification. Source and final-library files may live on the NAS
while work, state, logs, and staged RVZ files live on FastDrive. Publish and
Prune remain independent bounded actions; Prune is unavailable until every
manifest job is complete and published.

Nintendo 3DS uses `source_format = "zip-3ds"` and `output_format = "cia"`.
Settings provide the downloader manifest and absolute paths to `7z`, Python,
`3dsconv`, and `ctrtool`. The adapter only processes cartridge images whose
decrypted main content is proven by internal hashes; it does not accept or
store console keys. ZIPs and isolated work remain on FastDrive, CIAs are staged
there, Publish transfers verified CIAs to `library_dir`, and separately
confirmed Prune is refused until the complete manifest set is published.

PSP uses an absolute `chdman` path, the `zstd` codec, 2048-byte hunks, and full
round-trip verification. Source ISOs may remain on NAS storage while work,
state, logs, and CHD output live on a faster local drive.

For PSP, `output_dir` is fast local staging and `library_dir` is the final
library home. Publish and Prune are independent bounded actions and use the
same exclusive profile lock and graceful-stop control as conversion.

PS2 uses `source_format = "disc-image"` and `output_format = "mixed"`. Its
settings require the downloader's tab-separated size/name `manifest`, an
absolute `chdman` path, a `minimum_savings_percent`, and booleans controlling
original-format preservation and full round-trip verification. Source ISO/BIN
files can remain on the NAS while work and staged outputs live on FastDrive.
As with PSP, Publish transfers verified outputs to `library_dir`; Prune is a
separate, explicitly confirmed action and is unavailable until the complete
manifest set is published.

PlayStation Vita uses `source_format = "nonpdrm-zip"` and
`output_format = "native-vita-tree"`. `source_dir` keeps the original ZIP
archive set, while `library_dir` is the mounted SD2Vita `ux0:` root. The Vita
settings contain absolute paths to `7z` and `mountpoint` plus a byte reserve
that must remain free after deployment. The adapter refuses to write when the
destination is not a real mountpoint. Vita ZIPs are never moved to `done` and
there is no source Prune action.
