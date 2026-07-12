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

ptrace note: runs as your **normal user, no sudo** — this box sets Yama
`kernel.yama.ptrace_scope = 0` (persisted in /etc/sysctl.d/10-ptrace.conf), which allows
same-uid attach to a non-descendant like the Steam-launched game. If a future box has
scope=1 again, either restore that sysctl or run this under `~/.confirm-sudo.sh`.

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
import time

libc = ctypes.CDLL("libc.so.6", use_errno=True)

# ptrace requests
PTRACE_CONT = 7
PTRACE_DETACH = 17
PTRACE_POKEUSER = 6
PTRACE_GETREGS = 12  # x86-64: fills struct user_regs_struct
PTRACE_PEEKUSER = 3
PTRACE_SEIZE = 0x4206
PTRACE_INTERRUPT = 0x4207

# struct user: u_debugreg[0] lives at offset 848 on x86-64; DRi at 848 + i*8.
DEBUGREG_OFF = 848
# struct user_regs_struct field order on x86-64.
REG_NAMES = (
    "r15", "r14", "r13", "r12", "rbp", "rbx", "r11", "r10", "r9", "r8",
    "rax", "rcx", "rdx", "rsi", "rdi", "orig_rax", "rip", "cs", "eflags",
    "rsp", "ss", "fs_base", "gs_base", "ds", "es", "fs", "gs",
)

# DR7: enable a local 4-byte data watch in slot 0.
#   L0  = bit 0            (local enable, slot 0)
#   RW0 = bits 16-17       (01b = write only; 11b = read OR write — x86 has no read-only)
#   LEN0= bits 18-19 = 11b (4 bytes)
DR7_WRITE_4B_SLOT0 = (1 << 0) | (0b01 << 16) | (0b11 << 18)
DR7_RW_4B_SLOT0 = (1 << 0) | (0b11 << 16) | (0b11 << 18)  # fires on read too (a stable byte => read)

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


def get_regs(tid):
    buf = (ctypes.c_ubyte * 256)()
    libc.ptrace.argtypes = [ctypes.c_long, ctypes.c_long, ctypes.c_void_p, ctypes.c_void_p]
    ctypes.set_errno(0)
    res = libc.ptrace(PTRACE_GETREGS, tid, None, ctypes.cast(buf, ctypes.c_void_p))
    if res == -1 and ctypes.get_errno() != 0:
        raise OSError(ctypes.get_errno(), "PTRACE_GETREGS failed")
    values = struct.unpack_from(f"<{len(REG_NAMES)}Q", bytes(buf))
    return dict(zip(REG_NAMES, values))


def format_dump(addr, data):
    lines = []
    for off in range(0, len(data), 16):
        chunk = data[off:off + 16]
        lines.append(f"  {addr + off:#x}: " + " ".join(f"{b:02x}" for b in chunk))
    return "\n".join(lines)


def arm_thread(tid, addr, dr7=DR7_WRITE_4B_SLOT0):
    """Attach to one thread and arm DR0/DR7 for a 4-byte data watch on addr."""
    ptrace(PTRACE_SEIZE, tid, 0, 0)
    ptrace(PTRACE_INTERRUPT, tid, 0, 0)
    os.waitpid(tid, __WALL)  # wait for the interrupt-stop
    ptrace(PTRACE_POKEUSER, tid, DEBUGREG_OFF + 0 * 8, addr)            # DR0 = addr
    ptrace(PTRACE_POKEUSER, tid, DEBUGREG_OFF + 7 * 8, dr7)             # DR7
    ptrace(PTRACE_CONT, tid, 0, 0)


def disarm_thread(tid):
    try:
        ptrace(PTRACE_POKEUSER, tid, DEBUGREG_OFF + 7 * 8, 0)  # clear DR7
        ptrace(PTRACE_DETACH, tid, 0, 0)
    except OSError:
        pass


