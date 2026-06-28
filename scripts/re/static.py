# Static-RE workhorse for a CLEAN PE (eldenring.exe by default) — no GUI, no Ghidra.
#
# Consolidates the capstone+numpy techniques the session/sign RE passes kept re-inlining
# (see docs/SESSION-RE-FINDINGS.md) into one committed, importable + CLI tool. Clean-room:
# this is generic tooling over our own legitimately-owned binary; it reads metadata/bytes
# and disassembles — it never emits decompiler output. Point it at CLEAN targets only.
#
# Library use (the techniques worth reusing):
#   from static import PE
#   pe = PE()                                   # defaults to the installed eldenring.exe
#   pe.find_ascii("CS::SosSignMan::OnLeavePlayer")
#   pe.find_utf16("Menu.IsEnableOnlineMode")
#   pe.find_riprefs(0x143d7a4d0)                # rip-relative xrefs to a VA (lea/mov disp32)
#   pe.find_calls_to(0x140cabb60)               # E8 rel32 call sites targeting a VA
#   pe.func_bounds(0x140a52650)                 # .pdata enclosing-function (start,end)
#   pe.vtables_for_rtti(".?AVSosSignMan@CS@@")  # RTTI type name -> [vtable VA, ...]
#   pe.disasm(0x140a52650)                      # annotated single-function disassembly
#
# CLI:
#   python3 static.py sections
#   python3 static.py ascii  "CS::SosSignMan::"          # substring search, prints VA + string
#   python3 static.py utf16  "Menu.IsEnableOnlineMode"
#   python3 static.py xref   0x143d7a4d0                 # rip-relative xrefs (+ enclosing fn)
#   python3 static.py calls  0x140cabb60                 # E8 callers (+ enclosing fn)
#   python3 static.py vtable '.?AVSosSignMan@CS@@'       # RTTI name -> vtable(s) + first slots
#                                                        #   (quote the name: `.?AV` is a shell glob)
#   python3 static.py fn     0x140a52650                 # disassemble the .pdata function
#   python3 static.py BINARY ...                         # override target: pass a path as $RE_BIN
#
# Override the target binary with the RE_BIN env var (defaults to the Steam install path).

import os
import struct
import sys
from pathlib import Path

import numpy as np

DEFAULT_BIN = "/mnt/games/SteamLibrary/steamapps/common/ELDEN RING/Game/eldenring.exe"


