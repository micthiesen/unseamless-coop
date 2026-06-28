---
name: reverse-engineer
description: How to reverse-engineer Elden Ring / ERSC behavior for the rewrite — the behavioral-RE strategy, the static-triage and diagnostic patterns, and the tool decision table (rizin, capstone+numpy scans, the Ghidra/PyGhidra decompile wrapper, the native ptrace watchpoint, Frida). Use when figuring out an unknown game behavior or memory layout, deciding how to find a field the SDK doesn't name, or reaching for a disassembler/decompiler/instrumentation. TRIGGER on "reverse engineer", "how does ERSC do X", "find this flag/field", "use rizin/ghidra/frida/capstone", "diagnostic mode", "what offset is".
---

# Reverse-engineering for unseamless-coop

This is a **behavioral** reimplementation. We never copy upstream code; we observe *what* the
game/ERSC does and re-implement it from the public `fromsoftware-rs` SDK. Two hard constraints
frame all RE here:

1. **Clean-room (CLAUDE.md > Clean-room hygiene):** never paste decompiler/disassembler output
   into source, comments, or commits. Read to understand, write behavioral notes in your own
   words, implement from those.
2. **`ersc.dll` is Themida-packed:** ~5.5MB of ~7.4MB is a virtualized blob, only 8 stub imports
   are visible, so **static decompilation of ERSC is a dead end**. RE is observation-driven.

## Step 0 — check what's already charted (don't RE what the SDK gives you)

Before reversing anything, read [`docs/SDK-COVERAGE.md`](../../../docs/SDK-COVERAGE.md). The
`fromsoftware-rs` SDK already exposes most game state as **named typed fields** (networking,
params, task system, event flags, characters). If your behavior maps to a charted field, there's
nothing to reverse — just use it. Prefer named SDK fields over raw offsets always; only fall back
to byte reads for investigation (below). Pin `eldenring` + `fromsoftware-shared` to the **same**
commit; layouts are revision-specific.

## Pick the tool (decision table)

All headless/CLI, no GUI. Match the *goal*, not habit — most RE here is behavioral, so the bottom
rows do more work than the top ones.

| Goal | Tool | Notes |
|------|------|-------|
| "What is this binary?" (sections, imports/exports, strings, quick disasm) | **rizin** / `rz-bin` | JSON form (`-j`) pipes to `jq`. First stop for triage. |
| "Where in `eldenring.exe` is byte-pattern / AOB X?" | **capstone + numpy** (installed) | Throwaway Python over the raw PE; numpy scans, capstone disasms the hits (base `0x140000000`). |
| "Quick decompile while I'm already in rizin" | **rz-ghidra** `pdg` (installed) | `rizin -q -c 'aaa; s <addr>; pdg' bin`. Ghidra decompiler core, no JVM; rizin-fed analysis (lower fidelity). |
| "I need to *read* a hard function as good C" | **Ghidra/PyGhidra** (installed) | `scripts/re/ghidra-decompile.sh <bin> [func]`. Best fidelity; clean targets only (not `ersc.dll`). |
| "What instruction writes this live address?" | **`scripts/re/watch-write.py`** | Native ptrace HW watchpoint (exe at `0x140000000` under Wine). Root needed. No Frida. |
| "What flag/field flips when event X happens?" | **diagnostic DLL** | Our own mod, rising-edge bit observer (below). The default for unknown game state. |
| "Map an unknown call graph / hook live, iterating fast" | **Frida** (frida-gadget) | Host CLI + matching gadget staged (`.re-tools/frida/`); placing it in the rig is a rig action ([RUNTIME-RE.md](../../../docs/RUNTIME-RE.md) > B). |
| "What's on the wire?" (shape/timing) | **`ss` / `tcpdump` / `tshark`** | Payloads are Steam-framed/encrypted; pair with a hook for contents. |

Full bullets and install state: [`docs/DEVELOPMENT.md`](../../../docs/DEVELOPMENT.md) > "RE toolchain".

