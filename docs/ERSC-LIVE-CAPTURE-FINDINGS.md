# ERSC Live-Capture Findings (2026-07-04)

What a **real, working 2-player ERSC co-op session** looks like in memory, captured live. This is the
first time we've observed a *successful* session establishment (rather than our offline attempts), and
it corrects two load-bearing wrong beliefs that drove months of offline work.

## Method

- **Rig** ran the user's real ERSC stack (restored via `rig.sh restore`), hosting, password `salmon`.
- **Deck** was provisioned with the *same* ERSC (copied the rig's exact `ersc.dll` + launcher + config,
  version-matched: both buildid `22984413`), on its own Steam account `testthiesen`
  (`76561198681631498`), with the DLC-free test save copied to `ER0000.co2`. It joined via password.
- Both players linked, in the same world (`lobby_state = 3 Host` on the rig).
- Capture was **standalone ptrace** (`/proc/<pid>/mem` read-only + `scan-vtable`; `ptrace_scope=0`, no
  mod loaded). Raw dumps: `~/Documents/ersc-live-capture*.txt`.

> **Tooling bug found + fixed:** `scan-vtable.py` skipped any mapping `> 1GB`, so it **missed the entire
> Wine/Proton high heap (`0x7fff…`)** where all the live session objects live — it first reported "0
> `SessionSteam`" when there was one. A chunked full-region scan found everything. `scan-vtable.py` now
> chunks large regions (don't trust an old "0 objects" result from before this fix).

## The two corrections (both load-bearing)

### 1. `[context+0x168]` is the reject-stub EVEN in a working session — the gate-c theory is dead

Prior belief (CLAUDE.md status, SESSION-DRIVE Lane B, months of "avenue a"): admission fails offline
because the transport context's member-lookup at `+0x168` is a reject-stub `0x1423fdf00`, and the
*online flow installs a real lookup* there. **False.** In the live working session:

```
context (MTInternalThreadSteamSocket) @ 0x7fff66cf84d0
  +0x168 = 0x1423fdf00   ← the SAME reject-stub, in a fully working 2-player session
```

So the transport admit gate-c (`0x142640ecd` → `[context+0x168]`) is **not** how real members are
admitted. Members are built by the **session layer** (`SessionSteam` → `SessionMemberSteam`), and the
`+0x168` stub is simply always the stub. **Avenue (a) — "synthesize a real `+0x168` lookup" — is
unnecessary and was never the mechanism.** Stop trying to make `+0x168` real; stop treating the joiner's
transport SYN → gate-c as the admission path.

### 2. The full DLNR3D/DLNW3D object graph IS the real mechanism — and it all exists here

Everything our offline standup couldn't complete is present and live in a real session (all
RTTI-confirmed classes, found once the scan covered the high heap):

| Class | vtable | live count | address(es) |
|---|---|---|---|
| `DLNR3D::SessionSteam` | `0x1431fa248` | 1 | `0x7fff66cf08e0` |
| `DLNR3D::SessionMemberSteam` | `0x1431fa978` | **6** | `0x7fff66cf2070` + `0x1280` each (6 pre-alloc slots; ERSC max 6) |
| `DLNW3D::MTInternalThreadSteamSocket` (context) | `0x1432770b0` | 1 | `0x7fff66cf84d0` |
| `DLNW3D::SteamConnectionManager` (socketmgr) | `0x143278020` | 2 | `0x18cc950` (dormant), **`0x7fff66f1b8c0` (live)** |
| `DLNW3D::SteamServiceImpl` | `0x143277270` | 1 | `0x7fff66cdfe00` |
| `DLNR3D::SocketManagerHolder` | `0x1431f9280` | 1 | `0x7fff66cefce0` |
| `DLNR3D::ManagerImplSteam` (container) | `0x1431f8780` | 3 | co-op one = `0x143dcd3d0` (static) |
| `DLNR3D::SessionManagerSteam` | `0x1431f9140` | 3 | co-op one = `0x143dcdae0` (= container+0x710) |

**`SteamServiceImpl` is a real object here** — the exact standup (`0x142638b40`) that returns **null**
offline (the "native-builder dead end"). So the offline wall is specifically: *the establishment that
builds this graph doesn't complete offline* — NOT that a magic callback is missing.

## The live object graph (offsets, for reproduction)

