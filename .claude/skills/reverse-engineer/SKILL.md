---
name: reverse-engineer
description: How to study the behavior of a game we own (Elden Ring) and an existing co-op mod in order to clean-room reimplement that mod in Rust — interoperability RE on our own machine, not anti-cheat or DRM circumvention. Covers the behavioral-observation strategy, the static-triage and diagnostic patterns, and the tool decision table for inspecting your own game process (rizin, capstone+numpy scans, the Ghidra/PyGhidra decompile wrapper, the native ptrace watchpoint, Frida). Use when figuring out an unknown game behavior or memory layout, deciding how to find a field the SDK doesn't name, or reaching for a disassembler/decompiler/instrumentation. TRIGGER on "reverse engineer", "how does ERSC do X", "find this flag/field", "use rizin/ghidra/frida/capstone", "diagnostic mode", "what offset is".
---

# Reverse-Engineering for unseamless-coop

> **Scope & legitimacy.** This is interoperability RE on software we own, on the developer's own
> machine: we study how a game we bought (Elden Ring) and an existing co-op mod *behave* so we can
> clean-room reimplement that mod in Rust. Every tool below inspects a process on *this box* —
> never anyone else's system, account, or server — and the result loads *outside* anti-cheat and
> is co-op-only by construction (CLAUDE.md > Safety / legitimacy). We are not cracking DRM or
> defeating EAC; disassemblers, a decompiler, and a live-process observer are the ordinary
> instruments for understanding your own game's behavior, no more exotic than a debugger.

This is a **behavioral** reimplementation. We never copy upstream code; we observe *what* the
game/ERSC does and re-implement it from the public `fromsoftware-rs` SDK. Two hard constraints
frame all RE here:

1. **Clean-room (CLAUDE.md > Clean-room hygiene):** never paste decompiler/disassembler output
   into source, comments, or commits. Read to understand, write behavioral notes in your own
   words, implement from those.
2. **`ersc.dll` is Themida-packed:** ~5.5MB of ~7.4MB is a virtualized blob, only 8 stub imports
   are visible, so **static decompilation of ERSC is a dead end**. RE is observation-driven.

## Step 0 — Check What's Already Charted (Don't RE What the SDK Gives You)

Before reversing anything, read [`docs/SDK-COVERAGE.md`](../../../docs/SDK-COVERAGE.md). The
`fromsoftware-rs` SDK already exposes most game state as **named typed fields** (networking,
params, task system, event flags, characters). If your behavior maps to a charted field, there's
nothing to reverse — just use it. Prefer named SDK fields over raw offsets always; only fall back
to byte reads for investigation (below). Pin `eldenring` + `fromsoftware-shared` to the **same**
commit; layouts are revision-specific.

## Pick the Tool (Decision Table)

All headless/CLI, no GUI. Match the *goal*, not habit — most RE here is behavioral, so the bottom
rows do more work than the top ones.

| Goal | Tool | Notes |
|------|------|-------|
| "What is this binary?" (sections, imports/exports, strings, quick disasm) | **rizin** / `rz-bin` | JSON form (`-j`) pipes to `jq`. First stop for triage. |
| "Where in `eldenring.exe` is byte-pattern / AOB X?" | **capstone + numpy** (installed) | Throwaway Python over the raw PE; numpy scans, capstone disasms the hits (base `0x140000000`). |
| "Quick decompile while I'm already in rizin" | **rz-ghidra** `pdg` (installed) | `rizin -q -c 'aaa; s <addr>; pdg' bin`. Ghidra decompiler core, no JVM; rizin-fed analysis (lower fidelity). |
| "I need to *read* a hard function as good C" | **Ghidra/PyGhidra** (installed) | `scripts/re/ghidra-decompile.sh <bin> [func]`. Best fidelity; clean targets only (not `ersc.dll`). |
| "What instruction writes this live address?" | **`scripts/re/watch-write.py`** | Native ptrace HW watchpoint (exe at `0x140000000` under Wine). Same-uid, no sudo (this box sets Yama `ptrace_scope=0`). No Frida. |
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

