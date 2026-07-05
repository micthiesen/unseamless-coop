# State

The single **fast-moving** "what we're working on / what's next" file. It is **about the work** —
the current picture, the chosen next step, and the runners-up — **not** a snapshot of machine state.
**Overwritten, never appended** (git holds history). Owned by the orchestrator: `/wrap` rewrites it
when a session concludes, `/next` records next-step decisions in it, and `orch-start` seeds a fresh
orchestrator to read it and brief from it. Durable knowledge does **not** live here — findings go to
their proper doc (CLAUDE.md > "Project knowledge lives in the repo"); this file holds the current
picture, the chosen next step, and pointers.

> **Deliberately not tracked here:** live workers (use `scripts/fleet/worker-ls`), rig/Deck state (cheap
> to re-apply, never to remember or restore), and uncommitted git state (workers integrate before a wrap).

Last updated: **2026-07-05**.

## Now

- **Out-of-band connection stack (rungs 1/2/4 + the DLNW3D Steam P2P transport) is shipped and
  two-machine-proven.** Peers find each other by password-keyed Steam lobby, authenticate the
  side-channel, and exchange game-P2P packets by SteamID64 alone.
- **★★ HOST-SIDE rung-3 ESTABLISHMENT IS REPRODUCED — solo AND two-machine.** Our mod drives the
  game's own establish handler `0x1423f2820` to a stable `Host`/`Ingame` with the **full member graph**
  (1 SessionSteam + 6 SessionMemberSteam; `member[5]+0x80` = the host's own SteamID64; 5 empty slots) —
  byte-for-byte how real ERSC does it. The unlock was seeding the handler's input descriptor from the
  stood-up socketmgr's config defaults (in code). This solved the member machinery the whole
  gate-c/`+0x168` saga was stuck on. Details: [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★★ HOST-SIDE
  ESTABLISHMENT REPRODUCED".
- **The last rung-3 leg is the JOINER-MEMBER** (a connecting peer landing in an empty slot → roster > 1).
  The 2026-07-05 two-machine run proved: the Deck's SYN reaches host-admit `0x142640e30` and the
  side-channel links (`coop: linked`), but **no joiner member is added**, and the **transport admit path
  is the wrong door** — forcing gate-c accept doesn't help (a 2nd gate `0x142640ed5` bails, and gate-c
  rejects in real ERSC too). The joiner-member is added by the **session layer** (`add-member 0x1423fdf20`,
  same as the host's own), using the joiner's **identity handle** derived from its connection. See
  [SESSION-DRIVE.md](SESSION-DRIVE.md) > "★ JOINER-ADMIT".
- **Both prior "walls" are confirmed red herrings** (live): the `SteamServiceImpl` standup is satisfiable
  (owner=config), and the availability field `[[0x143d855c8]+0x10]` reads **0 in a working session** — the
  gate `0x140de2620` isn't on the establishment path.

## Next

**Chart how the host's establishment turns a connecting peer into a member — STATIC-first.** We do NOT
need a new ERSC capture to start: the task-#16 writer-trace already caught the joiner's member-add chain
(`update_step → 0x1423f2820 → session-create 0x1423f7070 → add-member 0x1423fdf20 → +0x80`), and the raw
dumps (`~/Documents/ersc-live-capture*.txt`) hold the member layout incl. the `+0x70`/`+0x78` handle
addresses.

1. **Static RE (offline, no ERSC):** chart where the joiner's **connection object + `+0x78` identity
   handle** come from (what feeds `add-member`'s `arg2`), and what makes the establishment add a member
   for a *connecting peer* vs the host itself. Use the binary + the capture dumps as the known-good ref —
   the same static-first path that cracked the host side.
2. **Reproduce it two-machine:** when the Deck connects (its SteamID is known from rung-4 / `coop: linked`),
   drive/allow the host to build the joiner's connection + handle and `add-member`. Verify a Deck SteamID64
   lands in an empty `member+0x80` slot (roster → 2). Harness + footgun-safe commands:
   [RUNG3-DRIVE-RUNBOOK.md](RUNG3-DRIVE-RUNBOOK.md) > "★ TWO-MACHINE HARNESS".
3. **Stabilize the Deck joiner** — it crashes ~30–60s into the join drive, limiting test windows.

- **Why this / why now:** host-side is done; the joiner-member is the last leg, now correctly scoped to the
  session layer (the transport-admit rabbit hole is ruled out). Static-first because we already hold most of
  the data; a targeted ERSC re-capture (watch `member+0x78` / the connection during a Deck join — a 10-min
  follow-up, Deck's still set up) is the **fallback** only if static stalls on a runtime-only piece.
- **Serial + Michael-gated:** the reproduce/verify step is two-machine (rig host + Deck joiner); the static
  charting is solo/delegable.

## Candidates Not Chosen

- **A fresh from-scratch ERSC live capture** — not needed first; we have the task-#16 trace + the dumps.
  Re-open only as the targeted fallback (see Next) if static charting can't source the joiner's identity
  handle / connection.
- **Forcing the transport admit** (`force_gatec_accept` / gate-c / the `0x142640e30` path) — **RULED OUT**
  this session: a 2nd gate bails and real ERSC rejects gate-c too. Lever kept in code but OFF. Don't
  re-chase; the joiner-member is session-layer.
- **Offline synthesis of the session graph** / the **`+0x168` "real member-lookup"** — long-dead ends.

## Learned Recently (Pointers Only)

- [SESSION-DRIVE.md](SESSION-DRIVE.md) — "★★ HOST-SIDE ESTABLISHMENT REPRODUCED" (the descriptor-seed fix +
  the reproduced graph) and "★ JOINER-ADMIT" (transport admit ruled out; the 2-gate disassembly; joiner-member
  is session-layer).
- [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) > "Writer-trace capture" — the member layout
  (`+0x80` = SteamID64), the add-member chain, the registry root, both red herrings.
- [STANDUP-NULL-FINDINGS.md](STANDUP-NULL-FINDINGS.md) — the standup factory is satisfiable; the offline null
  was a probe artifact.
- [RUNG3-DRIVE-RUNBOOK.md](RUNG3-DRIVE-RUNBOOK.md) > "★ TWO-MACHINE HARNESS" — footgun-safe two-machine
  procedure (pass `--auto-session` on `cycle`, not `apply`) + the Deck-crash gotcha.
