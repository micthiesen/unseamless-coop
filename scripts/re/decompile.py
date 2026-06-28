# Standalone PyGhidra decompiler — no GUI, no analyzeHeadless ceremony.
#
# Driven by scripts/re/ghidra-decompile.sh (which bootstraps the pyghidra venv and sets
# GHIDRA_INSTALL_DIR); can also be run directly with a python that has `pyghidra` installed:
#
#   GHIDRA_INSTALL_DIR=/opt/ghidra python decompile.py <binary> [function-name-or-hex-addr]
#
# With no function arg it dumps every function (slow on big targets); with one it dumps just
# that function (by name, else the function containing that hex address). Decompiled C goes to
# stdout. The Ghidra project is cached under <repo>/.ghidra-projects/ (gitignored) keyed by the
# binary name, so re-runs skip re-analysis.
#
# Why PyGhidra and not a Jython postScript: Ghidra 12.1 dropped Jython from the default install
# (it's now an opt-in GUI-installed extension), so the old analyzeHeadless + Jython post-script
# path no longer runs without the GUI. PyGhidra (CPython via JPype) is the supported headless
# scripting surface, and gives us the full standard library for ad-hoc analysis.
#
# Clean-room note: generic tooling. Point it at CLEAN targets (the game exe, our own builds, an
# unpacked dump) — never commit decompiler output of an upstream closed binary, and don't bother
# pointing it at ersc.dll (Themida-virtualized; you'd decompile the unpacker stub, not the logic).
# See CLAUDE.md > "Clean-room hygiene".

import os
import re
import sys
import tempfile
import warnings
from pathlib import Path

warnings.filterwarnings("ignore", category=DeprecationWarning)  # open_program() deprecation noise

import pyghidra  # noqa: E402  (must follow the warnings filter)


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: decompile.py <binary> [function-name-or-hex-addr]", file=sys.stderr)
        return 2
    binary = Path(sys.argv[1]).resolve()
    target = sys.argv[2] if len(sys.argv) > 2 else None
    if not binary.is_file():
        print(f"no such file: {binary}", file=sys.stderr)
        return 1

    # Ghidra project cache. It canNOT live inside the repo: Ghidra's ProjectLocator rejects any
    # path element starting with '.', and this repo is a rift workspace under ~/Code/.rifts/...
    # (and the in-repo cache dir is .ghidra-projects), so an in-repo location always trips that.
    # Default to a non-dotted temp dir; override with $GHX_PROJECT_DIR. Loss on reboot just means
    # re-analysis, which is cheap relative to keeping it out of the dotted rift path.
    proj_dir = Path(os.environ.get("GHX_PROJECT_DIR") or
                    (Path(tempfile.gettempdir()) / "unseamless-ghidra-projects"))
    proj_dir.mkdir(parents=True, exist_ok=True)
    proj_name = "ghx_" + re.sub(r"[^A-Za-z0-9_]", "_", binary.name)

    pyghidra.start()
    from ghidra.app.decompiler import DecompInterface
    from ghidra.util.task import ConsoleTaskMonitor

    # open_program reuses the cached project+analysis when it already exists for this binary.
    with pyghidra.open_program(
        str(binary),
        project_location=str(proj_dir),
        project_name=proj_name,
        analyze=True,
    ) as api:
        program = api.getCurrentProgram()
        fm = program.getFunctionManager()
        decomp = DecompInterface()
        decomp.openProgram(program)
        monitor = ConsoleTaskMonitor()

        def dump(func) -> None:
            res = decomp.decompileFunction(func, 60, monitor)
            if res is not None and res.decompileCompleted():
                print(res.getDecompiledFunction().getC())
            else:
                print("// DECOMPILE FAILED: %s @ %s" % (func.getName(), func.getEntryPoint()))

        if target:
            funcs = [f for f in fm.getFunctions(True) if f.getName() == target]
            if not funcs:
                try:
                    f = fm.getFunctionContaining(api.toAddr(target))
                    if f is not None:
                        funcs = [f]
                except Exception:
                    pass
            if not funcs:
                print("// no function matched: %s" % target)
            for f in funcs:
                dump(f)
        else:
            for f in fm.getFunctions(True):
                dump(f)
    return 0


if __name__ == "__main__":
    sys.exit(main())
