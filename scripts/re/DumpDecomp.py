# Ghidra headless post-script (Jython) — dump decompiled C to stdout.
#
# Driven by scripts/re/ghidra-decompile.sh; not run by hand. With no arg it decompiles every
# function; with one arg (a function name or hex address) it decompiles just that function.
#
# Clean-room note: this is generic tooling. Point it at CLEAN targets (the game exe, our own
# builds, an unpacked dump) — never commit decompiler output of an upstream closed binary.
# See CLAUDE.md > "Clean-room hygiene".

from ghidra.app.decompiler import DecompInterface
from ghidra.util.task import ConsoleTaskMonitor

program = currentProgram
fm = program.getFunctionManager()

decomp = DecompInterface()
decomp.openProgram(program)
monitor = ConsoleTaskMonitor()


def dump(func):
    res = decomp.decompileFunction(func, 60, monitor)
    if res is not None and res.decompileCompleted():
        print(res.getDecompiledFunction().getC())
    else:
        print("// DECOMPILE FAILED: %s @ %s" % (func.getName(), func.getEntryPoint()))


args = getScriptArgs()
target = args[0] if len(args) > 0 else None

if target:
    funcs = [f for f in fm.getFunctions(True) if f.getName() == target]
    if not funcs:
        try:
            f = fm.getFunctionContaining(toAddr(target))
            if f is not None:
                funcs = [f]
        except:
            pass
    if not funcs:
        print("// no function matched: %s" % target)
    for f in funcs:
        dump(f)
else:
    for f in fm.getFunctions(True):
        dump(f)
