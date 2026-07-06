import struct, subprocess, time

def find_pid():
    return int(subprocess.check_output(["pgrep","-f","eldenring.exe"]).split()[0])
pid = find_pid()
mem = open(f"/proc/{pid}/mem","rb",buffering=0)
def rq(a):
    try:
        mem.seek(a); return struct.unpack("<Q", mem.read(8))[0]
    except Exception: return -1

import sys
if len(sys.argv) < 2:
    sys.exit("usage: catch-endpoint.py <member_hex>  (the peer member addr from capture-endpoint.py)")
member = int(sys.argv[1], 16)  # peer member addr (per-launch ASLR, from capture-endpoint.py)
# poll member+0x130 (the transient endpoint) until non-zero, ~4s of tight polling
ep = 0
for _ in range(20000):
    ep = rq(member+0x130)
    if ep and ep != -1:
        break
if not ep or ep == -1:
    print("endpoint +0x130 stayed 0 (never caught it non-zero this window)")
else:
    vt = rq(ep)
    print(f"CAUGHT endpoint @{ep:#x}  vtable[ep]={vt:#x}  (0x143277750 = MTInternalThreadSteamConnection)")
    print(f"  [ep+0x18](transport_ctx)={rq(ep+0x18):#x}")
    print(f"  [ep+0x20]={rq(ep+0x20):#x}  [ep+0x28]={rq(ep+0x28):#x}")
    print(f"  [ep+0x60](token_src)={rq(ep+0x60):#x}")
    print(f"  [ep+0x128]={rq(ep+0x128):#x}  [ep+0x130]={rq(ep+0x130):#x}  [ep+0x138](key?)={rq(ep+0x138):#x}")
    print(f"  [ep+0x90](queue?)={rq(ep+0x90):#x}")
    # follow transport ctx
    ctx = rq(ep+0x18)
    if ctx and ctx!=-1:
        print(f"  transport_ctx @{ctx:#x} vtable={rq(ctx):#x} [ctx+0x138]={rq(ctx+0x138):#x} [ctx+0x128]={rq(ctx+0x128):#x}")
mem.close()
