#!/usr/bin/env python3
"""Scan a live eldenring.exe's heap for objects with a given vtable pointer.

Reads /proc/<pid>/maps + /proc/<pid>/mem (same-uid, kernel.yama.ptrace_scope=0 on
this box -> no sudo). For each target VA passed on the argv, searches every
private RW (heap/anonymous) mapping for 8-byte-aligned occurrences of that value —
i.e. any live object whose first qword is that vtable. Prints the object address
(the location holding the pointer) and a short qword dump around it.

Use to answer "is class X instantiated right now?": pass its vtable VA. Example:
    python3 scripts/re/scan-vtable.py 0x143277270 0x143278020 0x143278370
(SteamServiceImpl / SteamConnectionManager / SteamConnection @DLNW3D).

Read-only; opens /proc/mem O_RDONLY, never writes. Safe to run against the live game.
"""
import struct, sys, subprocess


def find_pid():
    out = subprocess.run(["pgrep", "-f", "[e]ldenring.exe"], capture_output=True, text=True)
    pids = [int(x) for x in out.stdout.split()]
    if not pids:
        sys.exit("no eldenring.exe process found. Is the game running?")
    return pids[0]


def read_maps(pid):
    regions = []
    with open(f"/proc/{pid}/maps") as f:
        for line in f:
            parts = line.split()
            if len(parts) < 5:
                continue
            addrs, perms = parts[0], parts[1]
            # private, readable+writable, anonymous-ish (heap/data). Skip r-x code and file maps.
            if "r" not in perms or "w" not in perms:
                continue
            lo, hi = (int(x, 16) for x in addrs.split("-"))
            path = parts[5] if len(parts) > 5 else ""
            regions.append((lo, hi, path))
    return regions


def main():
    targets = [int(a, 16) for a in sys.argv[1:]]
    if not targets:
        sys.exit("usage: scan-vtable.py <vtable_va> [<vtable_va> ...]")
    pid = find_pid()
    needles = {struct.pack("<Q", t): t for t in targets}
    counts = {t: 0 for t in targets}
    mem = open(f"/proc/{pid}/mem", "rb", buffering=0)
    for lo, hi, path in read_maps(pid):
        size = hi - lo
        if size <= 0 or size > 0x40000000:
            continue
        try:
            mem.seek(lo)
            buf = mem.read(size)
        except (OSError, ValueError):
            continue
        for needle, tva in needles.items():
            start = 0
            while True:
                i = buf.find(needle, start)
                if i < 0:
                    break
                if i % 8 == 0:  # aligned -> plausible object header
                    obj = lo + i
                    counts[tva] += 1
                    if counts[tva] <= 8:
                        dump = " ".join(
                            f"{struct.unpack_from('<Q', buf, i + k * 8)[0]:#018x}"
                            for k in range(4) if i + k * 8 + 8 <= len(buf)
                        )
                        print(f"  vtable {tva:#x}  OBJ@ {obj:#x}  [{dump}]  ({path or 'anon'})")
                start = i + 1
    print("\n=== summary ===")
    for t in targets:
        print(f"  vtable {t:#x}: {counts[t]} live object(s)")
    mem.close()


if __name__ == "__main__":
    main()
