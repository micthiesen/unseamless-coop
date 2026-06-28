#!/usr/bin/env python3
"""Hardware write-watchpoint for rung-3 session-FSM RE (see docs/SESSION-RE-RUNBOOK.md).

Arms an x86-64 hardware debug register (DR0/DR7) for a 4-byte *write* watch on an
absolute address in the running `eldenring.exe` and reports the RIP of each writing
instruction. This is the "cheap runtime confirm" from docs/SESSION-RE-FINDINGS.md
> "The cheap runtime confirm": find the instruction that stores `lobby_state`
(`CSSessionManager + 0xc`) on the first `None -> TryToCreateSession` (host) /
`None -> TryToJoinSession` (joiner) edge, then walk it back to its function prologue
to fill `SESSION_CREATE_SITE` / `SESSION_JOIN_SITE` in coop/session_probe.rs.

Why hardware (not Frida): the runtime confirm proved the exe loads at its preferred
base 0x140000000 under Wine, so a Linux-native ptrace hardware watchpoint works with
no instrumentation in the process. Debug registers are *per task*, so this attaches
to *every* thread of the pid and arms DR0 on each — the FSM store can land on any
game thread.

ptrace caveat: Yama `ptrace_scope=1` (this box) forbids same-uid attach to a
non-descendant, so run this as **root**:
    ~/.confirm-sudo.sh python3 scripts/re/watch-write.py --watch-lobby
(or `echo 0 | sudo tee /proc/sys/kernel/yama/ptrace_scope` first, then run as you).

RIP nuance: a data (write) breakpoint is a *trap*, so the reported RIP is the
instruction **after** the store — the writer is the instruction immediately before
RIP. Disassemble a few bytes back from `rip - 0x140000000` to find the store, then
take its `.pdata`-enclosing function prologue as the hook landmark.

Modes:
    # read the live CSSessionManager base from the instance global G=0x143d7a4d0
    watch-write.py --read-base [--pid N]

    # read base from G, watch base+0xc (lobby_state). The common case.
    watch-write.py --watch-lobby [--pid N] [--max-hits K]

    # watch an explicit absolute address (4-byte write)
    watch-write.py --addr 0x7fffXXXXXXXc [--pid N] [--max-hits K]

pid defaults to `pgrep -f '[e]ldenring.exe'`.
"""

import argparse
import ctypes
import os
import re
import signal
import struct
import subprocess
import sys

libc = ctypes.CDLL("libc.so.6", use_errno=True)

# ptrace requests
PTRACE_CONT = 7
PTRACE_ATTACH = 16
PTRACE_DETACH = 17
PTRACE_POKEUSER = 6
PTRACE_GETREGS = 12  # x86-64: fills struct user_regs_struct
PTRACE_PEEKUSER = 3

# struct user: u_debugreg[0] lives at offset 848 on x86-64; DRi at 848 + i*8.
DEBUGREG_OFF = 848
# struct user_regs_struct field order (x86-64): rip is index 16 -> byte offset 128.
RIP_OFF = 16 * 8

# DR7: enable a local 4-byte write watch in slot 0.
#   L0  = bit 0            (local enable, slot 0)
#   RW0 = bits 16-17 = 01b (break on data write)
#   LEN0= bits 18-19 = 11b (4 bytes)
DR7_WRITE_4B_SLOT0 = (1 << 0) | (0b01 << 16) | (0b11 << 18)

IMAGE_BASE = 0x140000000
SESSION_MANAGER_GLOBAL = 0x143D7A4D0  # G: [G] is the live CSSessionManager*
LOBBY_STATE_OFF = 0xC

# waitpid flag to wait on ptrace'd tasks that aren't our real children
__WALL = 0x40000000


def ptrace(request, pid, addr, data):
    libc.ptrace.restype = ctypes.c_long
    libc.ptrace.argtypes = [ctypes.c_long, ctypes.c_long, ctypes.c_void_p, ctypes.c_void_p]
    ctypes.set_errno(0)
    res = libc.ptrace(request, pid, ctypes.c_void_p(addr), ctypes.c_void_p(data))
    err = ctypes.get_errno()
    if res == -1 and err != 0:
        raise OSError(err, os.strerror(err), f"ptrace req={request} pid={pid} addr={addr:#x}")
    return res


def find_pid():
    out = subprocess.run(
        ["pgrep", "-f", "[e]ldenring.exe"], capture_output=True, text=True
    )
    pids = [int(x) for x in out.stdout.split()]
    if not pids:
        sys.exit("no eldenring.exe process found (pgrep). Is the game running?")
    if len(pids) > 1:
        print(f"warning: multiple eldenring pids {pids}; using {pids[0]}", file=sys.stderr)
    return pids[0]


def read_mem(pid, addr, size):
    with open(f"/proc/{pid}/mem", "rb") as m:
        m.seek(addr)
        return m.read(size)


def read_base(pid):
    """[G] -> the live CSSessionManager*; 0 until the manager is constructed at boot."""
    raw = read_mem(pid, SESSION_MANAGER_GLOBAL, 8)
    return struct.unpack("<Q", raw)[0]


def list_threads(pid):
    return [int(t) for t in os.listdir(f"/proc/{pid}/task")]


