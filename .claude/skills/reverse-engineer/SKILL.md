---
name: reverse-engineer
description: How to reverse-engineer Elden Ring / ERSC behavior for the rewrite — the behavioral-RE strategy, the static-triage and diagnostic patterns, and the rig tools (rizin, the Ghidra wrapper, Frida). Use when figuring out an unknown game behavior or memory layout, deciding how to find a field the SDK doesn't name, or reaching for a disassembler/decompiler/instrumentation. TRIGGER on "reverse engineer", "how does ERSC do X", "find this flag/field", "use rizin/ghidra/frida", "diagnostic mode", "what offset is".
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

A C decompiler is **optional/on-demand** (`ersc.dll` is undecompilable; `eldenring.exe` is
already SDK-charted). If a clean target ever needs one, `scripts/re/ghidra-decompile.sh <bin>
[function]` wraps Ghidra's headless analyzer (no GUI). See DEVELOPMENT.md > "RE toolchain".

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

Live observation (the game running) is the real RE channel here, and it happens on the rig, not
the Mac. The full playbook — our own diagnostic DLL (preferred), Frida-gadget under Proton, and
network capture — is in [`docs/RUNTIME-RE.md`](../../../docs/RUNTIME-RE.md). The first concrete
target (observing the session FSM to unblock the co-op core) is the
[`/test-loop`](../test-loop/SKILL.md) skill's layer 4 + [`docs/RIG-RUNBOOK.md`](../../../docs/RIG-RUNBOOK.md).

## Recording findings

Write observations **in your own words** ("on event X the mod does Y", "field at `ChrIns+0x…`
rises during a riposte → maps to SDK `action_flag.…`"), then implement from the note. Feed
confirmed mechanics into `docs/FEATURES.md` / `docs/ARCHITECTURE.md` and, where it's pure logic,
into a host-tested `unseamless-core` type. Never transcribe upstream structure verbatim.
