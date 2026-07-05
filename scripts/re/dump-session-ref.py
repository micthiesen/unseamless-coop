# Thorough transitive dump of the live DLNR3D/DLNW3D co-op session graph, from the CSSessionManager root.
# Run on a REAL working ERSC session (standalone ptrace, no sudo) to bank ground-truth reference data:
# the container, SessionManagerSteam, SessionSteam, all members + their endpoints, the transport
# (SteamConnectionManager + SteamConnection), the players roster, and the type-5 completion token. Run it on
# BOTH the host and the client — the graphs mirror (each side's remote member holds the endpoint).
#
#   python3 scripts/re/dump-session-ref.py           # local (rig)
#   ssh deck 'python3 /tmp/dump-session-ref.py'      # client (push it first)
#
# Everything is a bounded read + logged; never writes. Offsets are the ones charted in
# docs/SESSION-DRIVE.md + ERSC-LIVE-CAPTURE-FINDINGS.md. VTABLES map is for tagging known classes.

import struct
import subprocess
import sys

G_PTR = 0x143D7A4D0  # [G_PTR] -> CSSessionManager

VTABLES = {
    0x1431FA248: "SessionSteam",
    0x1431FA978: "SessionMemberSteam",
    0x1431F8780: "ManagerImplSteam",
    0x1431F9140: "SessionManagerSteam",
    0x143278020: "SteamConnectionManager",
    0x143278358: "SteamConnection",
    0x143277270: "SteamServiceImpl",
    0x1432770B0: "MTInternalThreadSteamSocket(context)",
    0x143277750: "MTInternalThreadSteamConnection(endpoint)",
    0x143276CB8: "MTInternalThreadSteamSocketManager",
    0x1431F9280: "SocketManagerHolder",
    0x1431FA4A8: "identity-handle(arg2)",
    0x1431F85D8: "holder-ref-handle(arg1)",
}
HOST_ID = 76561198004789432
DECK_ID = 76561198681631498


def find_pid():
    out = subprocess.run(["pgrep", "-f", "eldenring.exe"], capture_output=True, text=True).stdout.split()
    if not out:
        sys.exit("no eldenring.exe")
    return int(out[0])


