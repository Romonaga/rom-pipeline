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

## Legacy removal

Completed after the first production base+update+DLC conversion:

1. Confirm the new WUA checksum sidecar.
2. Confirm the new completion marker is readable on a reverified inventory.
3. Remove the installed `wiiu-wua-pipeline` executable and its old libexec
   directory.
