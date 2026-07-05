# Endpoint-capture helper for the rung-3 joiner-member RE (docs/SESSION-DRIVE.md > "★★ MEMBER PIPELINE
# CHARTED" > "wire the endpoint").
#
# The one remaining gap for the joiner-member is the member's TRANSPORT ENDPOINT `+0x130`: our
# `drive_add_peer` lever creates a correct member (member+0x80 = the peer SteamID64) but leaves `+0x130`
# null, so the per-frame handshake pump (0x1424007e0) reads nothing and the member is dropped. `+0x130` is
# a *transient* handshake endpoint — it's set at runtime by the transport handshake path we can't reach
# offline (gate-c's `[context+0x168]` stub rejects it) and no producer triggers in our driven setup. So we
# catch it on a REAL working ERSC session instead.
#
# This tool (standalone ptrace, read-only; ERSC runs outside EAC, kernel.yama.ptrace_scope=0 → no sudo):
#   1. Enumerates the live SessionSteam (vtable 0x1431fa248) + SessionMemberSteam (0x1431fa978) objects.
#   2. Tags each member's +0x80 SteamID (host / deck / empty), and reads +0x130 (endpoint), +0x70/+0x78
#      handles, and the handshake flags +0x150..+0x153.
#   3. Prints the session's EVENT queue [+0x578/+0x580/+0x588/+0x590] and PENDING-conn queue [+0x4f0/+0x4f8].
#   4. Prints the ready-to-run `watch-write.py` commands to arm on a chosen slot's +0x130 (the endpoint
#      writer we want) and +0x80 (the identity writer, the known-good writer-trace anchor).
#
# THE CAPTURE RECIPE (real ERSC; see docs/ERSC-LIVE-CAPTURE-FINDINGS.md > "Slot lifecycle"):
#   1. `scripts/rig.sh restore` → ERSC. Michael hosts (item-driven), Deck joins → 2-player.
#   2. `python3 scripts/re/capture-endpoint.py`            # snapshot: find the Deck member + its slot addr
#   3. Deck LEAVES (host stays up) → the slot's +0x80 clears but stays allocated at a STABLE address.
#   4. `python3 scripts/re/capture-endpoint.py`            # re-snapshot: note the now-cleared slot address
#   5. Arm the writer-catch on that STABLE slot (run each in its own terminal; they self-detach on max-hits):
#        python3 scripts/re/watch-write.py --addr <slot+0x130> --access write --max-hits 5
#        python3 scripts/re/watch-write.py --addr <slot+0x80>  --access write --max-hits 5   # cross-check
#   6. Deck REJOINS → catch the RIP that writes +0x130 (the endpoint source) and +0x80. Disassemble each
#      RIP's `.pdata`-enclosing function (`python3 scripts/re/static.py fn <fn>`) to read what it does /
#      where the endpoint comes from — then reproduce that after `drive_add_peer` pops the member.
#
# NB: a full rejoin may REALLOC the member objects, so the slot address from step 4 can move — re-run this
# tool right before arming, and prefer arming while the slot is cleared-but-allocated (host still up).

import os
import struct
import subprocess
import sys

SESSION_VT = 0x1431FA248  # DLNR3D::SessionSteam
MEMBER_VT = 0x1431FA978   # DLNR3D::SessionMemberSteam

# Known SteamID64s for tagging (override via env for a different pair).
HOST_ID = int(os.environ.get("CAP_HOST_ID", "76561198004789432"))
DECK_ID = int(os.environ.get("CAP_DECK_ID", "76561198681631498"))

HERE = os.path.dirname(os.path.abspath(__file__))


def find_pid():
    out = subprocess.run(["pgrep", "-f", "eldenring.exe"], capture_output=True, text=True).stdout.split()
    pids = [int(p) for p in out]
    if not pids:
        sys.exit("no eldenring.exe process — launch the game first")
    return pids[0]