**Grow the committed RE scripts — don't re-inline.** `scripts/re/` (`decompile.py`,
`watch-write.py`, …) is shared, extensible tooling, and improving it is *in* your lane, not out of
it. If you find yourself pasting the same pyghidra boilerplate, ad-hoc capstone scan, or ptrace
tweak more than once, **add a flag or a helper to the script** instead — e.g. teach `decompile.py`
to list xrefs-to-a-string or dump a function's callers (pyghidra is full CPython, so it can do far
more than decompile). Two rules of thumb: keep each script runnable headless with no GUI, and keep
genuinely throwaway one-off scans in `/tmp` (only promote the reusable shape into `scripts/re/`).
Leave the next agent a sharper tool than you found.

## Static triage (metadata only, safe)

`ersc.dll` lives under `reference/` (gitignored). What static triage *can* tell us (factual
metadata, not logic) is already captured in
[`docs/DEVELOPMENT.md`](../../../docs/DEVELOPMENT.md) > "Reverse-engineering ERSC": the
Themida finding, the linked libraries (Steam P2P + Winsock + a TLS stack), and the
`modengine_ext_init` export. Re-run with **rizin** (installed) if needed:

```bash
rz-bin -l ersc.dll          # linked libraries (the networking/crypto architecture)
rz-bin -S ersc.dll          # sections (the .themida blob)
rz-bin -i ersc.dll          # imports (8 stubs)
rz-bin -E ersc.dll          # exports
# JSON form for scripting: rz-bin -ilSj ersc.dll | jq …
```

For locating something in a clean binary, two scriptable tools beyond rz-bin (full bullets in
[`docs/DEVELOPMENT.md`](../../../docs/DEVELOPMENT.md) > "RE toolchain"):

- **capstone + numpy** (installed) — throwaway Python scan scripts over the raw PE: numpy
  vectorizes AOB/byte scanning across the whole image, capstone disassembles the hits offline
  (base `0x140000000`). The workhorse for "where in `eldenring.exe` is X" — see
  `docs/SESSION-RE-FINDINGS.md` / `docs/OFFLINE-TITLE-SCREEN.md` for worked passes.
- **Ghidra headless via PyGhidra** (installed) — break-glass *readable C* when raw asm isn't
  enough: `scripts/re/ghidra-decompile.sh <bin> [function]` bootstraps a pyghidra venv and prints
  decompiled C, no GUI. Point it at CLEAN targets only — `ersc.dll` is Themida-virtualized
  (you'd decompile the unpacker stub), and `eldenring.exe` is mostly SDK-charted, so this is
  occasional, not the default.

## Finding unknown game state (the diagnostic pattern)

When a behavior **isn't** a named SDK field, don't hand-diff memory dumps. `er-crit-coop`'s
`src/diagnostic.rs` is the template:

- A diagnostic build mode (compile-time `MODE` switch: `Patch` vs `Diagnostic`, so it ships
  dormant, never as the default) runs a loop that snapshots candidate byte regions per `ChrIns`
  each tick.
- It logs **rising edges** (0→1) of individual bits, suppressing high-churn bits as noise.
- Trigger the behavior in-game (e.g. riposte a lone enemy) and the responsible
  region/offset/bit names itself in the log; then map it to a typed SDK field and use that.

This is how you locate a flag/field the SDK doesn't expose without ever reading upstream code.

## Dynamic RE on the rig

Live observation (the game running) is the real RE channel here, and it happens on the rig with
the game running. The full playbook — our own diagnostic DLL (preferred), Frida-gadget under Proton, and
network capture — is in [`docs/RUNTIME-RE.md`](../../../docs/RUNTIME-RE.md). The first concrete
target (observing the session FSM to unblock the co-op core) is the
[`/test-loop`](../test-loop/SKILL.md) skill's layer 4 + [`docs/RIG-RUNBOOK.md`](../../../docs/RIG-RUNBOOK.md).

## Recording findings

Write observations **in your own words** ("on event X the mod does Y", "field at `ChrIns+0x…`
rises during a riposte → maps to SDK `action_flag.…`"), then implement from the note. Feed
confirmed mechanics into `docs/FEATURES.md` / `docs/ARCHITECTURE.md` and, where it's pure logic,
into a host-tested `unseamless-core` type. Never transcribe upstream structure verbatim.
