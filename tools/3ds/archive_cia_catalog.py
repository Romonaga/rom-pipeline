#!/usr/bin/env python3
"""Build a resumable title-ID catalog for CIA files in an Archive.org item."""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import struct
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path


HEADER_BYTES = 64 * 1024
USER_AGENT = "rom-pipeline-cia-catalog/1.0"


@dataclass(frozen=True)
class ArchiveFile:
    name: str
    size: int
    md5: str
    sha1: str


def align_64(value: int) -> int:
    return (value + 0x3F) & ~0x3F


def parse_title_id(data: bytes) -> str:
    if len(data) < 0x20:
        raise ValueError("CIA header is truncated")
    header_size, cert_size, ticket_size, tmd_size = struct.unpack_from(
        "<I4xIII", data, 0
    )
    if header_size < 0x20 or tmd_size < 0x194:
        raise ValueError("CIA section sizes are invalid")
    cert_offset = align_64(header_size)
    ticket_offset = align_64(cert_offset + cert_size)
    tmd_offset = align_64(ticket_offset + ticket_size)
    title_end = tmd_offset + 0x194
    if title_end > len(data):
        raise ValueError(f"CIA TMD title ID is beyond {len(data)} header bytes")
    return data[tmd_offset + 0x18C : title_end].hex().upper()


def request_bytes(url: str, attempts: int = 8) -> bytes:
    error: Exception | None = None
    for attempt in range(1, attempts + 1):
        request = urllib.request.Request(
            url,
            headers={
                "Range": f"bytes=0-{HEADER_BYTES - 1}",
                "User-Agent": USER_AGENT,
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=45) as response:
                data = response.read(HEADER_BYTES)
            if len(data) != HEADER_BYTES:
                raise ValueError(f"short CIA header read: {len(data)} bytes")
            return data
        except (OSError, ValueError, urllib.error.URLError) as caught:
            error = caught
            if attempt < attempts:
                time.sleep(min(5 * attempt, 30))
    raise RuntimeError(f"header request failed after {attempts} attempts: {error}")


def fetch_metadata(identifier: str) -> dict[str, object]:
    url = f"https://archive.org/metadata/{urllib.parse.quote(identifier)}"
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)


def archive_files(metadata: dict[str, object]) -> list[ArchiveFile]:
    files = []
    for raw in metadata.get("files", []):
        if not isinstance(raw, dict):
            continue
        name = str(raw.get("name", ""))
        if not name.lower().endswith(".cia"):
            continue
        files.append(
            ArchiveFile(
                name=name,
                size=int(raw.get("size", 0)),
                md5=str(raw.get("md5", "")),
                sha1=str(raw.get("sha1", "")),
            )
        )
    return sorted(files, key=lambda item: item.name.casefold())


def load_partial(path: Path) -> dict[str, str]:
    rows: dict[str, str] = {}
    if not path.is_file():
        return rows
    for line in path.read_text(encoding="utf-8").splitlines():
        fields = line.split("\t")
        if len(fields) == 5:
            rows[fields[0]] = line
    return rows


def catalog_one(identifier: str, item: ArchiveFile) -> tuple[str, str]:
    encoded = urllib.parse.quote(item.name, safe="")
    url = f"https://archive.org/download/{identifier}/{encoded}"
    title_id = parse_title_id(request_bytes(url))
    row = "\t".join([item.name, str(item.size), item.md5, item.sha1, title_id])
    return item.name, row


def write_final(path: Path, rows: dict[str, str]) -> None:
    temporary = path.with_suffix(path.suffix + ".new")
    ordered = sorted(rows.values(), key=lambda row: row.split("\t", 1)[0].casefold())
    temporary.write_text("\n".join(ordered) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--identifier", required=True)
    parser.add_argument("--state-dir", type=Path, required=True)
    parser.add_argument("--workers", type=int, default=12)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.workers < 1 or args.workers > 32:
        raise SystemExit("--workers must be between 1 and 32")
    args.state_dir.mkdir(parents=True, exist_ok=True)
    partial = args.state_dir / "catalog.tsv.part"
    final = args.state_dir / "catalog.tsv"
    failures_path = args.state_dir / "catalog-failures.tsv"
    metadata = fetch_metadata(args.identifier)
    items = archive_files(metadata)
    rows = load_partial(partial)
    pending = [item for item in items if item.name not in rows]
    print(
        f"catalog identifier={args.identifier} total={len(items)} "
        f"resumed={len(rows)} pending={len(pending)} workers={args.workers}",
        flush=True,
    )
    lock = threading.Lock()
    failures: list[tuple[str, str]] = []
    with partial.open("a", encoding="utf-8") as output:
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as pool:
            futures = {
                pool.submit(catalog_one, args.identifier, item): item for item in pending
            }
            completed = len(rows)
            for future in concurrent.futures.as_completed(futures):
                item = futures[future]
                try:
                    name, row = future.result()
                except Exception as error:  # one failure must not discard the catalog
                    failures.append((item.name, str(error)))
                    continue
                with lock:
                    rows[name] = row
                    output.write(row + "\n")
                    output.flush()
                    completed += 1
                    if completed % 50 == 0 or completed == len(items):
                        print(f"catalog progress={completed}/{len(items)}", flush=True)
    failures_path.write_text(
        "".join(f"{name}\t{reason}\n" for name, reason in failures),
        encoding="utf-8",
    )
    if failures:
        print(f"catalog incomplete failures={len(failures)}", flush=True)
        return 1
    write_final(final, rows)
    digest = hashlib.sha256(final.read_bytes()).hexdigest()
    print(f"catalog complete files={len(rows)} sha256={digest}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