def main():
    pid = find_pid()
    mem = open(f"/proc/{pid}/mem", "rb", buffering=0)

    def rq(a):
        try:
            mem.seek(a); return int.from_bytes(mem.read(8), "little")
        except OSError:
            return -1

    def rd(a):
        try:
            mem.seek(a); return int.from_bytes(mem.read(4), "little")
        except OSError:
            return -1

    def tagvt(vt):
        return VTABLES.get(vt, "?")

    def tagid(sid):
        return {0: "empty", HOST_ID: "HOST", DECK_ID: "DECK"}.get(sid, f"peer?{sid:#x}" if sid else "0")

    print(f"# dump-session-ref  pid={pid}\n")

    G = rq(G_PTR)
    print(f"CSSessionManager [G]={G:#x}  lobby_state[+0xc]={rd(G+0xc)}  protocol[+0x10]={rd(G+0x10)}")
    # players roster: [G+0x78]..[G+0x80], 0x100-byte entries
    pf, pl = rq(G + 0x78), rq(G + 0x80)
    pn = (pl - pf) // 0x100 if pl >= pf and pf else 0
    print(f"  players roster [G+0x78..0x80]={pf:#x}..{pl:#x}  count={pn}")
    for i in range(min(pn, 6)):
        e = pf + i * 0x100
        # scan the entry for a plausible SteamID64 (0x0110000100000000 | acct)
        found = None
        for off in range(0, 0x100, 8):
            v = rq(e + off)
            if (v >> 52) == 0x011 or v in (HOST_ID, DECK_ID):
                found = (off, v); break
        print(f"    player[{i}] @{e:#x}  steamid≈{tagid(found[1]) if found else '?'} @+{found[0]:#x}" if found
              else f"    player[{i}] @{e:#x}  (no steamid found)")

    # container chain
    cont = rq(rq(G + 0x48) + 0x18) if rq(G + 0x48) not in (0, -1) else 0
    # fall back: container is static; derive from the session's +0x58 later if this misses
    print(f"\ncontainer(ManagerImplSteam) via [[G+0x48]+0x18]={cont:#x} vt={tagvt(rq(cont))}")
    if cont and rq(cont) == 0x1431F8780:
        print(f"  +0x48 config={rq(cont+0x48):#x}  +0x708 holder={rq(cont+0x708):#x}  +0x710 SessionMgr(embedded)={cont+0x710:#x}")
        print(f"  +0x7c0 veto/status={rd(cont+0x7c0):#x}  +0x7f8 identity={rq(cont+0x7f8):#x}")
        mgr = cont + 0x710
        arr, cap, cnt = rq(mgr + 0x18), rd(mgr + 0x20), rd(mgr + 0x24)
        print(f"  SessionMgr +0x18 arr={arr:#x} cap={cap} count={cnt}  +0xa8 id-counter={rq(mgr+0xa8):#x}")

    # sessions
    def dump_session(s, label):
        print(f"\n{label} SessionSteam @{s:#x} vt={tagvt(rq(s))}")
        print(f"  state[+0x3cc]={rd(s+0x3cc)} [+0x3d0]={rd(s+0x3d0)}  member_count[+0x68]={rd(s+0x68)}")
        print(f"  container[+0x58]={rq(s+0x58):#x}  resolver[+0x568]={rq(s+0x568):#x}")
        print(f"  pending-conn queue [+0x4f0..0x4f8]={rq(s+0x4f0):#x}..{rq(s+0x4f8):#x}")
        print(f"  member registry [+0x528]={rq(s+0x528):#x} [+0x538]={rq(s+0x538):#x}")
        print(f"  event queue buf[+0x578]={rq(s+0x578):#x} idx[+0x580/+0x588/+0x590]={rq(s+0x580):#x}/{rq(s+0x588):#x}/{rq(s+0x590):#x}")

    # find sessions + members via the scan tool
    def scan(vt):
        # reuse the scan helper next to this file — accept scan-vtable.py OR sv.py (the name it's sometimes
        # pushed under to the Deck). Missing it silently returns empty, which once caused a WRONG reading.
        import os
        here = os.path.dirname(os.path.abspath(__file__))
        sv = next((os.path.join(here, n) for n in ("scan-vtable.py", "sv.py") if os.path.exists(os.path.join(here, n))), None)
        if sv is None:
            sys.exit(f"scan helper not found next to {here} (need scan-vtable.py or sv.py)")
        out = subprocess.run([sys.executable, sv, hex(vt)], capture_output=True, text=True).stdout
        return [int(l.split("OBJ@")[1].split("[")[0].strip(), 16) for l in out.splitlines() if "OBJ@" in l]

    for s in scan(0x1431FA248):
        dump_session(s, "SESSION")

    print("\nMEMBERS (each: id / endpoint / token / flags / handles):")
    for i, m in enumerate(sorted(scan(0x1431FA978))):
        sid = rq(m + 0x80); ep = rq(m + 0x130)
        fl = tuple(rd(m + 0x150).to_bytes(4, "little")) if rd(m + 0x150) >= 0 else ()
        tok = rq(m + 0x148); f144 = rd(m + 0x144)
        print(f"  member[{i}] @{m:#x} id={tagid(sid)} ep={ep:#x} token[+0x148]={tok:#x} +0x144={f144:#x} flags={fl}"
              f" h1[+0x70]={rq(m+0x70):#x} h2[+0x78]={rq(m+0x78):#x}")
        if ep and rq(ep) != -1:
            print(f"      endpoint @{ep:#x} vt={tagvt(rq(ep))} +0x8(idx)={rq(ep+0x8):#x} +0x10={rq(ep+0x10):#x}"
                  f" +0x18={rq(ep+0x18):#x} +0x20cb={rq(ep+0x20):#x} +0x28cb={rq(ep+0x28):#x}"
                  f" +0x50(memberback)={rq(ep+0x50):#x} +0x58={rq(ep+0x58):#x} +0x60={rq(ep+0x60):#x}")

    print("\nTRANSPORT (SteamConnectionManager + SteamConnection):")
    for cm in scan(0x143278020):
        span_b, span_e = rq(cm + 0xb8), rq(cm + 0xc0)
        n = (span_e - span_b) // 8 if span_e >= span_b and span_b else 0
        print(f"  SteamConnectionManager @{cm:#x} context[+0x48]={rq(cm+0x48):#x} conn-span[+0xb8..0xc0]={span_b:#x}..{span_e:#x} ({n} conn)")
        for j in range(min(n, 4)):
            c = rq(span_b + j * 8)
            if c and rq(c) != -1:
                print(f"    conn[{j}] @{c:#x} vt={tagvt(rq(c))} peerid[+0x138]={rq(c+0x138):#x} ({tagid(rq(c+0x138))}) [+0x128]={rq(c+0x128):#x}")

    mem.close()


if __name__ == "__main__":
    main()
