# x64dbg MCP on CachyOS

This repository carries a pinned, project-scoped setup for
[duty1g/x64dbg-mcp-server](https://github.com/duty1g/x64dbg-mcp-server). It gives Codex authenticated
debugger tools for our own ELDEN RING process: breakpoints, registers, memory, threads, modules,
call stacks, pattern scans, and debugger events. It is an additional live-RE surface, not a
replacement for the diagnostic DLL or the native watchpoint.

## Safety Shape

- The plugin binds to `127.0.0.1:9094`, never the upstream `0.0.0.0` default.
- A random bearer token lives in gitignored `.re-tools/x64dbg-mcp/`, mode `0600`.
- `.codex/config.toml` is project-scoped, optional, and prompts before debugger tools annotated as
  writes. The project still opens when the server is absent.
- x64dbg, its plugin, debugger databases, and any dumps stay under `.re-tools/` and must never be
  committed. The clean-room rule still applies: record behavioral findings in your own words, never
  paste disassembly or decompiler output into source or commits.

## One-Time Install

The helper downloads x64dbg `2026.05.27` and plugin `1.0`, verifies pinned SHA-256 hashes, installs
both under `.re-tools/`, generates the token, and writes the loopback-only plugin configuration.

```bash
scripts/re/x64dbg-mcp.sh install
scripts/re/x64dbg-mcp.sh doctor
```

The archive setup and security configuration can run on macOS. Running x64dbg and attaching to the
game remain CachyOS-only validation items.

## Start a Session

Source the generated token before starting Codex from this repository:

```bash
source .re-tools/x64dbg-mcp/env.sh
codex
```

In another terminal, launch x64dbg inside ELDEN RING's existing Proton prefix:

```bash
scripts/re/x64dbg-mcp.sh launch
scripts/re/x64dbg-mcp.sh probe
```

Use x64dbg's process list or the MCP `AttachProcess` tool to attach to `eldenring.exe`. A PID passed
to `launch` must be x64dbg's Windows PID, not the Linux `/proc` PID. Override discovery when needed:

```bash
X64DBG_PROTON='/path/to/proton' \
X64DBG_COMPAT_DATA='/path/to/steamapps/compatdata/1245620' \
scripts/re/x64dbg-mcp.sh launch
```

## Evaluation Gate

On CachyOS, the tool is worth keeping if it can attach without destabilizing Proton, pause at a
known-safe function boundary, read the session manager, set a hardware watchpoint on the
`join_wait` field, and report the writer and call stack. Start read-only. Pausing, stepping, patching,
and terminating the debuggee affect the live game and should remain prompted operations.

If Wine attachment proves brittle, keep the native `watch-write.py` path as the reliable writer
finder and remove the MCP config and helper. Do not gut the established tools until this gate passes.
