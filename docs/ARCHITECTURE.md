# Architecture

## Language and interface decision

Rust is the primary implementation language. It gives the service a single
deployable binary, explicit error handling, strong path and state modeling, and
good control over subprocesses and clean shutdown.

The configuration screen is a local web interface backed by the Rust
service. A browser-based interface works from the Linux desktop, the Odin, and
other devices without requiring a desktop toolkit on the ROM server. The first
release must bind only to loopback. Authentication and TLS are prerequisites
for access from other hosts.

## Components

1. **Core**
   - Profile and path configuration
   - Batch policy
   - Lifecycle and job state
   - Validation rules and safety invariants
2. **Inventory**
   - Detect supported input files
   - Group related files into one title job
   - Assign stable internal IDs and collision-safe display names
3. **Runner**
   - One exclusive service lock
   - Start, graceful stop, and resume
   - Bounded batches, defaulting to five completed jobs
   - Subprocess supervision and progress events
4. **System adapters**
   - Wii U NUS archives to validated WUA
   - GameCube ISO images to lossless, round-trip-verified RVZ
   - Nintendo 3DS ZIP archives containing decrypted cartridge images to
     validated, installable CIA
   - PSP ISO images to verified DVD-mode CHD, with exact duplicates grouped
   - PS2 ISO and proven raw Mode 2 disc images to verified CHD or a preserved
     original when compression is not worthwhile
   - PlayStation Vita NoNpDRM ZIPs to selected native SD2Vita deployments
   - Future adapters for other systems
5. **State store**
   - Durable job states and output fingerprints
   - Failure history and human-readable activity
   - The first adapter reads and writes the compatible atomic TSV markers used
     by the proven shell pipeline. A database can be introduced later behind
     this boundary without changing adapters.
6. **Web interface**
   - Create and edit profiles
   - Describe what a source folder contains
   - Select output and done folders
   - Set a batch count
   - Preview inventory and estimated space
   - Start, stop, resume, and view live per-step progress

## Non-negotiable safety rules

- Never alter a source file before the final output is validated.
- Never overwrite an unrelated output.
- Write output to a partial path and publish it atomically.
- Recognize a validated output after it is transferred from staging to its
  configured final library.
- Move source material to `done` only after validation succeeds.
- Make interruption safe at every external-tool boundary.
- Keep stable internal IDs even when output filenames are human-friendly.
- Treat a batch limit as completed jobs, not attempted jobs.
- Keep system-specific commands behind adapters.
- Validate that configured filesystems respond before beginning work.
- Require removable-device destinations to be real mountpoints before writing.

## Migration plan

1. Preserve the live shell pipeline and its tested binaries. Complete.
2. Capture black-box fixtures for inventory, conversion, validation, naming,
   stop, and resume. Complete.
3. Implement read-only inventory and status in Rust. Complete.
4. Implement the Wii U adapter and prove parity on copied fixtures. Complete.
5. Add the configuration API and loopback-only web screen. Complete.
6. Install under separate `rom-pipeline-*` user services. Complete.
7. Confirm one production conversion, then remove the installed legacy system.
   Complete.

The selected implementation installs as `~/.local/bin/rom-pipeline`. The former
installed shell executable and tool directory were removed after parity was
confirmed.

## GameCube compression

The GameCube adapter builds a stable inventory from the downloader's size/name
manifest. It verifies each ISO with Dolphin, creates an RVZ using Zstandard
compression without scrubbing, verifies the RVZ, reconstructs a full ISO on the
work filesystem, and requires the reconstructed SHA-256 to match the source
before recording completion.

Conversion, publication, and source pruning are separate locked, resumable
actions. Work, staged RVZ files, state, and logs can remain on FastDrive while
the source and final library remain on NAS storage. Publication uses a
same-directory partial file and atomically renames it after hash and Dolphin
verification. Pruning is refused until all manifest jobs are complete and
published, and it permanently removes an ISO only after revalidating its final
RVZ.

## Nintendo 3DS installable archives

The Nintendo 3DS adapter inventories the downloader's full ZIP manifest, so
unfinished downloads remain visible as waiting jobs and pruning cannot shrink
the known set. Each ready ZIP is CRC-tested and extracted in an isolated
FastDrive work directory. The adapter requires exactly one `.3ds` or `.cci`
image, validates its main NCCH extended-header, ExeFS, and RomFS hashes, and
normalizes only the decrypted-content crypto flags in the disposable work copy.

The normalized cartridge image is converted to CIA. ROM Pipeline independently
parses the CIA header and TMD, verifies every recorded content SHA-256, validates
the main NCCH hashes and title ID, and also runs CTRTool before atomic staging.
Conversion, final-library publication, and source-ZIP pruning are separate
locked actions. Images whose payload is encrypted or corrupt fail without
creating an output; ROM Pipeline does not accept or store console keys.

An explicit one-off migration command converts existing library CCIs to CIAs
while preserving every original CCI for cold storage.

## PlayStation Vita native deployment

The Vita adapter keeps NoNpDRM ZIPs as the archival source format and deploys
only user-selected games. It inventories stable Vita title IDs from filenames,
CRC-tests each selected ZIP, validates the native `app/TITLE_ID` layout and
required `eboot.bin`, `param.sfo`, and `work.bin`, and supports a bundled
`patch/TITLE_ID` update when present.

Deployment writes directly into an owned staging directory on the mounted
SD2Vita filesystem, verifies every extracted file against the ZIP size and CRC,
then atomically promotes title directories. It refuses a destination that is
not a real mountpoint, preserves a configurable free-space reserve, records a
per-file deployment manifest for resume and re-verification, and never moves or
prunes the source ZIP.

## PSP compression

The PSP adapter validates the UMD filesystem, parses `PARAM.SFO`, and requires
its disc ID to match `UMD_DATA.BIN`. Exact copies sharing a disc ID are grouped
into one job; different images that reuse an ID remain separate.

Conversion uses `chdman createdvd` with 2048-byte hunks and Zstandard
compression. The temporary CHD is verified, extracted back to an ISO on the
work filesystem, and compared to the source by SHA-256 before atomic
publication. Only then are all proven-identical source images moved to `done`.

PSP publication is a separate locked, resumable action. It transfers a staged
CHD into the configured final library through a same-directory partial file,
revalidates its recorded SHA-256 and CHD integrity, publishes atomically, and
then removes the redundant staging copy.

Source pruning is deliberately separate and explicitly confirmed. It is
refused until every remaining PSP job is complete and present in the final
library. Each library CHD is rehashed and verified again immediately before its
associated source ISO files are permanently removed. Durable completion markers
remain part of status inventory after those sources are gone.

## PlayStation 2 compression

The PS2 adapter builds a stable inventory from the downloader's size/name
manifest, so files that are still downloading remain visible as waiting jobs
and pruned sources do not vanish from the set total. A filename hash supplies
the internal job ID while human output names use deterministic numeric suffixes
for collisions.

Logical 2048-byte images use `chdman createdvd`. Raw 2352-byte BIN images are
accepted only after every sector proves a single Mode 2 data-track layout; the
adapter then generates an owned cue and uses `chdman createcd`. Ambiguous or
mixed/audio layouts fail safely because they require a trusted source cue.

Every CHD is verified, extracted on FastDrive, and compared byte-for-byte by
SHA-256 with its source. If the verified CHD does not meet the configured
minimum savings, the untouched ISO or BIN is staged instead. Conversion,
publication, and pruning remain separate locked actions. Pruning is refused
until all manifest jobs are complete and published.
