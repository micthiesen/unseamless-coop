# Two-Player Friend-Test Runbook

> **Status — first run done (2026-06-27):** rungs **2 + 4 CONFIRMED** (`coop: linked … versions match`;
> `coop_connect` showed lobby created, host-id resolved, handshake reached, sent 2674 / received 2011).
> **Rung 3 not captured** — the in-game multiplayer items are **greyed out offline** (outside EAC), so the
> game's own session FSM couldn't be triggered the normal way; re-enabling those items (an RE/patch) is the
> likely unlock. Also surfaced: the **overlay crashes on native Windows** (hudhook DX12) — the friend ran
> `[debug] overlay = false` + `[debug] auto_session = "join"` (headless connect) to get around it; fixing
> the overlay is a pre-release blocker (under investigation).

The recipe for the **one** thing the rig can't do alone: a real second player. It is built to move
three rungs in a single friend session —

- **Rung 4** — password-keyed lobby discovery (the joiner-finds-host leg; `CreateLobby` is already
  rig-proven solo).
- **Rung 2** — the private Steam P2P side-channel links over the discovered peer (`coop_connect`
  walks `linking → linked`).
- **Rung 3** — the **create-drive test**: with a real peer present, drive the charted create and see
  whether it walks past leg B's deep registry lookup to `Host`/`Client` (see "Part B" below; the
  initiation functions are already charted, so this is no longer a capture task).

One good friend session pays for all three. Stage everything below **up front** so a single launch
captures the lot; the friend is a low-effort peer (set a password, press Play, host once, join once,
send one file).

> **Player 2 can be a Steam Deck you drive over SSH** (a throwaway account), not just a human friend — see
> the [`/steam-deck`](../.claude/skills/steam-deck/SKILL.md) skill (`scripts/deck.sh`). It applies this
> same seed config (same password) + save and reaches gameplay on the Deck, so the assistant handles the
> player-2 *mechanics* and only a human-in-the-world is needed for actual co-op.

Read alongside [COOP-CONNECTION.md](COOP-CONNECTION.md) (the rung model + the Steam decisions) and
[SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) (the rung-3 create/join RE in full). The connection
itself is driven by lobby discovery — there are **no SteamIDs to copy**; both players share the same
password, then one picks **Open World** and the other picks **Join world** from the overlay menu.

## What's already shipped (lean on it)

The prep batch we deferred for this test has landed — don't rebuild it, just turn it on:

- **Per-stage connect report** (`coop_connect` section of the diag dump). Replaces the coarse
  off/linking/linked/lost phase atomic with a timestamped, per-stage `ConnectReport`, so a *failed*
  link is diagnosable from one log without a re-run: own-SteamID resolved? session accepted? messages
  sent vs received (`sent>0, recv=0` = one-way NAT)? handshake landed? version match? and — for rung
  4 — lobby created / found (`candidates`) / joined / host-ID resolved. Identities are `peer_tag`
  pseudonyms, no raw SteamIDs, so the block is safe to share.
- **One-click "Export diagnostics"** (overlay Actions tab). Writes a single shareable file —
  `<ELDEN RING\Game>\unseamless-coop\unseamless-coop-diagnostics.txt` — with the run header + a live
  snapshot + a recent log tail. It reads only local, present-thread-safe sources (never the co-op
  transport), so it captures exactly the **never-connected** case that log-forwarding can't (forward
  needs the link up). Password redacted, every SteamID64 scrubbed to a `peer_tag` — safe to post
  publicly. [README-FRIENDS.txt](../scripts/dist/README-FRIENDS.txt) already leads the friend to this
  button.

So the friend's whole reporting duty is: press the Export button, send back one file.

## Stage it up front (one seed config, both players)