def get_rip(tid):
    buf = (ctypes.c_ubyte * 256)()
    libc.ptrace.argtypes = [ctypes.c_long, ctypes.c_long, ctypes.c_void_p, ctypes.c_void_p]
    ctypes.set_errno(0)
    res = libc.ptrace(PTRACE_GETREGS, tid, None, ctypes.cast(buf, ctypes.c_void_p))
    if res == -1 and ctypes.get_errno() != 0:
        raise OSError(ctypes.get_errno(), "PTRACE_GETREGS failed")
    return struct.unpack_from("<Q", bytes(buf), RIP_OFF)[0]


def arm_thread(tid, addr):
    """Attach to one thread and arm DR0/DR7 for a 4-byte write watch on addr."""
    ptrace(PTRACE_ATTACH, tid, 0, 0)
    os.waitpid(tid, __WALL)  # wait for the attach-stop
    ptrace(PTRACE_POKEUSER, tid, DEBUGREG_OFF + 0 * 8, addr)            # DR0 = addr
    ptrace(PTRACE_POKEUSER, tid, DEBUGREG_OFF + 7 * 8, DR7_WRITE_4B_SLOT0)  # DR7
    ptrace(PTRACE_CONT, tid, 0, 0)


def disarm_thread(tid):
    try:
        ptrace(PTRACE_POKEUSER, tid, DEBUGREG_OFF + 7 * 8, 0)  # clear DR7
        ptrace(PTRACE_DETACH, tid, 0, 0)
    except OSError:
        pass


def watch(pid, addr, max_hits):
    if addr % 4 != 0:
        print(f"warning: addr {addr:#x} is not 4-byte aligned; a LEN=4 watch needs alignment",
              file=sys.stderr)
    print(f"watch: pid={pid} addr={addr:#x} 4-byte write, max_hits={max_hits}", file=sys.stderr)
    threads = list_threads(pid)
    armed = []
    for tid in threads:
        try:
            arm_thread(tid, addr)
            armed.append(tid)
        except OSError as e:
            print(f"  warn: could not arm tid {tid}: {e}", file=sys.stderr)
    if not armed:
        sys.exit("armed zero threads — attach failed (run as root? ptrace_scope?)")
    print(f"armed {len(armed)}/{len(threads)} threads; waiting for writes "
          f"(Ctrl-C to stop)...", file=sys.stderr)

    hits = 0
    stop = {"flag": False}
    signal.signal(signal.SIGINT, lambda *_: stop.update(flag=True))
    try:
        while not stop["flag"] and hits < max_hits:
            try:
                tid, status = os.waitpid(-1, __WALL)
            except ChildProcessError:
                break
            if os.WIFSTOPPED(status) and os.WSTOPSIG(status) == signal.SIGTRAP:
                try:
                    rip = get_rip(tid)
                    static_va = rip - IMAGE_BASE
                    hits += 1
                    # RIP is the instruction AFTER the store; the writer is just before it.
                    print(f"\nHIT {hits}: tid={tid}  RIP={rip:#x}  "
                          f"static_va(rip-imgbase)={static_va:#x}  "
                          f"writer≈ just before {static_va:#x}")
                    sys.stdout.flush()
                except OSError as e:
                    print(f"  (could not read regs for tid {tid}: {e})", file=sys.stderr)
                # re-arm DR6 is auto-cleared by hw; just continue this thread
                ptrace(PTRACE_CONT, tid, 0, 0)
            elif os.WIFSTOPPED(status):
                # forward other signals transparently
                ptrace(PTRACE_CONT, tid, 0, os.WSTOPSIG(status))
            elif os.WIFEXITED(status) or os.WIFSIGNALED(status):
                if tid in armed:
                    armed.remove(tid)
                if not armed:
                    break
    finally:
        for tid in armed:
            disarm_thread(tid)
        print(f"\ndetached; {hits} hit(s).", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--pid", type=int, default=None)
    ap.add_argument("--read-base", action="store_true",
                    help="print the live CSSessionManager base ([G]) and exit")
    ap.add_argument("--watch-lobby", action="store_true",
                    help="watch base+0xc (lobby_state); reads base from G")
    ap.add_argument("--addr", type=lambda s: int(s, 0), default=None,
                    help="explicit absolute address to watch (4-byte write)")
    ap.add_argument("--peek", type=lambda s: int(s, 0), default=None,
                    help="read --peek-len bytes at this absolute address and exit (no watch)")
    ap.add_argument("--peek-len", type=int, default=1)
    ap.add_argument("--max-hits", type=int, default=20)
    args = ap.parse_args()

    pid = args.pid or find_pid()

    if args.peek is not None:
        data = read_mem(pid, args.peek, args.peek_len)
        hexs = " ".join(f"{b:02x}" for b in data)
        print(f"[{args.peek:#x}] = {hexs}"
              + (f"   (byte0 = {data[0]})" if data else ""))
        return

    if args.read_base:
        base = read_base(pid)
        if base == 0:
            print("base = 0 (CSSessionManager not constructed yet — boot to title first)")
        else:
            print(f"CSSessionManager base = {base:#x}  (lobby_state @ {base + LOBBY_STATE_OFF:#x})")
        return

    if args.watch_lobby:
        base = read_base(pid)
        if base == 0:
            sys.exit("base = 0 — CSSessionManager not live yet; boot to title before watching.")
        addr = base + LOBBY_STATE_OFF
        print(f"CSSessionManager base = {base:#x}; watching lobby_state @ {addr:#x}",
              file=sys.stderr)
        watch(pid, addr, args.max_hits)
        return

    if args.addr is not None:
        watch(pid, args.addr, args.max_hits)
        return

    ap.error("pick a mode: --read-base | --watch-lobby | --addr ADDR")


if __name__ == "__main__":
    main()
