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

Last updated: **2026-07-06**. Two-player co-op is **one message away**: everything up to a persistent,
endpoint-wired joiner member works; the last gap is delivering a valid **type-5** (Steam auth-ticket) message
to complete the handshake (`member+0x152=1` → `players=2`). This session course-corrected off a red-herring
trail, disproved two producer approaches, and pinned the path: **hand-frame the type-5 ourselves and
`SendP2PPacket` it on ch30** (the game's own send is Arxan-locked). The next session builds that sender.

## Now

- **Out-of-band connection stack (rungs 1/2/4 + the DLNW3D Steam P2P transport) is shipped and
  two-machine-proven.** Peers find each other by password-keyed Steam lobby, authenticate the side-channel,
  and exchange game-P2P packets by SteamID64 alone.
- **The session graph is reproduced up to the handshake.** `drive_add_peer` queues the peer member; the
  game's own per-frame pump (`0x1424007e0 → 0x142401110 → 0x14203ef70`) **builds the peer's endpoint**
  `member+0x130` (a real `MTInternalThreadSteamConnection`, vtable `0x143277750`) — on both machines, stable,
  no crash. Run 12 confirmed it builds even with our fabricated SYN suppressed (the SYN was pure noise).
- **★★ THE ONE REMAINING GAP: the DLNW3D handshake never completes.** The peer member's flags reach
  `(0,1,0,0)` (`+0x151`, set by the endpoint build) but never `(0,0,1,0)` (`+0x152`, working-ERSC). `+0x152`
  is set only by a **type-5** message = a real Steam **auth ticket** the host validates
  (`0x142402ee0`/`BeginAuthSession` vs `member+0x80`). Our driven flow never produces it, so the member
  times out (~30s), drops, re-adds — `players` stuck at 1. Producing a valid type-5 is the whole ballgame.
- **The type-5 is fully charted** (ERSC capture #2). Pump `0x1424007e0`, `buf[0]`=type, jump table
  `0x1424009f8`; **type 5** case `0x142400924` → validator `0x142402ee0` → `conn+0x148`=token,
  **`conn+0x152`=1**. Payload = `{[5], 8B token, 4B len (1..0x400), len·blob}`; validator gates ONLY on the
  ticket (`BeginAuthSession`, result ∈ {0,2}) — **the 8B token is stored unvalidated (any bytes)**. The full
  send/validator/injection chart is SESSION-DRIVE.md > "★ TYPE-5 PRODUCER + INJECTION".
- **Two producer approaches DISPROVEN this session (don't retry):** (a) *let the game's endpoints talk* — run
  12: suppressing our SYN didn't help, `+0x152` still never sets. (b) *drive the game's own send
  `0x142400df0` cold* — runs 13–15: **crashes**, because that send dispatches through an **Arxan-obfuscated
  endpoint vtable slot** (`[vtable+0x70]=0x119930522`, below the image base, identical across ASLR launches),
  only valid in the game's protected control flow. The send-drive is OBSERVE-ONLY on `main` (`ec691cc`) —
  **do not re-enable its `send()` call.**
- **⚠️ RED HERRING (runs 4–11) — DO NOT RE-LITIGATE:** the transport gate-c / find-or-create `0x142640e30` /
  `[S+0x168]` member-resolve is **not** the admission mechanism — ERSC-LIVE-CAPTURE correction #1 proved
  `[context+0x168]` is the stub `0x1423fdf00` **even in a fully working ERSC session**. The connection is
  built by the session-layer pump, not find-or-create. (Banner atop SESSION-DRIVE.md > "★ JOINER INBOUND AIM
  SHEET".)

## Next

**★ Build the HAND-FRAME type-5 sender: construct the message bytes ourselves and `SendP2PPacket` them on
ch30.** This is the only path that avoids the Arxan-locked send `0x142400df0` — we never call the game's
obfuscated dispatch, we just push bytes through the plain `ISteamNetworking006` send we already drive in
`drive_p2p`. The peer's pump reads ch30 → routes by sender id → the endpoint's queue → the type-5 case →
validator → `member+0x152=1` → roster → **`players=2`**. Serial (orchestrator drives the rig + Deck).

**The scaffold is already on `main`** (host-tested): `core::dlnw3d::frame_type5(token, blob)` frames
`[5][8B token][4B len][blob]` (`be7f64e`); `steam::get_auth_session_ticket()` binds the flat
`GetAuthSessionTicket` and returns the ticket blob (`be7f64e`). **The build is: wrap that payload in the
transport length header + send it on ch30, gated by a new flag, on both machines.** Then a two-machine
confirm run watching `member+0x152` / flags `(0,0,1,0)` (read with `scripts/re/capture-endpoint.py`) and
`players → 2`.

### Decisions we'll face (in order)

1. **Plain-frame-first, or capture-the-bytes-first?** *(Recommendation: plain first — it's free.)* The plain
   frame is `[byte0=len&0xff][byte1=0x40|((len>>8)&7)][5][8B token][4B ticket_len][ticket]` (the len header
   is the same 11-bit scheme our old 14-byte SYN used). Try it first — no ERSC restore. **If it doesn't
   dispatch** (the chart flagged a possible *inner* transport frame beyond the len header; `0x142642860` /
   `0x1426408b0` are Arxan-opaque on disk), fall back to **capturing the exact on-wire bytes** of a real ERSC
   type-5 by hooking `SendP2PPacket` ch30 on a live ERSC session, and copy the framing verbatim. (Small
   targeted capture — just the bytes — not a full graph dump.)
2. **The 8B token — send zeros, or a real value?** The validator stores it unvalidated (`conn+0x148`), so
   **any 8 bytes should pass.** Start with zeros; only revisit if the pump's *pre-validator* framing cares.
3. **Who sends — symmetric (both) or a real joiner?** In `symmetric_peer` mode both machines build the
   other's endpoint, so **both can send their own ticket to the other** (each completes the other's member) —
   the simplest first attempt. If symmetric misbehaves (both think they're host), fall back to an asymmetric
   host+joiner and send only from the joiner. *(Try symmetric first; it's what's wired.)*
4. **Ticket validity timing.** `GetAuthSessionTicket` fills bytes synchronously but the ticket is only
   *accepted* by a remote `BeginAuthSession` after Steam fires `GetAuthSessionTicketResponse_t` (~ms) — and
   the game owns that callback pump. So **fetch once early and retry the send on a throttle**; the first sends
   may bounce as `InvalidTicket` until it goes valid. Confirm the validator's accept set is `{0,2}` (k_EResultOK / k_EResultOKPending-ish).
5. **The big fork — if hand-frame ALSO hits an Arxan wall.** If the inner transport frame turns out to be
   Arxan-produced too (not just obfuscated-on-disk but genuinely unforgeable), then producing a type-5
   *offline* is blocked, and the fallback is to **drive the game's own connect flow into the state where it
   sends the type-5 itself** (its protected control flow decodes the Arxan slot) — a much bigger RE effort
   (chart what gates the send, reach that state). Only go here if choice 1's capture shows the framing is
   unreproducible. *(Not expected — the ticket is the only validated field — but name it so we don't thrash.)*

**Watch item (latent, fix when the handshake lands):** the client's self-identity
`[[member+0x58]+0x7f8]==0` under the bit-2-OR lever misroutes the client's *self* member; set `+0x7f8`
directly alongside the bit-2 OR once a session establishes.

## Candidates Not Chosen

- **Seamlessness disconnect-suppression gate — ALREADY SHIPPED (2026-07-04), don't rebuild.**
  `gameplay.stay_connected` (default off): hooks both charted sites, settings toggle, host-tested core, toast,
  install+arm rig-validated solo (`7ad9000`/`38917c0`/`1981f70`). Remaining is rig-gated only: live 2-player
  validation + caller→cause attribution in the suppression log — both unblock the moment co-op is 2-player.
- **Post-rung-3 wiring** (inert overlay toggles, nameplate color-by-SteamID, session-roster event toasts) —
  gated on 2-player co-op by definition; unblocked the moment `players=2` lands.
- **Drive the game's own type-5 send `0x142400df0`** — RULED OUT (runs 13–15): Arxan-locked vtable slot,
  crashes cold (see Now). Kept observe-only for diagnosis; do not re-enable.
- **Let the game's endpoints complete the handshake on their own** — RULED OUT (run 12): suppressing our SYN
  didn't make `+0x152` appear; the game's flow never produces the type-5 in our driven setup.
- **The transport gate-c / `[S+0x168]` / find-or-create path & levers 3a/3b** — RED HERRING (runs 4–11):
  the `[S+0x168]` stub is present even in working ERSC; not the admission mechanism.
- **Joiner-side `drive_add_peer` (symmetric add-peer at the session layer)** — RULED OUT (run 9): crashes the
  Client session (null `+0x5f8`, a Host-only sub-object).
- **`force_gatec_accept` / fabricating transport admit** — moot: the wall was never the transport admit.

## Learned Recently (Pointers Only)

- **The whole type-5 / handshake trail** → [SESSION-DRIVE.md](SESSION-DRIVE.md): "★ TYPE-5 PRODUCER +
  INJECTION" (send `0x142400df0`, validator `0x142402ee0`, injection path), "★★ ENDPOINT CAPTURED" +
  "★★★ HOST BUILDS THE JOINER ENDPOINT" (the pump builds `member+0x130`), and "▶ RIG RESULT (runs 10–15)"
  (the red-herring confirm, option-a disproof, the Arxan-locked send). The 8-type protocol + type-5 payload:
  [ERSC-LIVE-CAPTURE-FINDINGS.md](ERSC-LIVE-CAPTURE-FINDINGS.md) > "★ The DLNW3D connect protocol" +
  correction #1 (the `[context+0x168]` stub is stub-in-working-ERSC too).
- **Arxan gotcha** → the [`/reverse-engineer`](../.claude/skills/reverse-engineer/SKILL.md) skill >
  "Capturing Arxan-Decoded Call Targets": you **can't drive (call cold)** a game function whose own dispatch
  goes through an Arxan slot; the tell is a call target below the image base, identical across ASLR launches —
  observe-log the slot before calling, and replicate the effect without the dispatch instead.
- **Code on `main`:** `core::dlnw3d` (host-tested type-5 framing) + `steam::get_auth_session_ticket` (the
  scaffold, `be7f64e`); `session_probe.rs` — the type-5 producer (OBSERVE-ONLY, `ec691cc`), the endpoint-set
  latch `0x14203ef70`, `drive_add_peer`, `symmetric_peer`/`suppress_syn` levers. Config gates in
  `unseamless-core/config.rs`; the seed (`scripts/rig/seed-config.toml`) documents each flag inline.
- **Current seed for the next run** (`scripts/rig/seed-config.toml`): `symmetric_peer` + `drive_add_peer` ON
  (both build endpoints); `suppress_syn` ON (SYN retired, proven noise); `drive_type5` OFF (send-drive is
  observe-only/retired — the hand-frame sender will get its own new flag); both machines `--auto-session host`.
- **Workflow/rig learnings (this session):** a running game is NOT a blocker — kill-first then cycle
  (CLAUDE.md); `/next` git-greps `main` before delegating a "build this" lane (a stale entry once spawned a
  dup lane); don't chain `sleep N; cmd` (harness blocks it — poll or `run_in_background`) (global CLAUDE.md).
- **Ground-truth ERSC captures** (persistent, uncommitted): `~/Documents/ersc-session-ref-{host,client}.txt`
  + `ersc-live-capture*.txt` — the real 2-player member graph, the type-5 payload, the mirrored endpoints.
