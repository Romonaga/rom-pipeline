# Acceptance testing

## Browser test

1. Open `http://127.0.0.1:8787` in Firefox or Chrome on the ROM server.
2. Confirm the selected profile shows `stopped` and no active worker.
3. Set **Titles this run** to `1`.
4. Select **Start / Resume**.
5. Confirm the badge changes to `processing`.
6. Confirm **Current** names a title and step.
7. Confirm **Worker** shows the active adapter tool while an external command is
   running.
8. Let the batch finish. The card should return to `stopped` and increment the
   completed count.
9. Confirm one additional validated output exists in the configured
   `output_dir`.
10. Confirm the title's source files moved to the configured `done_dir`.

## CLI equivalents

```bash
rom-pipeline status
rom-pipeline start 1
watch -n 5 rom-pipeline status
rom-pipeline stop
```

`stop` is graceful. It leaves the current title resumable and does not remove
source archives.

## PS2 pilot

Before processing the whole library, select representative jobs with
`inventory --profile ps2`, then run them individually with `--only`. Include a
logical DVD, a padded DVD, a large dual-layer title, and a proven raw Mode 2
BIN. For every completed pilot job:

1. Confirm the activity reports inspect, create, CHD verify, round-trip, and
   completion stages.
2. Confirm the completion marker records the staged output hash and actual
   output name.
3. Publish it and confirm the final library copy is verified and the redundant
   FastDrive copy is removed.
4. Test the final output on the target emulator before starting the full set.
5. Do not prune pilot sources; the full-manifest guard should reject pruning
   while any job remains incomplete or unpublished.

## GameCube pilot

Select one GameCube job with `inventory --profile gamecube`, then run it with
`start 1 --profile gamecube --only JOB_ID`. Confirm source verification, RVZ
creation, RVZ verification, ISO round-trip, and completion are visible in
status. Publish one job, verify the final RVZ with Dolphin, and confirm the
source ISO remains in `done`. Do not prune until the full manifest is complete
and published.

## Legacy removal

Completed after the first production base+update+DLC conversion:

1. Confirm the new WUA checksum sidecar.
2. Confirm the new completion marker is readable on a reverified inventory.
3. Remove the installed `wiiu-wua-pipeline` executable and its old libexec
   directory.
