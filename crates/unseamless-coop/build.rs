//! Statically link the mingw C++/GCC runtime, so the cross-compiled cdylib is self-contained — no
//! `libstdc++-6.dll` dependency that the game's Wine prefix lacks (LoadLibrary would fail). hudhook's
//! imgui-sys is C++ and is always compiled in, so this is unconditional. Requires `CXXSTDLIB=""` (set
//! in `.cargo/config.toml`) so imgui-sys doesn't emit a competing *dynamic* `stdc++` link that rustc
//! would force `-Bdynamic`, defeating a static one.

fn main() {
    // This recipe depends only on this file (and the mingw toolchain, which Cargo can't track);
    // pin the rerun to it so an unrelated source change doesn't needlessly relink.
    println!("cargo:rerun-if-changed=build.rs");

    emit_build_id();

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
        // mingw runtime: defines `__mingwthr_key_dtor` (the TLS-key destructor), which libgcc's
        // *win32*-threads `gthr-win32.o` references. Local (posix-threads) mingw never pulls it, but
        // CI/Debian default to win32 threads — without this the static link fails there. Inside the
        // group because libgcc ↔ libmingw32 reference each other. Harmless under posix threads.
        "-lmingw32",
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

/// Bake a short build id — `<short-sha>` or `<short-sha>-dirty` — into the cdylib as the
/// `UNSEAMLESS_BUILD_ID` compile-time env (read via `env!` in `logger.rs` and `overlay.rs`). It
/// lands in every log header (always) and the overlay title/watermark (debug builds), so a
/// friend's log or screenshot names exactly which build they're running — the first thing to check
/// when "it didn't install right". This is *source* identity; the exact-bytes check is the
/// package-time sha256 in `scripts/rig.sh package`'s MANIFEST.
///
/// Re-derivation: `git rev-parse --short=7 HEAD` for the sha, `git status --porcelain` (non-empty
/// ⇒ uncommitted changes) for `-dirty`. Falls back to `nogit` when git/the repo is unavailable
/// (e.g. a source tarball build), so `env!` always resolves.
fn emit_build_id() {
    // Re-run when the checked-out commit moves. Unstaged edits don't touch these, so the dirty flag
    // can lag a plain rebuild; `rig.sh package` `touch`es build.rs to force a fresh stamp at package
    // time, which is the only build whose id we hand to someone else.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");

    let sha = run_git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "nogit".into());
    let dirty = run_git(&["status", "--porcelain"]).map(|s| !s.trim().is_empty()).unwrap_or(false);
    let build_id = if dirty { format!("{sha}-dirty") } else { sha };
    println!("cargo:rustc-env=UNSEAMLESS_BUILD_ID={build_id}");
}

/// Run a git subcommand, returning trimmed stdout on success, `None` on any failure (git missing,
/// not a repo, non-zero exit) so callers degrade to a sentinel instead of failing the build.
fn run_git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}
