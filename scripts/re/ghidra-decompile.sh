#!/usr/bin/env bash
# Headless Ghidra decompile — no GUI. Drives analyzeHeadless to import a binary, auto-analyze
# it, and dump decompiled C to stdout via DumpDecomp.py.
#
#   scripts/re/ghidra-decompile.sh <binary> [function-name-or-hex-addr]
#
# With no function arg it dumps every function (slow on big targets); with one it dumps just
# that function. Caches the Ghidra project under .ghidra-projects/ (gitignored) keyed by the
# binary, so re-runs skip re-analysis.
#
# Clean-room note: intended for CLEAN targets (the game exe, our own builds, an unpacked dump).
# ersc.dll is Themida-virtualized, so this won't recover its logic — and never commit
# decompiler output of an upstream closed binary. See CLAUDE.md > "Clean-room hygiene".
set -euo pipefail

BIN="${1:?usage: ghidra-decompile.sh <binary> [function-name-or-addr]}"
FUNC="${2:-}"
[[ -f "$BIN" ]] || { echo "no such file: $BIN" >&2; exit 1; }

HEADLESS="$(find /opt/homebrew/Caskroom/ghidra -name analyzeHeadless -type f 2>/dev/null | head -1)"
[[ -n "$HEADLESS" ]] || { echo "analyzeHeadless not found — is the ghidra cask installed?" >&2; exit 1; }

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PROJ_DIR="$ROOT/.ghidra-projects"
mkdir -p "$PROJ_DIR"

name="$(basename "$BIN")"
proj="ghx_${name//[^A-Za-z0-9_]/_}"

# First run imports + analyzes; later runs reuse the project (-process instead of -import).
if [[ -f "$PROJ_DIR/$proj.gpr" ]]; then
  mode=(-process "$name")
else
  mode=(-import "$BIN")
fi

"$HEADLESS" "$PROJ_DIR" "$proj" \
  "${mode[@]}" \
  -scriptPath "$SCRIPT_DIR" \
  -postScript DumpDecomp.py ${FUNC:+"$FUNC"} \
  -analysisTimeoutPerFile 1200 \
  2> >(grep -vE 'INFO|REPORT|WARN|Using|Picked' >&2 || true)
