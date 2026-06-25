//! Statically link the mingw C++/GCC runtime, so the cross-compiled cdylib is self-contained — no
//! `libstdc++-6.dll` dependency that the game's Wine prefix lacks (LoadLibrary would fail). hudhook's
//! imgui-sys is C++ and is always compiled in, so this is unconditional. Requires `CXXSTDLIB=""` (set
//! in `.cargo/config.toml`) so imgui-sys doesn't emit a competing *dynamic* `stdc++` link that rustc
//! would force `-Bdynamic`, defeating a static one.

fn main() {
    // This recipe depends only on this file (and the mingw toolchain, which Cargo can't track);
    // pin the rerun to it so an unrelated source change doesn't needlessly relink.
    println!("cargo:rerun-if-changed=build.rs");

    // Link args land at the END of the command (after imgui-sys's `cimgui.a`), so the static runtime
    // archives that resolve its C++ symbols come after the objects that use them — the ld ordering
    // rule. `--start-group/--end-group` resolves the circular deps among the GCC runtime archives
    // (libstdc++ ↔ libgcc/libgcc_eh ↔ libwinpthread). `-l:libX.a` forces the static archive over the
    // shared import lib.
    for arg in [
        "-Wl,--start-group",
        "-l:libstdc++.a",
        "-l:libgcc.a",
        "-l:libgcc_eh.a",
        "-l:libwinpthread.a",
        "-Wl,--end-group",
        // System import libs the static C++/pthread runtime pulls (the CRT for sprintf et al., which
        // mingw maps onto the UCRT `api-ms-win-crt-*`; kernel32 for the semaphores/critical sections
        // libwinpthread uses). They must follow the group for ld's left-to-right resolution. rustc's
        // gnu target already links most of these, but repeating them here is what puts them *after*
        // the group. Extend this list if a mingw bump introduces a new unresolved external.
        "-lmsvcrt",
        "-lkernel32",
        "-ladvapi32",
        "-luser32",
    ] {
        println!("cargo:rustc-link-arg-cdylib={arg}");
    }
}