def stop_and_disarm(armed, already_stopped):
    """Stop every traced thread before clearing DR7 and detaching."""
    stopped = set(already_stopped)
    for tid in armed:
        if tid in stopped:
            continue
        try:
            ptrace(PTRACE_INTERRUPT, tid, 0, 0)
        except OSError:
            pass
    for tid in armed:
        if tid in stopped:
            continue
        try:
            _, status = os.waitpid(tid, __WALL)
            if os.WIFSTOPPED(status):
                stopped.add(tid)
        except (ChildProcessError, OSError):
            pass
    for tid in armed:
        if tid in stopped:
            disarm_thread(tid)
        else:
            print(f"  warn: tid {tid} did not stop for DR7 cleanup", file=sys.stderr)


def watch(pid, addr, max_hits, access="write", follow_qword=0, hold_ms=0):
    dr7 = DR7_RW_4B_SLOT0 if access == "rw" else DR7_WRITE_4B_SLOT0
    if addr % 4 != 0:
        print(f"warning: addr {addr:#x} is not 4-byte aligned; a LEN=4 watch needs alignment",
              file=sys.stderr)
    print(f"watch: pid={pid} addr={addr:#x} 4-byte {access}, max_hits={max_hits}", file=sys.stderr)
    threads = list_threads(pid)
    armed = []
    for tid in threads:
        try:
            arm_thread(tid, addr, dr7)
            armed.append(tid)
        except OSError as e:
            print(f"  warn: could not arm tid {tid}: {e}", file=sys.stderr)
    if not armed:
        sys.exit("armed zero threads — attach failed (run as root? ptrace_scope?)")
    print(f"armed {len(armed)}/{len(threads)} threads; waiting for writes "
          f"(Ctrl-C to stop)...", file=sys.stderr)

    hits = 0
    stopped = set()
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
                    regs = get_regs(tid)
                    rip = regs["rip"]
                    static_va = rip - IMAGE_BASE
                    hits += 1
                    # RIP is the instruction AFTER the store; the writer is just before it.
                    print(f"\nHIT {hits}: tid={tid}  RIP={rip:#x}  "
                          f"static_va(rip-imgbase)={static_va:#x}  "
                          f"writer≈ just before {static_va:#x}")
                    watched = struct.unpack("<Q", read_mem(pid, addr, 8))[0]
                    print(f"  watched_qword={watched:#x}  "
                          f"rax={regs['rax']:#x} rbx={regs['rbx']:#x} rcx={regs['rcx']:#x} "
                          f"rdx={regs['rdx']:#x} r8={regs['r8']:#x} r9={regs['r9']:#x} "
                          f"r14={regs['r14']:#x} rsp={regs['rsp']:#x}")
                    if follow_qword and watched:
                        print(f"  pointee dump ({follow_qword:#x} bytes from {watched:#x}):")
                        print(format_dump(watched, read_mem(pid, watched, follow_qword)))
                    sys.stdout.flush()
                    if hold_ms:
                        print(f"  holding writer thread for {hold_ms}ms", file=sys.stderr)
                        time.sleep(hold_ms / 1000.0)
                except OSError as e:
                    print(f"  (could not read regs for tid {tid}: {e})", file=sys.stderr)
                if hits >= max_hits:
                    stopped.add(tid)
                    break
                # Re-arm DR6 is auto-cleared by hardware; continue this thread.
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
        stop_and_disarm(armed, stopped)
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
                    help="explicit absolute address to watch (4-byte)")
    ap.add_argument("--access", choices=["write", "rw"], default="write",
                    help="watch on write only (default) or read-or-write (rw) — rw catches reads of a "
                         "stable byte, i.e. who consults it")
    ap.add_argument("--peek", type=lambda s: int(s, 0), default=None,
                    help="read --peek-len bytes at this absolute address and exit (no watch)")
    ap.add_argument("--peek-len", type=int, default=1)
    ap.add_argument("--max-hits", type=int, default=20)
    ap.add_argument("--follow-qword", type=lambda s: int(s, 0), default=0,
                    help="on each hit, treat the watched qword as a pointer and dump this many bytes")
    ap.add_argument("--hold-ms", type=int, default=0,
                    help="hold the writer thread this many milliseconds at each hit")
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
        watch(pid, args.addr, args.max_hits, args.access, args.follow_qword, args.hold_ms)
        return

    ap.error("pick a mode: --read-base | --watch-lobby | --addr ADDR")


if __name__ == "__main__":
    main()