```
container ManagerImplSteam 0x143dcd3d0 (STATIC)
  +0x8   = 0x143dcd3d0 (self/owner back-ptr)
  +0x48  = 0x143d87750  (config/owner — the SteamServiceImpl standup reads this; STATIC, so present
                         offline too — the standup's null offline is NOT simply "+0x48 is null")
  +0x1e8 = 0x142bbce18  (member-registry root; members hook in here)
  +0x708 = 0x7fff66cefce0  → SocketManagerHolder (LIVE; null in our offline standup until we build it)
  +0x710 = embedded SessionManagerSteam (= 0x143dcdae0; its array/+0x20 cap=16/+0x24 count=1)

SessionSteam 0x7fff66cf08e0 (vtable 0x1431fa248)
  +0x8/+0x10 = 0x143dcd5b8  (member list; = container+0x1e8 region)
  +0x58  = 0x143dcd3d0  (container back-ptr)
  +0x570 = 0x143d87750  (same config object as container+0x48)
  +0x5a8 = secondary-interface sub-object (vtable 0x1431fa918)

SessionMemberSteam[0] 0x7fff66cf2070 (vtable 0x1431fa978)   [6 slots, +0x1280 stride]
  +0x58  = 0x143dcd3d0  (container)          +0x60 = SessionSteam
  +0x70  = handle1 (vtable 0x1431f85d8; its +0x18 → SocketManagerHolder)  ← add-member arg1
  +0x78  = handle2 (vtable 0x1431fa4a8; peer/connection identity)          ← add-member arg2
  +0xa4  = 0x10002 (flags)

live SteamConnectionManager 0x7fff66f1b8c0 (vtable 0x143278020)
  +0x40  = 0x142639810  (gate-c callback thunk)     +0x48 = context (0x7fff66cf84d0)
  +0xb8/+0xc0 = connection span [begin,end) — 1 live connection

SteamConnection 0x7fff66f1d9f0 (vtable 0x143278358)
  +0x138 = peer SteamID64 (individual format 0x0110000100000000 | accountID)

context MTInternalThreadSteamSocket 0x7fff66cf84d0 (vtable 0x1432770b0)
  +0x48  = SessionSteam    +0x168 = 0x1423fdf00 (STUB — see correction #1)    +0x170 = SessionSteam
```

**Real Steam P2P, keyed by SteamID64.** Both players' `SteamID64`s appear throughout the session heap
(Deck `76561198681631498` ×7, rig ×11 in the session region) and in `SteamConnection+0x138`. This is
exactly the peer identity our **rung-4 side-channel already discovers** — no server broker blob needed,
consistent with the earlier legacy-P2P finding.

## Member slots + the two add-member args (decoded)

- **6 member slots, only 2 real.** Searching each slot's block for the players' SteamID64s: **member[4]
  = the Deck (joiner), member[5] = the Rig (host / self)**; slots [0]–[3] are empty pre-allocated slots
  (`+0xa4 = 0x10002` default flags, no ID). So a live session pre-allocates all `max_players` member
  objects; only the connected ones carry a peer identity. (Active slots were [4]/[5], not [0]/[1] — slot
  index isn't join-order-dense.)
- **add-member arg1 (`member+0x70`, handle1, vtable `0x1431f85d8`)** = a small refcounted wrapper
  (`+0x8` = refcount) whose **`+0x18` points at the single `SocketManagerHolder`** (`0x7fff66cefce0`).
  Every member's handle1 references the *same* holder — it's the host's shared transport, ref-wrapped
  per member. So arg1 is essentially "a ref to the host socket-manager holder."
- **add-member arg2 (`member+0x78`, handle2, vtable `0x1431fa4a8`)** = the **peer-identity object**: an
  inline small-string (`+0x20`/`+0x28`/`+0x48` point at adjacent in-object buffers) plus tag fields. This
  is the per-peer identity/name handle.
- **`SteamConnection` `0x7fff66f1d9f0`** (vtable `0x143278358`): `+0x18` → live `SteamConnectionManager`
  (back-ptr), `+0x138` = peer `SteamID64` (`0x0110000100000000 | accountID`), block contains both IDs.
- **member registry** = `SessionSteam+0x8`/`+0x10` = `0x143dcd5b8` (= `container+0x1e8` region), a
  map/container holding the member objects.

⇒ To reproduce add-member we must supply (1) a ref to the host's `SocketManagerHolder` (we already build
that offline via `stand_up_transport`) and (2) a peer-identity handle built from the rung-4 SteamID64.
Neither is exotic — both are downstream of standing up the host transport + knowing the peer ID, which we
already have. This is far more tractable than the abandoned "synthesize a real `+0x168`" plan.

## What this means for the pivot (reproduce target, refined)

