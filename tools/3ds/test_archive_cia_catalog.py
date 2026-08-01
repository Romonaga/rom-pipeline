#!/usr/bin/env python3

import importlib.util
import struct
import sys
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("archive_cia_catalog.py")
SPEC = importlib.util.spec_from_file_location("archive_cia_catalog", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
CATALOG = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = CATALOG
SPEC.loader.exec_module(CATALOG)


class ParseTitleIdTests(unittest.TestCase):
    def test_reads_title_id_from_tmd(self) -> None:
        data = bytearray(64 * 1024)
        header_size = 0x2020
        cert_size = 0xA00
        ticket_size = 0x350
        tmd_size = 0xB64
        struct.pack_into("<I", data, 0, header_size)
        struct.pack_into("<I", data, 8, cert_size)
        struct.pack_into("<I", data, 12, ticket_size)
        struct.pack_into("<I", data, 16, tmd_size)
        tmd_offset = CATALOG.align_64(
            CATALOG.align_64(CATALOG.align_64(header_size) + cert_size) + ticket_size
        )
        data[tmd_offset + 0x18C : tmd_offset + 0x194] = bytes.fromhex(
            "0004000000047900"
        )

        self.assertEqual(CATALOG.parse_title_id(data), "0004000000047900")

    def test_rejects_truncated_header(self) -> None:
        with self.assertRaisesRegex(ValueError, "truncated"):
            CATALOG.parse_title_id(b"short")


if __name__ == "__main__":
    unittest.main()