class PE:
    def __init__(self, path=None, base=0x140000000):
        self.path = Path(path or os.environ.get("RE_BIN") or DEFAULT_BIN)
        self.base = base
        self.data = self.path.read_bytes()
        self._arr = np.frombuffer(self.data, dtype=np.uint8)
        self.sections = self._parse_sections()
        self.pdata = self._parse_pdata()

    # --- PE layout -------------------------------------------------------
    def _parse_sections(self):
        pe_off = struct.unpack_from("<I", self.data, 0x3C)[0]
        assert self.data[pe_off:pe_off + 4] == b"PE\0\0", "not a PE"
        coff = pe_off + 4
        nsec = struct.unpack_from("<H", self.data, coff + 2)[0]
        opt_sz = struct.unpack_from("<H", self.data, coff + 16)[0]
        sec_off = coff + 20 + opt_sz
        secs = []
        for i in range(nsec):
            o = sec_off + i * 40
            name = self.data[o:o + 8].rstrip(b"\0").decode("latin1")
            vsize, vaddr, rsize, raddr = struct.unpack_from("<IIII", self.data, o + 8)
            secs.append((name, vaddr, vsize, raddr, rsize))
        return secs

    def _parse_pdata(self):
        for name, vaddr, vsize, raddr, rsize in self.sections:
            if name == ".pdata":
                n = vsize // 12
                return np.frombuffer(self.data, dtype=np.uint32, count=n * 3, offset=raddr).reshape(-1, 3)
        return None

    def va_to_off(self, va):
        rva = va - self.base
        for _n, vaddr, vsize, raddr, rsize in self.sections:
            if vaddr <= rva < vaddr + max(vsize, rsize):
                return raddr + (rva - vaddr)
        return None

    def off_to_va(self, off):
        for _n, vaddr, vsize, raddr, rsize in self.sections:
            if raddr <= off < raddr + rsize:
                return self.base + vaddr + (off - raddr)
        return None

    def text_ranges(self):
        return [(self.base + v, self.base + v + max(vs, rs), r)
                for (n, v, vs, r, rs) in self.sections if n == ".text"]

    # --- searches --------------------------------------------------------
    def find_bytes(self, pattern: bytes):
        arr = self._arr
        pat = np.frombuffer(pattern, dtype=np.uint8)
        if len(pat) == 0:
            return []
        cand = np.where(arr == pat[0])[0]
        cand = cand[cand + len(pat) <= len(arr)]
        for i in range(1, len(pat)):
            if len(cand) == 0:
                break
            cand = cand[arr[cand + i] == pat[i]]
        return cand.tolist()

    def find_ascii(self, s: str):
        return [self.off_to_va(o) for o in self.find_bytes(s.encode("latin1"))]

    def find_utf16(self, s: str):
        return [self.off_to_va(o) for o in self.find_bytes(s.encode("utf-16-le"))]

    def cstr(self, va, n=200):
        o = self.va_to_off(va)
        if o is None:
            return ""
        e = self.data.find(b"\0", o, o + n)
        return self.data[o:e].decode("latin1", "replace") if e > 0 else ""

    def find_riprefs(self, target_va):
        """rip-relative xrefs: any 4-byte disp where site+4+disp == target_va, in .text."""
        hits = []
        for (lo, _hi, _r) in self.text_ranges():
            start = self.va_to_off(lo)
            if start is None:
                continue
            # bound the scan to this .text section's file span
            sec = next(s for s in self.sections if self.base + s[1] == lo)
            end = start + sec[4]
            seg = self._arr[start:end]
            if len(seg) < 4:
                continue
            d = seg.astype(np.int64)
            disp = (d[0:-3] | (d[1:-2] << 8) | (d[2:-1] << 16) | (d[3:] << 24))
            disp = disp.astype(np.uint32).astype(np.int32).astype(np.int64)
            base_va = self.off_to_va(start)
            pos = np.arange(len(disp))
            va = base_va + pos
            for m in np.where(va + 4 + disp == target_va)[0]:
                hits.append(self.off_to_va(start + int(m)))
        return hits

    def find_calls_to(self, target_va):
        """E8 rel32 call sites whose target == target_va (decode-free)."""
        hits = []
        for o in np.where(self._arr == 0xE8)[0]:
            v = self.off_to_va(int(o))
            if v is None:
                continue
            rel = struct.unpack_from("<i", self.data, int(o) + 1)[0]
            if v + 5 + rel == target_va:
                hits.append(v)
        return hits

    def _in_text(self, va):
        for (lo, hi, _r) in self.text_ranges():
            if lo <= va < hi:
                return (lo, hi)
        return None

    def func_bounds(self, va):
        """(start_va, end_va) of the .pdata function containing va, else None.

        Returns None for any VA outside a real .text function — including .rdata
        vtable slots and .data globals — so callers can use it as a function test.
        """
        if self.pdata is None:
            return None
        sec = self._in_text(va)
        if sec is None:
            return None
        rva = va - self.base
        starts = self.pdata[:, 0]
        idx = int(np.searchsorted(starts, rva, side="right") - 1)
        if idx < 0 or not (starts[idx] <= rva < self.pdata[idx, 1]):
            return None
        start = self.base + int(starts[idx])
        end = self.base + int(self.pdata[idx, 1])
        # the found function must lie within the same .text section as `va`
        if not (sec[0] <= start and end <= sec[1]):
            return None
        return (start, end)

    # --- RTTI walk: type name -> vtable(s) -------------------------------
    def vtables_for_rtti(self, type_name: str):
        """MSVC RTTI: TypeDescriptor name -> Complete-Object-Locator -> vtable.

        name field starts at the '.?AV...' string; TypeDescriptor base = name-0x10;
        a COL holds the TD's image-RVA at COL+0xC; the 8-byte absolute pointer to the
        COL sits at vtable-8.
        """
        hits = self.find_ascii(type_name)
        if not hits:
            return []
        name_va = hits[0]
        td_rva = (name_va - 0x10) - self.base
        cols = []
        for o in self.find_bytes(struct.pack("<I", td_rva)):
            va = self.off_to_va(o)
            if va is None:
                continue
            col_va = va - 0xC
            coff = self.va_to_off(col_va)
            if coff is None:
                continue
            if struct.unpack_from("<I", self.data, coff)[0] in (0, 1):  # COL signature
                cols.append(col_va)
        vtables = []
        for col_va in cols:
            for o in self.find_bytes(struct.pack("<Q", col_va)):
                va = self.off_to_va(o)
                if va is not None:
                    vtables.append(va + 8)
        return sorted(set(vtables))

    def vtable_slots(self, vt_va, n=12):
        off = self.va_to_off(vt_va)
        out = []
        for i in range(n):
            p = struct.unpack_from("<Q", self.data, off + i * 8)[0]
            fb = self.func_bounds(p)
            out.append((i, p, fb))
            if fb is None:
                break
        return out

    # --- annotated disassembly ------------------------------------------
    def disasm(self, start, end=None, maxins=400, known=None):
        from capstone import CS_ARCH_X86, CS_MODE_64, Cs
        md = Cs(CS_ARCH_X86, CS_MODE_64)
        md.detail = True
        known = known or {}
        fb = self.func_bounds(start)
        if fb and end is None:
            start, end = fb
        if end is None:
            end = start + 0x400
        off = self.va_to_off(start)
        if off is None:
            return f"// {hex(start)} is not in a mapped section"
        code = self.data[off:off + (end - start)]
        lines = []
        for i, ins in enumerate(md.disasm(code, start)):
            if i >= maxins:
                lines.append("  ...(truncated)")
                break
            ann = ""
            if ins.mnemonic in ("call", "jmp") and ins.operands and ins.operands[0].type != 1:
                try:
                    t = int(ins.op_str, 16)
                    ann = f"   -> {hex(t)}" + (f" ({known[t]})" if t in known else "")
                except ValueError:
                    pass
            if "rip" in ins.op_str and ins.operands:
                try:
                    ref = ins.address + ins.size + ins.disp
                    s = self.cstr(ref, 60)
                    tag = f" {known[ref]}" if ref in known else (f" {s!r}" if s.isprintable() and 2 < len(s) < 60 else "")
                    ann += f"   [rip->{hex(ref)}{tag}]"
                except Exception:
                    pass
            lines.append(f"  {hex(ins.address)}: {ins.mnemonic} {ins.op_str}{ann}")
        return "\n".join(lines)


