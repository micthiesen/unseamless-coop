//! Parent-loader: load other simple DLL mods from `mods/`.
//!
//! Because our shipped artifact is the game's `dinput8.dll`, this mod is the entry point — so it
//! also takes on what Elden Mod Loader does: scan a `mods/` folder next to the game exe and
//! `LoadLibrary` each DLL, letting a user run our co-op mod *and* their other mods from one install.
//! The **ordering policy** is host-tested in `unseamless_core::loader`; this is just the Windows
//! filesystem + `LoadLibrary` glue, so it's rig-validated.

use std::ffi::{OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use unseamless_core::config::Config;
use unseamless_core::loader::mod_load_order;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, LoadLibraryW};
use windows::core::PCWSTR;

/// The directory our own DLL lives in (the game folder), from the module handle stashed in
/// `DllMain`. `mods/` and the other game files sit alongside it.
pub fn self_dir(module: usize) -> Option<PathBuf> {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(Some(HMODULE(module as *mut c_void)), &mut buf) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    let path = PathBuf::from(OsString::from_wide(&buf[..n]));
    path.parent().map(Path::to_path_buf)
}

/// Discover and load the other DLL mods in `<self_dir>/mods/`, in the configured order. Logs each
/// outcome. No-op when loading is disabled or the folder is empty/absent. A mod whose `LoadLibrary`
/// *returns* failure is logged and skipped so the rest still load — but note we can't contain a mod
/// that crashes or calls `ExitProcess` from its own `DllMain` (that runs synchronously inside
/// `LoadLibrary` and would take the process down regardless).
pub fn load_mods(config: &Config, self_dir: &Path) {
    if !config.loader.enabled {
        log::info!("extra mod loading disabled ([loader] enabled = false)");
        return;
    }
    let mods_dir = self_dir.join("mods");
    let discovered = discover_dlls(&mods_dir);
    if discovered.is_empty() {
        log::info!("no extra mods found in {}", mods_dir.display());
        return;
    }

    let order = mod_load_order(&discovered, &config.loader.order);
    log::info!("loading {} extra mod(s) from {}", order.len(), mods_dir.display());
    for name in order {
        let path = mods_dir.join(&name);
        if load_one(&path) {
            log::info!("loaded mod: {name}");
        } else {
            log::error!("failed to load mod: {name}");
        }
    }
}

/// Filenames of `*.dll` regular files in `dir` (non-recursive). Non-`.dll` files (e.g. the bundle's
/// `mods/README.txt`) and subdirectories are ignored. Empty if the dir can't be read.
fn discover_dlls(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_str()?;
            if path.is_file() && name.to_ascii_lowercase().ends_with(".dll") {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn load_one(path: &Path) -> bool {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())).is_ok() }
}