The pivot ("let the game establish it") is **validated** — ERSC drives the game's own DLNR3D/DLNW3D
machinery, and the whole graph is native game objects we can now enumerate. The reproduce target is now
precise and the `+0x168`/gate-c rabbit hole is closed:

1. **Drive the game's establishment to build `SessionSteam` + its `SessionMemberSteam`(s) + stand up the
   transport (`SteamServiceImpl` + live `SteamConnectionManager` + `SteamConnection`), keyed to the
   rung-4-discovered peer SteamID64.** We already build the transport primitives offline
   (`stand_up_transport`); the gap is completing the *session-layer* establishment so the graph is wired
   as above and `lobby_state → Host` sticks with members.
2. **Resolve the one concrete wall: why `SteamServiceImpl` standup (`0x142638b40`) returns null offline
   but yields `0x7fff66cdfe00` here.** `[container+0x48]` (its config) is a *static* object present
   offline too, so the difference is subtler — chart what the standup reads/requires using this live
   object as the "known-good" reference (diff its state against an offline standup attempt).
3. Next concrete RE step: statically chart the establishment call sequence that produces this graph
   (now that we know the exact classes/offsets), and/or a **second live capture that watches the
   *writers*** — arm a hardware watchpoint (via the static `SessionManagerSteam`/container fields, or the
   member-registry root `container+0x1e8`) during a fresh Deck join to catch the RIPs that add the member
   and stand up the service.

Superseded by this doc: the "avenue (a) synthesize the member + real `+0x168`" plan and the "joiner SYN
→ gate-c admit" framing in SESSION-DRIVE.md / CLAUDE.md status (both kept as history).

## Re-running this capture (for the writer-trace follow-up, task #16)

The exact setup, so a future session can stand up a live ERSC session and watch it fast:

1. **Rig → real ERSC:** `scripts/rig.sh restore` (swaps our mod off, ERSC on). Password is in
   `…/ELDEN RING/Game/SeamlessCoop/ersc_settings.ini` (`cooppassword = salmon`, `save_file_extension =
   co2`). When done, `scripts/rig.sh apply` to put our mod back.
2. **Windowed launch:** `printf '1920 1080\n' > "${XDG_RUNTIME_DIR}/unseamless-rig-gamescope"` then
   `steam -applaunch 1245620` — the rig's `gamescope-wrapper.sh` consumes the one-shot flag and runs
   windowed (no flag = fullscreen). Michael loads his co-op character and **uses the in-game items to
   host** (this ERSC is item-driven host/connect, not auto-seamless); the game session only forms when a
   real peer connects — a solo host shows `lobby_state=0` and none of the session objects.
3. **Deck → ERSC peer** (`DECK_HOST=deck@10.10.1.57 DECK_PORT=2222`): back up the Deck's current mod
   (`start_protected_game.exe` + our `dinput8.dll`) to `~/deck-mod-backup-*`, **remove our `dinput8.dll`**
   (ERSC doesn't use it), then rsync the rig's `start_protected_game.exe` + `SeamlessCoop/{ersc.dll,
   ersc_settings.ini,locale}` to the Deck's Game dir. **Version must match** (both buildid `22984413` —
   copying the rig's exact `ersc.dll` guarantees the ERSC match). Deck runs its **own** Steam account
   (`testthiesen` `76561198681631498`, MostRecent, separate from the rig's main), co-op save =
   `…/compatdata/1245620/pfx/…/EldenRing/76561198681631498/ER0000.co2` (copy the DLC-free `ER0000.uco`
   test save to `.co2`; already signed for the Deck account). Michael launches ELDEN RING on the Deck and
   joins. (No Steam-friends requirement — confirmed both here and in the rung-4 tests.)
4. **Capture (standalone, no mod):** `[G]=0x143D7A4D0` holds the live `CSSessionManager*`; `lobby_state`
   at `[csm+0xc]`. Enumerate objects with `scripts/re/scan-vtable.py <vtable>…` (now chunks the high heap;
   the session objects live at `0x7fff…`). The vtables of interest are in the table above. Read fields with
   `/proc/<pid>/mem` (see the capture scripts, backed up in `~/Documents/ersc-live-capture*.txt`).
5. **Writer-trace (the follow-up's point):** arm `scripts/re/watch-write.py --addr <A> --access write`
   (or `watch-bt.py` for a backtrace) on a **stable** anchor while a fresh join happens — the heap objects
   realloc per session, so use a static/embedded anchor: `lobby_state` at `[csm+0xc]` (catches host-setup
   FSM writers on a leave+rehost), or the member-registry root `container+0x1e8` (`0x143dcd3d0+0x1e8`).