This run rides the **lobby-discovery build** (the one where rung 4's discovery path is live). In
`scripts/rig/seed-config.toml` (and in the bundle that ships to the friend), set:

- `[session] password`: the **same** value on both players, and now **at least 8 characters**
  (`MIN_PASSWORD_LEN`; the startup guard refuses to launch a shorter one). This is the only pairing
  input: it keys the lobby (`SetLobbyData`/filter via the verbatim SHA-256 `lobby_discovery_token`),
  it **authenticates the peer** (the side-channel handshake now proves password knowledge before
  linking, see below), **and** it derives the session AES key later. (The lobby key/filter is
  `lobby_discovery_token`, a domain-separated SHA-256 over the verbatim password, truncated to 32 hex
  chars, KAT-pinned so the DLL and the harness agree.) Nothing else identifies the peer.

> **Both players MUST set the identical password.** The side-channel now authenticates the peer: each
> side presents a password-keyed proof (`Auth`) and is **not linked** until the other side verifies it.
> A mismatch shows a plain-voice **"Authentication failed … (wrong co-op password)"** banner and the
> peers never link: no config sync, no session actions, no log forwarding. A wrong password is no
> longer a silent non-connect; it names itself.
- `[debug] enabled = true`, `level = "debug"` — so the `session-probe:` / `coop` lines (and the
  rung-3 register dumps, which are `debug!`) are captured.
- `[debug] forward_to_host = true` — once linked, the client tees its log to the host's `LogBundle`
  (client→host only; the manual export is the fallback until the link is up).
- `[debug.probes] session_probe = true` — turns on the rung-3 FSM rising-edge logger + the create/join
  entry hooks (`session-probe:` prefix). See [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md).
- **For Part B (the rung-3 create-drive test), also set on BOTH machines:** `[debug.probes] drive_create
  = true`, `[debug.probes] force_netsession_ready = true`, `[gameplay] bypass_session_create_gate = true`,
  `[gameplay] enable_offline_multiplayer = true`. These drive the charted create directly past the Arxan
  gate + reject #1, so the only open question is whether a **real peer** satisfies leg B's deeper session
  registry lookup (the solo drive's failure point). See [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Leg B charted".
- The **friend-capture export** is always available (the overlay button); no flag needed.
- `[debug] guide = "<name>"` — **when this session is to validate something specific, always ship a
  guide.** An in-overlay guide pins the test steps on-screen so the friend(s) **and** you follow the
  exact same sequence (advance with `L3 + D-pad Up`, skip with `L3 + D-pad Down`), instead of a chat
  round-trip. Add it **by hand** to each machine's `unseamless-coop/unseamless_coop.toml` after
  extracting (the `rig.sh package` step doesn't emit these debug keys, and ConfigSync doesn't carry
  them). For `two-player-join` each machine's **role is derived** from its Open/Join action by the
  guide's connect step, so **leave `[debug] rig_role` at the default `solo`** — you no longer set it per
  machine. (It remains an override/solo fallback: set it `host` / `join` only to force a role for a
  guide without a connect step, or to run one leg solo.) For this connect test, `two-player-join` fits;
  for the create RE, `rung3-create-chart`. Their steps **auto-finish off the run log** (`coop: linked`,
  `coop: adopted host config`, the `session-probe:` FSM lines), so the result lands in the
  shareable/forwarded log and the tester is never asked "did it connect?" — that's the whole point of
  staging a guide over a chat round-trip. The whole subsystem is debug-only, so **every machine must
  run a diag build** (release strips it entirely). Authoring + the committed list:
  [RIG-GUIDES.md](RIG-GUIDES.md) + the `rig-guides` skill.

> The `lobby_callback_probe` (rung-4 gate) has already served its purpose solo — it confirmed ER pumps
> via `RunCallbacks` and `CreateLobby` succeeds. It can stay off for the friend test; the live
> discovery driver is what we're exercising now, not the probe.

## Part A — Connect via lobby discovery (rungs 4 + 2)

The connection model: **both share the same password; one opens a world, the other joins.** Co-op is
triggered on demand from the overlay menu, not at launch — the actions stay disabled until Steam
networking is ready and the player is in-game. The host (Open World) creates the password-keyed lobby;
the joiner (Join world) filters the list by the password, finds it, and joins. The role is the user's
**choice**, never derived (only the host creates a lobby). The connection is **repeatable**: a failed
attempt can be retried (Leave, then Open/Join again), and each Open/Join **resets the `coop_connect`
report** so the per-stage diagnostics reflect only the latest attempt, not a stale earlier one.

**The in-game procedure is the `two-player-join` guide — don't hand-relay it.** Stage `[debug] guide =
"two-player-join"` (no per-machine `rig_role` — the connect step derives each machine's role from its
Open/Join action), and both machines are walked through the connect on-screen, each step
**auto-finishing off the run log** so the result is captured, not relayed: the connect step resolves on
the Open/Join action itself, then the rung-2 link milestone (`coop: linked`) and the client's config
adoption (`coop: adopted host config`). The host (Open World) sees the host steps, the joiner (Join
world) the joiner steps, both the shared ones — derived per machine. Just watch the rig side with
`scripts/rig.sh log -f` and have the friend hit Export at the end; the guide drives the rest. The
ordered steps live only in the guide (`crates/unseamless-core/src/guide/guides.rs`), never duplicated
here.

**What the logged stages mean (the guide auto-detects these; this table is the diagnostic reference for
reading the `coop_connect` section of a diag dump, or the live log):**

| Stage | Success looks like | A failure here means |
|---|---|---|
| Own SteamID (rung 1) | `self_id_at` set | rung-1 resolution broke; nothing downstream can run |
| Lobby create (host) | `lobby.role = host`, `created_at` set | `CreateLobby` poll failed (rig-proven solo, so suspect the build) |
| Lobby find (joiner) | `candidates = Some(N≥1)`, `list_returned_at` set | `Some(0)` = empty filter (password mismatch / host not up / version-tag mismatch); `None` = list never returned |
| Lobby join (joiner) | `joined_at` set, `host_id_resolved = true` | joined but couldn't read the owner SteamID (the value that seeds rung 2) |
| Session accept (rung 2) | `session_accepted_at` set | we never `AcceptSessionWithUser`'d the resolved peer |
| Messages | `messages_sent > 0` **and** `messages_received > 0` | `sent>0, recv=0` = one-way NAT (the classic) |
| Handshake | handshake stamped, `coop_connect` → `linked` | partner's `Hello` never landed |
| Auth | peer linked (proof verified) | an **"Authentication failed with <peer> (wrong co-op password)"** banner = the two passwords differ; fix them to match |
| Version | match | version mismatch (confirm both ran the same `build_id`) |

A clean link shows `coop_connect` walking `linking → linked`, an overlay **"Co-op partner connected"**
toast, the client **adopting the host's config**, and (with `forward_to_host`) the host's `LogBundle`
picking up the client's lines.

**Watch the one open Steam question:** messaging two arbitrary SteamIDs over `ISteamNetworkingMessages`
may require the accounts to be **Steam friends** for NAT-punch/auth. If `sent>0, recv=0` persists,
have the two accounts friend each other and retry — that's the most likely fix, and confirming it is
itself a result worth recording.

**Regardless of outcome, have the friend hit Export and send the one file** — the connect report makes
even a *failed* attempt fully diagnosable without a second session.

## Part B — Rung-3 create-drive test (does a real peer unblock create?)

> **This supersedes the old "capture the create/join initiation functions" leg — that's done.** The
> create wrapper is charted (`0x140cad4c0`), the Arxan availability gate is bypassed, and leg B (the
> network-create vmethod `0x1423f5c00`) is charted down to a deep session **registry/init lookup**
> (`0x1423fa1b0`) that yields nothing in a **solo** drive. The one open question this leg answers: **does
> a real connected peer satisfy that lookup so create reaches `Host`/`Client`?** Full trace:
> [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Leg B charted".

With Part A linked (rung-4 lobby + rung-2 side-channel up) and the Part B probes staged on **both**
machines (`drive_create`, `force_netsession_ready`, `bypass_session_create_gate`,
`enable_offline_multiplayer`), the `session-create-driver` fires once per machine the moment it's in-game
with `lobby_state == None` — i.e. **with the peer present** this time, not solo. No item, no menu action
needed for the drive itself; the lobby just needs to be up (Open World on one, Join world on the other)
so a real peer/match context exists when create runs.

**Read these `session-probe:` lines on each machine's log (`scripts/rig.sh log -f` on the rig side; the
friend hits Export):**

| Line | Solo result (known) | What a PASS looks like with a peer |
|---|---|---|
| `NetworkSession+0x10 (reject#1 flag) = N before create` | `0` | `0` (forced to `1` next line either way) |
| `forced NetworkSession+0x10 = 1` | present | present |
| `drive-create returned <bool> — lobby_state now <state>` | `false` → `FailedToCreateSession` | **`true` → `TryToCreateSession`/`Host`** |
| `FSM … lobby None->…` | `None->FailedToCreateSession` | **`None->TryToCreateSession->Host`** (joiner: `->TryToJoinSession->Client`) |

- **PASS** (`lobby_state` reaches `Host`/`Client`, `protocol_state` advances toward `Ingame`) ⇒ the
  registry lookup needed a real peer; rung-3 create is unblocked. Next: wire the driver into the real
  Open/Join actions (replace the probe), establish the password-derived AES key, and source the menu's
  session-state bits from the FSM.
- **STILL `FailedToCreateSession`** ⇒ a real peer isn't sufficient either. Capture both machines' logs and
  hand back; the fallback is to keep tracing leg B's registry chain (`0x1423fa1b0 →
  [new_session_vtable+0xd8] → 0x1423fa100`) to its root, or ERSC-style session neutralization.

> **Privacy:** `session-probe:` register/SteamID dumps can carry a raw peer SteamID64 (resolves to a Steam
> profile). Keep those `debug!` lines out of any shared log — the `coop_connect` report + Export bundle are
> scrubbed; the raw `session-probe:` lines are not. ([SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) warns of this.)

This stays behavioral RE — read *what the game does*, implement our own driver from that, never paste
decompiler output into source/commits ([CLAUDE.md](../CLAUDE.md) > Clean-room hygiene).

## Part C — Native overlay-crash trace (solo friend, any NVIDIA box)

Independent of Parts A/B (no co-op session needed): the overlay crashes at the first hooked `Present`
on native NVIDIA but works on our vkd3d rig and on WARP in the VM, so the crash is NVIDIA-driver-specific
and only a real NVIDIA Windows machine can produce the decisive datum. The `crashdump` handler
(`coop/crashdump.rs`, in every build) turns that crash into a logged **faulting module+offset**. This
is a *light* ask — one friend, no co-op, often crashes at launch.

**Build + share the bundle** (diag = symbols + reliable crash tail; `--trace` adds the hudhook
breadcrumbs alongside the crashdump line):

```
scripts/rig.sh package --trace      # diag friend bundle with [debug] level = "trace", overlay on
scripts/rig.sh share                # upload to the rolling GitHub prerelease; copies the link
```

Do **not** bake a `--guide` for this one — the guide renders *through* the overlay, which is the thing
crashing, so it can't display. Hand the friend the release link; they Install + just launch the game
(solo). It either crashes (the case we want) or runs.

**What to read in the returned logs** (they zip `unseamless-coop\logs\`; the Export button is
unreachable if the overlay crashed — README-FRIENDS tells them this):

- The decisive line: `crashdump: ==== UNHANDLED EXCEPTION ==== code=0xc0000005 (ACCESS_VIOLATION) at <module>+0x…`.
  `nvwgf2umx.dll`/`nvd3dumx.dll` ⇒ inside the NVIDIA driver (hyp #1 trigger); a Streamline/overlay
  interposer DLL ⇒ hyp #2; `hudhook`/`unseamless_coop.dll` ⇒ the detour glue.
- The breadcrumbs before it (`overlay: DX12 present-hook installed` → `initialize() reached` →
  `Call IDXGISwapChain::Present trampoline`) localize *where* in the flow it died; diff against the rig
  baseline in [OVERLAY-RENDERING.md](OVERLAY-RENDERING.md).
- Symbolicate our own frames: `x86_64-w64-mingw32-addr2line -f -C -e <diag dll/exe> $((ImageBase + offset))`
  (DLL ImageBase via `objdump -p`). Full recipe + the WARP self-test in the [`/windows-test`] skill.

[`/windows-test`]: ../.claude/skills/windows-test/SKILL.md

## After the session

- **Collect:** the friend's `unseamless-coop-diagnostics.txt` (Export button) + our own rig log.
- **Rung 4/2 verdict:** did `coop_connect` reach `linked`? If not, the per-stage report names the
  failing stage (use the table above). Record whether peers had to be Steam friends.
- **Rung 3 verdict (the create-drive test):** did `drive-create` return `true` with the peer present —
  `lobby_state` reaching `TryToCreateSession`/`Host` (joiner: `Client`) instead of the solo
  `FailedToCreateSession`? If **PASS**, rung-3 create is unblocked by a real peer: next is wiring the
  driver into the real Open/Join actions (drop the probe), the password-derived AES key, and sourcing the
  menu's `SessionContext` bits from the FSM. If **STILL fails**, the peer isn't sufficient — keep tracing
  leg B's registry chain (`0x1423fa1b0` →…) per [SESSION-DRIVE.md](SESSION-DRIVE.md) > "Leg B charted", or
  fall back to ERSC-style session neutralization. Document any new finding inline per
  [CLAUDE.md](../CLAUDE.md) > "Document how to re-derive RE results".

## Cross-references

- [COOP-CONNECTION.md](COOP-CONNECTION.md) — the rung model, the poll-not-pump Steam decisions, the
  lobby-discovery connection model.
- [SESSION-RE-RUNBOOK.md](SESSION-RE-RUNBOOK.md) — the rung-3 create/join RE in full (the probe, the
  AOB strategies, the scaffold to fill, the outcomes).
- [RUNTIME-RE.md](RUNTIME-RE.md) — Frida-under-Proton + the diagnostic-DLL tooling for the write-watch.
- [README-FRIENDS.txt](../scripts/dist/README-FRIENDS.txt) — what the friend sees (install + the
  Export button + the zip-your-logs fallback).
- [RIG-RUNBOOK.md](RIG-RUNBOOK.md) — the broader rig procedures + the two-player live verifications
  this session can also knock out (scaling in-combat, player count, session persistence).
