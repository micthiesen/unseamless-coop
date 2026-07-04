#!/usr/bin/env python3
"""HW write-watchpoint + stack backtrace, to recover the damage call chain.

Arms DR0/DR7 for a 4-byte write watch on --addr across every thread of eldenring.exe
(ptrace, no sudo; kernel.yama.ptrace_scope=0 on this box). At each write trap it reads
RIP (writer) AND scans the stack from RSP for return addresses into .text, printing them
as static VAs (addr - image base) — i.e. the call chain that reached the write.

Based on unseamless-coop/scripts/re/watch-write.py (same ptrace mechanics). Defaults to
--max-hits 1 to minimize ptrace residency (Arxan + long ptrace attach can crash the game).
"""
import argparse, ctypes, os, signal, struct, subprocess, sys

libc = ctypes.CDLL("libc.so.6", use_errno=True)
PTRACE_CONT, PTRACE_ATTACH, PTRACE_DETACH = 7, 16, 17
PTRACE_POKEUSER, PTRACE_GETREGS = 6, 12
DEBUGREG_OFF = 848
RIP_OFF, RSP_OFF = 16 * 8, 19 * 8
DR7_WRITE_4B_SLOT0 = (1 << 0) | (0b01 << 16) | (0b11 << 18)
IMAGE_BASE = 0x140000000
__WALL = 0x40000000
# eldenring.exe code ranges (two .text sections), from `static.py sections`.
TEXT_RANGES = [(0x140001000, 0x1429a2c00), (0x144c0e000, 0x145e01800)]


def ptrace(request, pid, addr, data):
    libc.ptrace.restype = ctypes.c_long
    libc.ptrace.argtypes = [ctypes.c_long, ctypes.c_long, ctypes.c_void_p, ctypes.c_void_p]
    ctypes.set_errno(0)
    res = libc.ptrace(request, pid, ctypes.c_void_p(addr), ctypes.c_void_p(data))
    if res == -1 and ctypes.get_errno() != 0:
        raise OSError(ctypes.get_errno(), os.strerror(ctypes.get_errno()))
    return res


def find_pid():
    out = subprocess.run(["pgrep", "-f", "[e]ldenring.exe"], capture_output=True, text=True)
    pids = [int(x) for x in out.stdout.split()]
    if not pids:
        sys.exit("no eldenring.exe process found. Is the game running?")
    return pids[0]


def read_mem(pid, addr, size):
    with open(f"/proc/{pid}/mem", "rb") as m:
        m.seek(addr)
        return m.read(size)


def get_rip_rsp(tid):
    buf = (ctypes.c_ubyte * 256)()
    libc.ptrace.argtypes = [ctypes.c_long, ctypes.c_long, ctypes.c_void_p, ctypes.c_void_p]
    ctypes.set_errno(0)
    if libc.ptrace(PTRACE_GETREGS, tid, None, ctypes.cast(buf, ctypes.c_void_p)) == -1 and ctypes.get_errno():
        raise OSError(ctypes.get_errno(), "GETREGS failed")
    b = bytes(buf)
    return struct.unpack_from("<Q", b, RIP_OFF)[0], struct.unpack_from("<Q", b, RSP_OFF)[0]


def in_text(a):
    return any(lo <= a < hi for lo, hi in TEXT_RANGES)


def backtrace(pid, rsp, depth=20, scan=0x600):
    """Scan `scan` bytes up from RSP; return code addresses (likely return addrs), in order."""
    try:
        raw = read_mem(pid, rsp, scan)
    except OSError:
        return []
    out, seen = [], set()
    for i in range(0, len(raw) - 8, 8):
        v = struct.unpack_from("<Q", raw, i)[0]
        if in_text(v) and v not in seen:
            seen.add(v)
            out.append(v)
            if len(out) >= depth:
                break
    return out


def arm(tid, addr):
    ptrace(PTRACE_ATTACH, tid, 0, 0)
    os.waitpid(tid, __WALL)
    ptrace(PTRACE_POKEUSER, tid, DEBUGREG_OFF + 0 * 8, addr)
    ptrace(PTRACE_POKEUSER, tid, DEBUGREG_OFF + 7 * 8, DR7_WRITE_4B_SLOT0)
    ptrace(PTRACE_CONT, tid, 0, 0)


def disarm(tid):
    try:
        ptrace(PTRACE_POKEUSER, tid, DEBUGREG_OFF + 7 * 8, 0)
        ptrace(PTRACE_DETACH, tid, 0, 0)
    except OSError:
        pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--addr", type=lambda s: int(s, 0), required=True)
    ap.add_argument("--pid", type=int, default=None)
    ap.add_argument("--max-hits", type=int, default=1)
    args = ap.parse_args()
    pid = args.pid or find_pid()
    addr = args.addr
    print(f"watch+bt: pid={pid} addr={addr:#x} max_hits={args.max_hits}", file=sys.stderr)

    threads = [int(t) for t in os.listdir(f"/proc/{pid}/task")]
    armed = []
    for tid in threads:
        try:
            arm(tid, addr); armed.append(tid)
        except OSError as e:
            print(f"  warn arm {tid}: {e}", file=sys.stderr)
    if not armed:
        sys.exit("armed zero threads")
    print(f"armed {len(armed)}/{len(threads)} threads; hit the enemy now...", file=sys.stderr)

    hits = 0
    stop = {"f": False}
    signal.signal(signal.SIGINT, lambda *_: stop.update(f=True))
    try:
        while not stop["f"] and hits < args.max_hits:
            try:
                tid, status = os.waitpid(-1, __WALL)
            except ChildProcessError:
                break
            if os.WIFSTOPPED(status) and os.WSTOPSIG(status) == signal.SIGTRAP:
                try:
                    rip, rsp = get_rip_rsp(tid)
                    hits += 1
                    print(f"\nHIT {hits}: writer≈ just before static_va={rip - IMAGE_BASE:#x} (RIP={rip:#x})")
                    print("  stack call-chain (return addrs in .text, nearest first):")
                    for a in backtrace(pid, rsp):
                        print(f"    static_va={a - IMAGE_BASE:#x}   (runtime {a:#x})")
                    sys.stdout.flush()
                except OSError as e:
                    print(f"  regs read failed tid {tid}: {e}", file=sys.stderr)
                ptrace(PTRACE_CONT, tid, 0, 0)
            elif os.WIFSTOPPED(status):
                ptrace(PTRACE_CONT, tid, 0, os.WSTOPSIG(status))
            elif os.WIFEXITED(status) or os.WIFSIGNALED(status):
                if tid in armed:
                    armed.remove(tid)
                if not armed:
                    break
    finally:
        for tid in armed:
            disarm(tid)
        print(f"\ndetached; {hits} hit(s).", file=sys.stderr)


if __name__ == "__main__":
    main()