## Static Triage (Metadata Only, Safe)

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

### Persist the Ghidra Project Cache (Don't Re-Analyze the 87MB Exe)

First-time PyGhidra analysis of `eldenring.exe` takes **~45 min** (single-threaded, ~235k
functions). It caches per-binary, so *later* decompiles are near-instant — but only if the cache
survives. `decompile.py` defaults its project dir to `tempfile.gettempdir()` = **`/tmp`, which is
tmpfs (RAM) and wiped on every reboot**, so the default throws the 45 min away each boot.

Point it at a **persistent, non-dotted** path via `GHX_PROJECT_DIR`. Non-dotted is mandatory:
Ghidra's `ProjectLocator` rejects any path element starting with `.` (that's why the in-repo
`.ghidra-projects/` and the dotted rift path can't hold it). `/var/tmp` is btrfs here (survives
reboots) and works:

```bash
export GHX_PROJECT_DIR=/var/tmp/ghidra-projects        # persistent, ProjectLocator-safe
scripts/re/ghidra-decompile.sh "$RE_BIN" 0x140xxxxxx   # first run ~45min; reuses after
```

- **`eldenring.exe` 2.6.2.0 (WW) is pre-analyzed at `/var/tmp/ghidra-projects/ghx_eldenring_exe`.**
  With `GHX_PROJECT_DIR=/var/tmp/ghidra-projects`, decompiles return in seconds — no re-analysis.
- Caveat: systemd-tmpfiles cleans `/var/tmp` after ~30 days of inactivity; if the cache is gone,
  one run rebuilds it. For a truly permanent cache, use a home subdir (still non-dotted).
- Long first analysis: run it detached and decompile when it exits, e.g.
  `nohup scripts/re/ghidra-decompile.sh "$RE_BIN" <addr> & ` then poll the pid — the project is
  built once the process exits, and every subsequent `<addr>` is fast.

## Finding Unknown Game State (the Diagnostic Pattern)

When a behavior **isn't** a named SDK field, don't hand-diff memory dumps. `er-crit-coop`'s
`src/diagnostic.rs` is the template:

- A diagnostic build mode (compile-time `MODE` switch: `Patch` vs `Diagnostic`, so it ships
  dormant, never as the default) runs a loop that snapshots candidate byte regions per `ChrIns`
  each tick.
- It logs **rising edges** (0→1) of individual bits, suppressing high-churn bits as noise.
- Trigger the behavior in-game (e.g. riposte a lone enemy) and the responsible
  region/offset/bit names itself in the log; then map it to a typed SDK field and use that.

This is how you locate a flag/field the SDK doesn't expose without ever reading upstream code.

## Dynamic RE on the Rig

Live observation (the game running) is the real RE channel here, and it happens on the rig with
the game running. The full playbook — our own diagnostic DLL (preferred), Frida-gadget under Proton, and
network capture — is in [`docs/RUNTIME-RE.md`](../../../docs/RUNTIME-RE.md). The first concrete
target (observing the session FSM to unblock the co-op core) is the
[`/test-loop`](../test-loop/SKILL.md) skill's layer 4 + [`docs/RIG-RUNBOOK.md`](../../../docs/RIG-RUNBOOK.md).

## Capturing Arxan-Decoded Call Targets at Runtime