def scan_objects(vt):
    """Enumerate live objects with the given vtable via the committed scan-vtable.py (chunks the high heap)."""
    out = subprocess.run(
        [sys.executable, os.path.join(HERE, "scan-vtable.py"), hex(vt)],
        capture_output=True, text=True,
    ).stdout
    addrs = []
    for line in out.splitlines():
        if "OBJ@" in line:
            addrs.append(int(line.split("OBJ@")[1].split("[")[0].strip(), 16))
    return addrs


def tag(sid):
    if sid == 0:
        return "empty"
    if sid == HOST_ID:
        return "HOST-self"
    if sid == DECK_ID:
        return "DECK (remote)"
    return "other-peer"


def main():
    pid = find_pid()
    mem = open(f"/proc/{pid}/mem", "rb", buffering=0)

    def rq(a):
        mem.seek(a)
        return int.from_bytes(mem.read(8), "little")

    def rd(a):
        mem.seek(a)
        return int.from_bytes(mem.read(4), "little")

    def rb(a):
        mem.seek(a)
        return mem.read(1)[0]

    print(f"# eldenring.exe pid={pid}\n")

    sessions = scan_objects(SESSION_VT)
    if not sessions:
        print("no live SessionSteam — not in a formed co-op session (host solo shows none until a peer joins)")
        return
    for s in sessions:
        print(f"SessionSteam @ {s:#x}")
        print(f"  state [+0x3cc]={rd(s + 0x3cc)}  member_count [+0x68]={rd(s + 0x68)}")
        print(f"  PENDING-conn queue [+0x4f0]={rq(s + 0x4f0):#x} .. [+0x4f8]={rq(s + 0x4f8):#x}"
              f"  ({(rq(s + 0x4f8) - rq(s + 0x4f0)) // 8} entry/entries)")
        print(f"  EVENT queue buf [+0x578]={rq(s + 0x578):#x}  idx [+0x580]={rq(s + 0x580):#x}"
              f" [+0x588]={rq(s + 0x588):#x} [+0x590]={rq(s + 0x590):#x}")
        print()

    members = scan_objects(MEMBER_VT)
    print(f"{len(members)} SessionMemberSteam:")
    cleared_or_remote = []
    for i, m in enumerate(sorted(members)):
        sid = rq(m + 0x80)
        ep = rq(m + 0x130)
        h1 = rq(m + 0x70)
        h2 = rq(m + 0x78)
        flags = (rb(m + 0x150), rb(m + 0x151), rb(m + 0x152), rb(m + 0x153))
        t = tag(sid)
        star = "  <== endpoint SET" if ep != 0 else ""
        print(f"  member[{i}] @{m:#x}  +0x80={sid:#x} ({t})  +0x130(endpoint)={ep:#x}{star}"
              f"  +0x70={h1:#x} +0x78={h2:#x}  flags={flags}")
        if t in ("empty", "DECK (remote)", "other-peer"):
            cleared_or_remote.append((m, t, ep))

    # Arming hints: prefer a DECK/remote slot (to watch its endpoint get set on rejoin), else any cleared slot.
    print("\n# --- arm the writer-catch on a slot's endpoint (+0x130) + identity (+0x80) ---")
    print("# Pick the DECK slot if present (watch its +0x130 as it rejoins), else a cleared/empty slot that")
    print("# will be reused. Run each in its own terminal; they self-detach after --max-hits writes.")
    for m, t, ep in cleared_or_remote:
        print(f"#   slot @{m:#x} ({t}):")
        print(f"#     python3 scripts/re/watch-write.py --addr {m + 0x130:#x} --access write --max-hits 5   # ENDPOINT")
        print(f"#     python3 scripts/re/watch-write.py --addr {m + 0x80:#x} --access write --max-hits 5    # identity (cross-check)")
    if sessions:
        s = sessions[0]
        print(f"# Event-queue producer (what posts the add-peer event): watch the ring buffer head being published:")
        print(f"#     python3 scripts/re/watch-write.py --addr {s + 0x588:#x} --access write --max-hits 8    # event produce idx")
    mem.close()


if __name__ == "__main__":
    main()
