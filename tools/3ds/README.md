# 3DS Archive tools

`archive_cia_catalog.py` inventories CIA files in an Archive.org item using
64 KiB range requests. It records Archive.org size and checksums plus the CIA
TMD title ID, resumes from its partial catalog, and never downloads full CIAs.

```sh
python3 tools/3ds/archive_cia_catalog.py \
  --identifier ARCHIVE_IDENTIFIER \
  --state-dir STATE_DIRECTORY \
  --workers 12
```

`download_exact_cia_replacements.sh` downloads only the rows in a reviewed
replacement manifest. Downloads use hidden resumable partial files, validate
the declared size, MD5, and SHA-1, and become visible only after verification.
Capacity checks account only for bytes still missing, so an interrupted large
collection can resume even when the full original collection no longer fits.

```sh
tools/3ds/download_exact_cia_replacements.sh \
  REPLACEMENTS.tsv DESTINATION STATE_DIRECTORY ARCHIVE_IDENTIFIER
```

The manifest columns are:

```text
title_id  failed_zip  remote_cia  size  md5  sha1
```
