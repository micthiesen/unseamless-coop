# Session create/join RE runbook (rung 3)

The exact recipe for the **one networking gap the SDK doesn't chart**: the internal functions that
start a co-op session. This is rig-gated RE — it can only be done with the game running, ideally with
two instances we control. It hands the rig operator (the orchestrator) a self-contained, batchable
probe and a precise "find these two functions" task.

Read alongside [COOP-CONNECTION.md](COOP-CONNECTION.md) (> "What the SDK gives us vs. the RE gap"),
[SDK-COVERAGE.md](SDK-COVERAGE.md) (networking/session row), the [`/reverse-engineer`] skill, and
[RUNTIME-RE.md](RUNTIME-RE.md) (the Frida-under-Proton + diagnostic-DLL tooling). The instrumentation
is `crates/unseamless-coop/src/session_probe.rs`.

## What we're looking for and why

The SDK charts the session **state** but not the **transitions**. `CSSessionManager.lobby_state` (a
`repr(u32)` at struct offset **`+0xc`** — the `usize` vftable is 8 bytes, then a `u32` `unk8`, so the
FSM pair lands at `+0xc`/`+0x10`, not `+0x8`) walks:

- **Host:** `None (0) -> TryToCreateSession (1) -> Host (3)`
- **Joiner:** `None (0) -> TryToJoinSession (4) -> Client (6)`

(`protocol_state` at `+0x10` then advances `None -> JoinCheck -> WaitInitData -> … -> Ingame (6)` once
a session is up.) The two functions we want are the ones that **initiate** those walks — the internal
"start hosting" and "start joining a peer" calls. Charting their entry points lets the co-op core
(rung 3 of COOP-CONNECTION.md) *drive* a session to `Host`/`Client` for a given peer SteamID instead
of only observing one. They are not summon-sign creation/acceptance per se; they are the lower
initiation that the sign flow (and any other entry into multiplayer) funnels into to flip
`lobby_state` off `None`.

## The instrumentation (already shipped, gated, inert by default)

`[debug.probes] session_probe = true` turns on two `session-probe:`-prefixed surfaces. Everything
uses that one prefix, so a batched rig run greps `session-probe:` and gets exactly this story:

