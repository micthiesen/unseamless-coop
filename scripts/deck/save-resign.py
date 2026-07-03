#!/usr/bin/env python3
"""Re-sign an ELDEN RING save for another Steam account.

Usage: save-resign.py <in> <out> <new-steamid64>
"""

from __future__ import annotations

import hashlib
import os
import re
import sys
import tempfile
import unittest
from pathlib import Path


# Re-derived from community save-ID tooling and confirmed against our own ER0000.uco:
# the owning SteamID64 is stored as a plaintext little-endian u64 at 0x19003B4,
# inside the general save region. The same 8-byte value can appear elsewhere in the
# file, so re-signing replaces every occurrence of the old LE value, then refreshes
# the per-slot and general-region MD5 digests the game validates. These offsets are
# format facts; this implementation is deliberately independent code.
STEAM_ID_OFFSET = 0x19003B4
STEAM_ID_RE = re.compile(r"^[0-9]{17}$")

SLOT_COUNT = 10
SLOT_STRIDE = 0x280010
SLOT_MD5_BASE = 0x300
SLOT_DATA_BASE = 0x310
SLOT_DATA_LEN = 0x280000

GENERAL_MD5_OFFSET = 0x019003A0
GENERAL_DATA_START = 0x019003B0
GENERAL_DATA_END_INCLUSIVE = 0x019603AF


def die(message: str) -> None:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def parse_steam_id(value: str, label: str) -> int:
    if not STEAM_ID_RE.fullmatch(value):
        die(f"{label} must be a 17-digit SteamID64, got {value!r}")
    return int(value)


def steam_id_at(data: bytes | bytearray, offset: int = STEAM_ID_OFFSET) -> int:
    end = offset + 8
    if len(data) < end:
        die(f"save is too small to contain SteamID64 at 0x{offset:X}")
    return int.from_bytes(data[offset:end], "little")


def validate_embedded_id(old_id: int) -> None:
    if not STEAM_ID_RE.fullmatch(str(old_id)):
        die(
            f"SteamID64 at 0x{STEAM_ID_OFFSET:X} is not a 17-digit value "
            f"(got {old_id}); save format may have changed"
        )


def replace_all(data: bytearray, old_id: int, new_id: int) -> int:
    old_bytes = old_id.to_bytes(8, "little")
    new_bytes = new_id.to_bytes(8, "little")
    count = data.count(old_bytes)
    if count == 0:
        die(f"old SteamID64 {old_id} was not found in the save")

    start = 0
    while True:
        pos = data.find(old_bytes, start)
        if pos < 0:
            break
        data[pos : pos + 8] = new_bytes
        start = pos + 8
    return count


def require_range(data: bytes | bytearray, offset: int, length: int, label: str) -> None:
    if len(data) < offset + length:
        die(f"save is too small for {label} at 0x{offset:X} length 0x{length:X}")


def validate_stored_md5s(data: bytes | bytearray) -> None:
    for slot in range(SLOT_COUNT):
        digest_offset = SLOT_MD5_BASE + slot * SLOT_STRIDE
        data_offset = SLOT_DATA_BASE + slot * SLOT_STRIDE
        require_range(data, digest_offset, 16, f"slot {slot} MD5")
        require_range(data, data_offset, SLOT_DATA_LEN, f"slot {slot} data")
        expected = hashlib.md5(data[data_offset : data_offset + SLOT_DATA_LEN]).digest()
        if data[digest_offset : digest_offset + 16] != expected:
            die(f"slot {slot} MD5 mismatch; save may be corrupt or unsupported")

    general_len = GENERAL_DATA_END_INCLUSIVE - GENERAL_DATA_START + 1
    require_range(data, GENERAL_MD5_OFFSET, 16, "general MD5")
    require_range(data, GENERAL_DATA_START, general_len, "general region")
    expected = hashlib.md5(data[GENERAL_DATA_START : GENERAL_DATA_START + general_len]).digest()
    if data[GENERAL_MD5_OFFSET : GENERAL_MD5_OFFSET + 16] != expected:
        die("general-region MD5 mismatch; save may be corrupt or unsupported")


def refresh_md5s(data: bytearray) -> None:
    for slot in range(SLOT_COUNT):
        digest_offset = SLOT_MD5_BASE + slot * SLOT_STRIDE
        data_offset = SLOT_DATA_BASE + slot * SLOT_STRIDE
        require_range(data, digest_offset, 16, f"slot {slot} MD5")
        require_range(data, data_offset, SLOT_DATA_LEN, f"slot {slot} data")
        data[digest_offset : digest_offset + 16] = hashlib.md5(
            data[data_offset : data_offset + SLOT_DATA_LEN]
        ).digest()

    general_len = GENERAL_DATA_END_INCLUSIVE - GENERAL_DATA_START + 1
    require_range(data, GENERAL_MD5_OFFSET, 16, "general MD5")
    require_range(data, GENERAL_DATA_START, general_len, "general region")
    data[GENERAL_MD5_OFFSET : GENERAL_MD5_OFFSET + 16] = hashlib.md5(
        data[GENERAL_DATA_START : GENERAL_DATA_START + general_len]
    ).digest()