def _main(argv):
    if not argv:
        print(__doc__)
        return 2
    pe = PE()
    cmd = argv[0]
    if cmd == "sections":
        for n, v, vs, r, rs in pe.sections:
            print(f"  {n:10} va={hex(pe.base + v)} vsize={hex(vs)} raw={hex(r)}")
        print(f"  .pdata functions: {len(pe.pdata)}")
    elif cmd in ("ascii", "utf16"):
        finder = pe.find_ascii if cmd == "ascii" else pe.find_utf16
        for va in finder(argv[1]):
            print(f"  {hex(va)}  {pe.cstr(va)!r}")
    elif cmd in ("xref", "calls"):
        target = int(argv[1], 16)
        sites = pe.find_riprefs(target) if cmd == "xref" else pe.find_calls_to(target)
        for s in sites:
            fb = pe.func_bounds(s)
            print(f"  {hex(s)}  in fn {hex(fb[0]) if fb else '??'}")
        print(f"  ({len(sites)} site(s))")
    elif cmd == "vtable":
        for vt in pe.vtables_for_rtti(argv[1]):
            print(f"  vtable {hex(vt)}")
            for i, p, fb in pe.vtable_slots(vt):
                print(f"    [{i}] {hex(p)}" + (f" fn[{hex(fb[0])}..{hex(fb[1])}]" if fb else " (non-func)"))
    elif cmd == "fn":
        print(pe.disasm(int(argv[1], 16)))
    else:
        print(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv[1:]))