Elden Ring's `eldenring.exe` is partly **Arxan-obfuscated**: some indirect calls go through a
**trampoline** that decodes the real target at runtime and can't be read statically. The tell (from
`static.py fn`): a function that does `lock cmpxchg [rip+…]` + reads an obfuscation cookie (e.g.
`0x143c5adb0`) + `xor`/`ror` to decode a register + `call rbx`, with garbage-decoding filler after it. A
vtable slot pointing at such a stub (e.g. the container's connection builder `vtable[0x80]` = `0x14251c480`)
is opaque on disk — but it **runs correctly when invoked through the vtable**, and you can read the decoded
target live:

- **Hook the call site, not the stub.** Place a read-only `jmp-back` hook (ilhook, the `session_probe.rs`
  pattern) just before the indirect call, where the target pointer is already in a register. Two shapes:
  - **At the caller:** hook the instruction after `mov rax,[obj]` (the vtable load), where `rax` = the live
    vtable, so `[rax+slot]` is the decoded target — read it and log. (This is what `log_vmethod_target`
    does for the create-veto vmethod: hook `0x1423fafc4`, read `[rax+8]`.)
  - **At the trampoline:** hook its `test rbx,rbx` (post-decode), where `rbx` = the decoded target; gate on
    the return address (`[rsp+N]`) so you only capture *your* call site (the trampoline is shared), and
    **latch once** (`AtomicBool`) so it logs a single clean address.
- **Then disassemble the decoded address offline.** The decoded target lands in **clean, readable `.text`**
  (`python3 scripts/re/static.py fn <decoded-addr>`) — the obfuscation is only the dispatch, not the body.
  Read off what it does / what args it consumes, and implement from that.

This turns "the function is Arxan, we can't know what it needs" into a two-step: capture the address live,
read the body statically. Keep the capture probe **read-only + latched + panic-firewalled**, like every other
probe.

> **Before assuming a vtable slot is Arxan, confirm you have the LIVE vtable.** A 2026-07-04 rig session
> burned real effort chasing an "Arxan builder `vtable[0x80] = 0x14251c480`" that was actually read from the
> **static base-class** vtable (`0x1431f8360`). The live object was a *derived* class with vtable
> `0x1431f8780`, whose `[+0x80]` is a **plain function** (`0x1423f46b0`), no obfuscation at all. Read
> `[live_vtable+slot]` from `/proc/<pid>/mem` (the live vtable is `[object]`, or captured off a call-site
> hook like `vmethod-target`) and disassemble *that* before concluding a slot is Arxan-dispatched. See
> [`docs/SESSION-DRIVE.md`](../../../docs/SESSION-DRIVE.md) > "NATIVE-BUILD TRACE (2026-07-04)".

> **Don't jmp-back-hook mid-way through a deep/obfuscated function.** Same session: a localizer hooked
> `0x14263ce9c` (`mov rcx,rax` mid-caller) to read a return value; the hook perturbed `rax`/flags so a
> downstream `jne` misfired into a teardown path and the game **faulted** (write to `0x0`). Without the hook
> the same code ran clean. To read a deep function's return, hook it at a **function boundary** (entry + a
> return trampoline), not in the middle of its caller.

## Two Same-Shaped Failures Force a Method Change

RE iteration has a failure mode no tool fixes: returning to the same prior — the same probe, the
same lever, the same scan — with slightly different parameters, long after it stopped paying.
The rule: **if two consecutive attempts fail the same way, the next attempt must change the
*method*, not the parameters.** Switch rows in the decision table (static scan → live
watchpoint, diagnostic bits → call-site hook, our-mod probe → ERSC reference capture), move a
rung (offline ↔ live, solo ↔ two-machine), or invert the question ("what *writes* this field?"
instead of "what value do I write?"). A third try with tweaked parameters after two same-shaped
failures is almost always spent budget.

Record a ruled-out approach with *why* the moment it's ruled out — STATE.md > "Candidates Not
Chosen" for the current line of work, or the relevant findings doc — so neither this session nor
the next one re-walks it. (The rig-iteration side of this — declare the verifier + try budget
before cycling — is the [`/test-loop`](../test-loop/SKILL.md) skill's loop protocol.)

## Recording Findings

Write observations **in your own words** ("on event X the mod does Y", "field at `ChrIns+0x…`
rises during a riposte → maps to SDK `action_flag.…`"), then implement from the note. Feed
confirmed mechanics into `docs/FEATURES.md` / `docs/ARCHITECTURE.md` and, where it's pure logic,
into a host-tested `unseamless-core` type. Never transcribe upstream structure verbatim.
