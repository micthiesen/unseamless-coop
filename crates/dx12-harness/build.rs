//! Statically link the mingw C++/GCC runtime into the harness *exe*, so it has no `libstdc++-6.dll`
//! dependency — a native Windows box (and a plain Windows VM) lacks one just like the game's Wine
//! prefix does, and LoadLibrary/exe-load would otherwise fail. hudhook's imgui-sys is C++ and always
//! compiled in, so this is unconditional.
//!
//! This is the cdylib's `build.rs` recipe (crates/unseamless-coop/build.rs) re-pointed at a `bin`:
//! the only change is `rustc-link-arg-bins` instead of `rustc-link-arg-cdylib`. The same
//! `CXXSTDLIB=""` from `.cargo/config.toml` keeps imgui-sys's `cc` from emitting a competing dynamic
//! `stdc++` link. Keep the two link-arg lists in sync if a mingw bump changes one.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    emit_build_id();

    // Link args land at the END of the command (after imgui-sys's `cimgui.a`), so the static runtime
    // archives that resolve its C++ symbols come after the objects that use them (ld ordering).
    // `--start-group/--end-group` resolves the circular deps among the GCC runtime archives.
    // `-l:libX.a` forces the static archive over the shared import lib.
    for arg in [
        "-Wl,--start-group",
        "-l:libstdc++.a",
        "-l:libgcc.a",
        "-l:libgcc_eh.a",
        "-l:libwinpthread.a",
        // mingw runtime: defines `__mingwthr_key_dtor`, which libgcc's win32-threads gthr-win32.o
        // references (CI/Debian default to win32 threads). Inside the group; harmless under posix.
        "-lmingw32",
        "-Wl,--end-group",
        // System import libs the static C++/pthread runtime pulls; must follow the group for ld's
        // left-to-right resolution. Extend if a mingw bump introduces a new unresolved external.
        "-lmsvcrt",
        "-lkernel32",
        "-ladvapi32",
        "-luser32",
    ] {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
}

/// Bake a short build id (`<short-sha>` or `<short-sha>-dirty`, else `nogit`) into the exe as the
/// `UNSEAMLESS_BUILD_ID` compile-time env, read via `option_env!` for the harness's startup-banner
/// line — so logs from different harness builds are distinguishable when diffing against the rig
/// baseline. Mirrors the cdylib's `build.rs::emit_build_id`.
fn emit_build_id() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git").args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let sha = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "nogit".into());
    let dirty = git(&["status", "--porcelain"]).map(|s| !s.trim().is_empty()).unwrap_or(false);
    let build_id = if dirty { format!("{sha}-dirty") } else { sha };
    println!("cargo:rustc-env=UNSEAMLESS_BUILD_ID={build_id}");
}
