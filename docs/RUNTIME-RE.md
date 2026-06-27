# Runtime / behavioral RE on the Proton rig

How we observe live behavior — game state, hooked functions, network traffic — to drive the
clean-room reimplementation. This is the work that needs the game running, on the Linux + Proton
rig.

Read this alongside [DEVELOPMENT.md](DEVELOPMENT.md) (toolchain, build/run workflow) and
[FEATURES.md](FEATURES.md) (what we're trying to reproduce).

## First: what is NOT blocked on this

Most development does not need any of this. Before reaching for runtime RE, note that without the
game running we can already:

- Build every **SDK-driven feature** (the `[SCALING]` params, `allow_summons`,
  `skip_splash_screens`, death debuffs, etc.). These are typed-field/param writes
  the `fromsoftware-rs` SDK already exposes — write Rust, cross-compile, deploy + eyeball the
  log on the rig. That's the **M1** milestone and it needs no Frida.
- **Statically analyze clean binaries** — `eldenring.exe` is well-charted (the SDK is built
  against it); Ghidra headless / rizin here cover it.

Runtime RE is for the **unknowns**: the networking/session protocol, player-state sync, and any
game function ERSC hooks that the SDK doesn't already name. That's **M2/M3** territory.

## Two ways to observe at runtime (pick per task)

### Option A — our own diagnostic DLL (default, recommended)

We already build and load DLLs into the game reliably (our own `dinput8.dll` proxy + the task-hook
harness). The cheapest, most clean-room-friendly observer is a Rust **diagnostic build of our
own mod** that logs what we want, exactly like er-crit-coop's `src/diagnostic.rs` (snapshot
state, log rising-edge bit flips, dump SpEffects). For protocol work, the same DLL can detour
or IAT-hook the Steam networking calls (`steam_api64.dll` `SteamNetworking*` / `SteamNetworkingSockets*`)
and log payload sizes/bytes.

- Pros: reuses our toolchain; no Proton/Frida friction; observations are first-party (we write
  what we log, in our own words → naturally clean-room).
- Cons: each change is a recompile + redeploy (our build+deploy loop is fast, so this is minor).

Use this for steady, known-shape observation.

### Option B — Frida (frida-gadget), for fast iterative hooking

Frida lets you hook/trace arbitrary functions and rewrite instrumentation live (JS, no
recompile). The catch on this rig: the game is a **Windows PE under Proton/Wine**, so attaching
Frida from the Linux host to the process does **not** work the normal way. The workable path is
**frida-gadget**: a Windows DLL loaded *into* the game (same injection path we already use)
that opens a local Frida server you connect to.

Setup on the rig (one time):

1. **Install the host CLI** (Linux): `pipx install frida-tools` (gives `frida`, `frida-trace`).
2. **Get the gadget** matching the game arch: download `frida-gadget-<ver>-windows-x86_64.dll`
   from the Frida releases, rename to something like `frida-gadget.dll`, and drop it in `mods/` —
   our own `dinput8.dll` proxy loads DLL mods from there.
3. **Configure it** with a sibling `frida-gadget.config` (same basename), listen mode:
   ```json
   { "interaction": { "type": "listen", "address": "127.0.0.1", "port": 27042,
       "on_load": "wait" } }
   ```
   Wine uses host networking, so the port is reachable on the Linux host's localhost.
4. **Connect from the host** and run a script:
   ```bash
   frida -H 127.0.0.1:27042 -l trace-steamnet.js   # or: frida-trace -H ... -i 'SteamNetworking*'
   ```

- Pros: hook anything, iterate instrumentation without rebuilding; great for mapping an unknown
  call graph quickly (e.g. "what does ERSC call when I press the open-world hotkey").
- Cons: the Proton/gadget setup above is fiddly; payloads still need interpretation.

> Frida scripts are just JavaScript — edit them and reload without rebuilding the DLL; they hook
> the game live once it's running on the rig.

### Option C — network capture (transport-level, complementary)

For the wire side specifically, capture alongside hooking: `ss -tunp`, `tcpdump`/Wireshark on
the rig to see Steam relay vs. direct P2P, ports, and volume. Payloads are encrypted/Steam-
framed, so capture tells you *shape and timing*; the hooks (A or B) tell you *contents*.

## Recommended approach for this project

1. **M1 now** — SDK-driven features, no runtime RE.
2. **M2 transport spike** — start with **Option A** (a diagnostic DLL that hooks the
   `steam_api64` networking calls and logs them) since we already have that toolchain. Add
   **Frida (Option B)** only if/when fast iterative hooking pays for the Proton setup cost.
3. Keep all observations as **behavioral notes in our own words** (clean-room), feeding
   FEATURES.md and the implementation — never transcribe ERSC internals.

## Clean-room reminder

Runtime observation watches *behavior*, which is the safe side of the line. Don't dump or
commit upstream memory/code; record what you learn in your own words and implement from that.
See CLAUDE.md > "Clean-room hygiene".
