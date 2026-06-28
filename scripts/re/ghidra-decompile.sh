#!/usr/bin/env bash
# Headless Ghidra decompile — no GUI. Bootstraps a PyGhidra venv and runs decompile.py to dump
# decompiled C to stdout.
#
#   scripts/re/ghidra-decompile.sh <binary> [function-name-or-hex-addr]
#
# With no function arg it dumps every function (slow on big targets); with one it dumps just that
# function. Re-runs reuse a cached Ghidra project (keyed by the binary) so they skip re-analysis;
# that cache lives in a temp dir, not the repo, because Ghidra's ProjectLocator rejects any path
# element starting with '.' and this repo sits under a dotted rift path (override with
# $GHX_PROJECT_DIR). The pyghidra venv lives in .ghidra-projects/ (gitignored).
#
# Ghidra 12.1 dropped Jython from the default install, so the old analyzeHeadless + Jython
# post-script path no longer runs headless. This drives PyGhidra (CPython via JPype) instead —
# the supported headless surface. First run creates the venv + installs the pyghidra wheel that
# ships inside Ghidra; later runs reuse it.
#
# Clean-room note: intended for CLEAN targets (the game exe, our own builds, an unpacked dump).
# ersc.dll is Themida-virtualized, so this recovers the unpacker stub, not its logic — and never
# commit decompiler output of an upstream closed binary. See CLAUDE.md > "Clean-room hygiene".
set -euo pipefail

BIN="${1:?usage: ghidra-decompile.sh <binary> [function-name-or-addr]}"
[[ -f "$BIN" ]] || { echo "no such file: $BIN" >&2; exit 1; }

# Locate the Ghidra install: honor $GHIDRA_INSTALL_DIR / $GHIDRA_HOME, else common Linux paths
# (the Arch `ghidra` package installs to /opt/ghidra).
GHIDRA_DIR=""
for d in "${GHIDRA_INSTALL_DIR:-}" "${GHIDRA_HOME:-}" /opt/ghidra /usr/share/ghidra /opt/ghidra*; do
  [[ -n "$d" && -d "$d" && -f "$d/support/analyzeHeadless" ]] || continue
  GHIDRA_DIR="$d"; break
done
[[ -n "$GHIDRA_DIR" ]] || { echo "Ghidra not found — install it (pacman -S ghidra) or set GHIDRA_INSTALL_DIR" >&2; exit 1; }
export GHIDRA_INSTALL_DIR="$GHIDRA_DIR"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PROJ_DIR="$ROOT/.ghidra-projects"
VENV="$PROJ_DIR/venv"
mkdir -p "$PROJ_DIR"

# Bootstrap the pyghidra venv once. The wheel ships inside Ghidra; the system python is
# externally managed (PEP 668), so a dedicated venv is the clean way to install it.
if [[ ! -x "$VENV/bin/python" ]] || ! "$VENV/bin/python" -c "import pyghidra" 2>/dev/null; then
  echo "bootstrapping pyghidra venv at $VENV ..." >&2
  python3 -m venv "$VENV"
  WHEEL="$(find "$GHIDRA_DIR/Ghidra/Features/PyGhidra/pypkg/dist" -name 'pyghidra-*.whl' 2>/dev/null | head -1)"
  [[ -n "$WHEEL" ]] || { echo "pyghidra wheel not found under $GHIDRA_DIR" >&2; exit 1; }
  "$VENV/bin/pip" install -q "$WHEEL"
fi

# Drop Ghidra's INFO/WARN/REPORT chatter and the bundled-analyzer stack traces from stderr;
# decompiled C still goes to stdout cleanly.
exec "$VENV/bin/python" "$SCRIPT_DIR/decompile.py" "$@" \
  2> >(grep -vE 'INFO|REPORT|WARN|Using|Picked|^\s+at |Exception|Error in analy' >&2 || true)