def read_owner_id(src: Path) -> int:
    if not src.is_file():
        die(f"input save not found: {src}")
    data = src.read_bytes()
    validate_stored_md5s(data)
    old_id = steam_id_at(data)
    validate_embedded_id(old_id)
    return old_id


def atomic_write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    tmp_path = Path(tmp_name)
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        if tmp_path.stat().st_size != len(data):
            die("self-check failed: temp output size differs from generated save size")
        os.replace(tmp_path, path)
    except BaseException:
        try:
            tmp_path.unlink()
        except FileNotFoundError:
            pass
        raise


def resign(src: Path, dst: Path, new_id: int) -> tuple[int, int]:
    original = src.read_bytes()
    data = bytearray(original)

    old_id = steam_id_at(data)
    validate_embedded_id(old_id)
    validate_stored_md5s(data)
    if old_id == new_id:
        die(f"save is already signed for SteamID64 {new_id}")
    occurrence_count = replace_all(data, old_id, new_id)
    refresh_md5s(data)

    if len(data) != len(original):
        die("internal error: re-signing changed the save size")
    if steam_id_at(data) != new_id:
        die(f"self-check failed: SteamID64 at 0x{STEAM_ID_OFFSET:X} was not updated")
    if old_id.to_bytes(8, "little") in data:
        die(f"self-check failed: old SteamID64 {old_id} is still present")

    atomic_write(dst, data)
    if dst.stat().st_size != src.stat().st_size:
        die("self-check failed: output size differs from input size")
    return old_id, occurrence_count


def main(argv: list[str]) -> int:
    if len(argv) == 2 and argv[1] == "--self-test":
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(SelfTests)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
        return 0 if result.wasSuccessful() else 1

    if len(argv) == 3 and argv[1] == "--read-id":
        print(read_owner_id(Path(argv[2])))
        return 0

    if len(argv) != 4:
        print(__doc__.strip(), file=sys.stderr)
        print("       save-resign.py --read-id <in>", file=sys.stderr)
        print("       save-resign.py --self-test", file=sys.stderr)
        return 2

    src = Path(argv[1])
    dst = Path(argv[2])
    new_id = parse_steam_id(argv[3], "new SteamID64")

    if not src.is_file():
        die(f"input save not found: {src}")
    if src.resolve() == dst.resolve():
        die("input and output paths must differ")

    old_id, occurrence_count = resign(src, dst, new_id)
    print(f"{old_id} -> {new_id} ({occurrence_count} occurrence(s) replaced)")
    return 0


class SelfTests(unittest.TestCase):
    def test_resign_updates_owner_occurrences_and_md5_regions(self) -> None:
        old_id = 76561198004789432
        new_id = 76561198681631498
        data = bytearray(GENERAL_DATA_END_INCLUSIVE + 1)
        for i in range(len(data)):
            data[i] = i % 251

        old_bytes = old_id.to_bytes(8, "little")
        data[STEAM_ID_OFFSET : STEAM_ID_OFFSET + 8] = old_bytes
        extra_offset = 0x800
        data[extra_offset : extra_offset + 8] = old_bytes
        refresh_md5s(data)
        original_len = len(data)

        with tempfile.TemporaryDirectory(prefix="save-resign-test-") as tmp:
            src = Path(tmp) / "ER0000.uco"
            dst = Path(tmp) / "ER0000.resigned.uco"
            src.write_bytes(data)

            previous_id, occurrences = resign(src, dst, new_id)
            self.assertEqual(previous_id, old_id)
            self.assertEqual(occurrences, 2)

            out = dst.read_bytes()
            self.assertEqual(len(out), original_len)
            self.assertEqual(steam_id_at(out), new_id)
            self.assertNotIn(old_bytes, out)
            self.assertEqual(read_owner_id(dst), new_id)

            for slot in range(SLOT_COUNT):
                digest_offset = SLOT_MD5_BASE + slot * SLOT_STRIDE
                data_offset = SLOT_DATA_BASE + slot * SLOT_STRIDE
                self.assertEqual(
                    out[digest_offset : digest_offset + 16],
                    hashlib.md5(out[data_offset : data_offset + SLOT_DATA_LEN]).digest(),
                )

            general_len = GENERAL_DATA_END_INCLUSIVE - GENERAL_DATA_START + 1
            self.assertEqual(
                out[GENERAL_MD5_OFFSET : GENERAL_MD5_OFFSET + 16],
                hashlib.md5(out[GENERAL_DATA_START : GENERAL_DATA_START + general_len]).digest(),
            )


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
