import struct, subprocess, sys

def find_pid():
    out = subprocess.check_output(["pgrep","-f","eldenring.exe"]).split()
    return int(out[0])

pid = find_pid()
mem = open(f"/proc/{pid}/mem","rb",buffering=0)
def rq(a):
    mem.seek(a); return struct.unpack("<Q", mem.read(8))[0]
def rd(a,n):
    mem.seek(a); return mem.read(n)

import sys
if len(sys.argv) < 2:
    sys.exit("usage: peek-socketmgr.py <socketmgr_hex>  (read it from the host-worker-drain log line)")
sm = int(sys.argv[1], 16)  # socketmgr from the worker-drain log (per-launch ASLR)
print(f"# pid={pid} socketmgr={sm:#x}")
begin = rq(sm+0xb8); end = rq(sm+0xc0); cap = rq(sm+0xc8)
print(f"conn-vector: begin[+0xb8]={begin:#x} end[+0xc0]={end:#x} cap[+0xc8]={cap:#x}")
print(f"  -> entries={(end-begin)//8}  capacity_slots={(cap-begin)//8 if cap>=begin else 'n/a'}  room_to_push={cap>end}")
# pending queue
pb = rq(sm+0xd8); pe = rq(sm+0xe0); pc = rq(sm+0xe8)
print(f"pending-queue: begin[+0xd8]={pb:#x} end[+0xe0]={pe:#x} cap[+0xe8]={pc:#x}  has_pending[+0xf0]={rd(sm+0xf0,1)[0]}")
# channel + gate fields
print(f"channel[+0x50]={struct.unpack('<i', rd(sm+0x50,4))[0]}  gate_a[+0x5c]={struct.unpack('<I', rd(sm+0x5c,4))[0]}  syn_gate_on[+0x61]={rd(sm+0x61,1)[0]}")
# the member-resolve context S=[sm+0x48], its lookup [S+0x168]
resolve = rq(sm+0x40); S = rq(sm+0x48)
print(f"resolve_cb[+0x40]={resolve:#x}  S[+0x48]={S:#x}")
if S:
    print(f"  lookup[S+0x168]={rq(S+0x168):#x}  coll[S+0x98..0xa0]={rq(S+0x98):#x}..{rq(S+0xa0):#x}")
mem.close()