1. **FSM rising-edge logger** (`SessionFsmProbe`, a FrameBegin task) — works **today, solo**. Logs:
   - once, on first sight: `session-probe: FSM live @frame N — CSSessionManager @0x… lobby=None protocol=None`
   - on every transition: `session-probe: FSM @frame N lobby None->TryToCreateSession protocol None->JoinCheck`
   - a `session-probe: live, no CSSessionManager yet (frame N)` heartbeat (~30s) before a session exists.

   Solo this just sits at `lobby=None`; the transition machinery is still exercised and correct.
   **Caveat:** this is a once-per-frame poll, so a `TryTo*` substate that lives less than one frame
   would be coalesced (you'd see `None->Host` with no `TryToCreateSession` line). The `TryTo*` states
   almost certainly persist across the multi-frame network handshake, so this should hold — but a
   *missing* `TryTo*` line means "too fast to catch at FrameBegin," not "the game skipped it." The
   entry hooks (below) are the authoritative capture; the FSM log is the correlation timeline.

2. **Create/join entry hooks** (`install_hooks`) — **inert until the AOBs below are charted**. With
   the probe on but the addresses unset it logs, once each at boot:
   `session-probe: create-session hook AOB not yet charted (rig RE pending); hook inert — see docs/SESSION-RE-RUNBOOK.md`
   Once charted (below), each hook logs on a real connect, **at `debug!` level** (so enable `[debug]
   verbosity` for the RE run — the line carries a raw peer SteamID64, kept out of the default `info`
   shareable log):
   `session-probe: create-session initiated | rcx=0x… rdx=0x… r8=0x… r9=0x… (rdx/r8/r9 may carry a raw peer SteamID64 — do not share this log verbatim)`
   (win64 ABI: `rcx` = `this`, then `rdx`/`r8`/`r9` — the candidate `CSSessionManager` pointer and the
   peer SteamID argument; read-only, the registers are dumped, never dereferenced. **Privacy:** that
   SteamID64 resolves straight to a Steam profile, so scrub or `peer_tag`-pseudonymize it before
   sharing the log, and once you know which register holds it, route it through `diagnostics::peer_tag`
   like the observer does.)

The hooks log the **call**; the FSM logger logs the **resulting transition** a frame or two later.
Correlate them by frame/timestamp — together they are the before/after of an initiation.

## Rig recipe

### 0. Arrange the run

- Set `[debug.probes] session_probe = true` in the seed config (`scripts/rig/seed-config.toml`), then
  `scripts/rig.sh apply` + launch. (`[debug] enabled = true` is already on in the seed, so the
  `session-probe:` lines — which are `info` — are captured.)
- Two instances we control + the rung-2 side-channel to coordinate ("both go now"). One machine can
  host while a second (or a second account) joins; the side-channel is just for human coordination
  here.
- `scripts/rig.sh log -f | grep session-probe:` to watch the story live.

### 1. Confirm the FSM logger (no addresses needed)

On a real connect you should see the host walk `None -> TryToCreateSession -> Host` and the joiner
`None -> TryToJoinSession -> Client`, each line frame-stamped, and the `CSSessionManager @0x…` base
printed once per instance. This alone is the first real capture of the transition — it confirms the
enum values, the ordering, and the frame timing, and it gives you the live base address to match a
hooked call's `rcx` against. **If this is all you get this session, that's still a useful result** (it
de-risks and times the next step).

### 2. Find the two initiation functions

The goal is the entry point of the function that performs the `lobby_state` store to
`TryToCreateSession` (1) / `TryToJoinSession` (4).

> **Read [SESSION-RE-FINDINGS.md](SESSION-RE-FINDINGS.md) first.** A static pass (2026-06-27) already
> charted the live `CSSessionManager` instance global (`G = 0x143d7a4d0`, so `[G]` is the manager;
> equivalently the `base` the FSM probe prints), the constructor (`0x140cabb60`), and the field
> offsets — and **proved strategy B below does not work on this build** (the transition is a register
> store, not an immediate; the immediate `+0xc` stores all land on unrelated reflection/`"FACE"`
> objects). So go straight to the write-watch; the findings doc hands you `base + 0xc` directly.

Strategies, in the order that now actually pays off:

- **A — write-watch (the route).** With Frida-gadget attached (RUNTIME-RE.md, Option B), set a 4-byte
  memory-write watch on `&CSSessionManager.lobby_state` = `base + 0xc` (the `base` is in the
  `session-probe: FSM live …` line, and equals `[0x143d7a4d0]`). Trigger a host/join; the watch
  reports the instruction that writes `1`/`4` on the first `None →` edge (use the probe's transition
  line to ignore later copies in the session-assignment family). Walk up to the enclosing function
  prologue → that entry is the hook site. Solo reaches the **host/create** edge only; **join** needs
  a peer (folds into the two-player friend test).

- **B — store-site AOB (does NOT work on the current build; kept as a record).** Statically scanning
  for `mov dword ptr [reg+0xc], 1` (`C7 4? 0C 01 00 00 00`, or the `C7 8? …` disp32 form) was the
  original plan, but on the 2026-06-02 exe every such immediate store is on an unrelated object and
  the real `lobby_state` writes are register stores in `this`-param callees — see
  [SESSION-RE-FINDINGS.md](SESSION-RE-FINDINGS.md) > "Why static stops here." Don't burn time re-running
  it unless a future patch changes how the field is written.

- **C — ERSC accelerator (optional).** If blind RE stalls, restore the real ERSC stack
  (`scripts/rig.sh restore`) and Frida-watch the same `base + 0xc` write while ERSC connects, to see
  which function ERSC lets the game run for the initiation. Observe behavior only — never copy ERSC
  bytes (CLAUDE.md > Clean-room).

The win64 prologue at the entry is typically `48 8B C4` (`mov rax, rsp`), `40 53` (`push rbx`), or
`48 83 EC ..` (`sub rsp, ..`); use ~16 unique bytes from there as the landmark and note the opcode
byte for the `expect` guard.

### 3. Fill the scaffold and rebuild

In `crates/unseamless-coop/src/session_probe.rs`, replace the two `None` consts (look for the
`RIG TODO` block):

```rust
const SESSION_CREATE_SITE: Option<HookSite> = Some(HookSite {
    landmark: pelite::pattern!("48 8B C4 ?? ?? .."),   // ~16 unique entry bytes; one `?` per wildcard
    offset: 0,                                          // landmark match-start IS the entry
    expect: 0x48,                                       // first prologue byte, anti-drift guard
});
const SESSION_JOIN_SITE: Option<HookSite> = Some(HookSite { /* the join entry */ });
```

(If the unique landmark sits a few bytes *before* the prologue, point it there and set `offset` to the
signed distance to the entry — same convention as the skip-intros patch in `coop/app.rs`.) Rebuild
(`cargo build --release`), redeploy, relaunch.

### 4. Verify

Watch for, in order: `session-probe: hooked create-session initiation at 0x…` at boot, then on a real
host: `session-probe: create-session initiated | rcx=0x… …` immediately followed (within a frame or
two) by `session-probe: FSM … lobby None->TryToCreateSession`. Confirm `rcx` matches the
`CSSessionManager @0x…` base, and look for the partner's SteamID64 among `rdx`/`r8`/`r9` (that pins
which argument is the peer). The join hook mirrors this for `None->TryToJoinSession`.

### Outcomes and what they mean

- **`hooked …` then `initiated …` then the matching FSM transition** → success: the entry is correct
  and the argument registers name the `this` + peer. Record the landmark + the register→meaning
  mapping inline next to the const (per CLAUDE.md > "Document how to re-derive RE results"), and hand
  the confirmed entries to the co-op core to *drive*.
- **`hooked …` but no `initiated …` on a real connect** → the landmark resolved but to the wrong
  function; the store-site you walked back from wasn't the initiation entry. Re-do step 2 (prefer
  strategy A's write-watch, which points at the exact instruction).
- **`site byte 0x.. != expected …` / `landmark not found or not unique`** (from `resolve_landmark`) →
  the AOB is too loose, too tight, or drifted; tighten/retake it. Fails safe — no hook is placed.
- **FSM transitions log but the hook never fires** → the initiation doesn't pass through the function
  you hooked (another path flips `lobby_state`); write-watch again to find the real writer.

## Clean-room note

This is behavioral RE: we watch *what* writes the state and *which* function the game runs, then
implement our own driver from that. Do not paste decompiler/disassembler output into source, comments,
or commits — record findings in your own words (CLAUDE.md > Clean-room hygiene).

[`/reverse-engineer`]: ../.claude/skills/reverse-engineer/SKILL.md
