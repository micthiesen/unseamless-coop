//! Co-op save-file isolation: decide whether a file path is a vanilla Elden Ring save and, if so,
//! what the co-op path should be. Pure and host-tested — the cdylib hooks `CreateFileW` and asks
//! this module per open, so the game's saves land in `ER0000.<ext>` (default `co2`) instead of the
//! vanilla `ER0000.sl2`, fully isolated from a player's single-player progress. See
//! [`docs/COOP-SAVES.md`](../../../docs/COOP-SAVES.md).
//!
//! The only thing that distinguishes a vanilla save from a co-op save is the extension — same
//! directory, same `ER0000` stem, same internal format — so the whole feature is a trailing-suffix
//! swap on the path string. We never touch the directory or stem.

/// The vanilla active-save suffix and its `.bak` backup. Matched longest-first: a path ending in
/// `".sl2.bak"` does *not* end in `".sl2"` (it ends in `".bak"`), so both must be listed explicitly,
/// and longest-first is the robust habit regardless.
const VANILLA_SUFFIXES: [&str; 2] = [".sl2.bak", ".sl2"];

/// Whether co-op save isolation is active for the configured extension `ext`. False when isolation
/// would be a no-op or wrong: an empty extension, or `sl2` itself (case-insensitive; surrounding
/// whitespace and dots ignored) — i.e. the user opted back into sharing the vanilla save. The cdylib
/// skips installing the `CreateFileW` hook entirely when this is false, so vanilla saves are untouched.
pub fn isolates_saves(ext: &str) -> bool {
    let ext = ext.trim().trim_matches('.');
    !ext.is_empty() && !ext.eq_ignore_ascii_case("sl2")
}

/// Allocation-free pre-filter for the cdylib's `CreateFileW` hot path: does the (NUL-less) wide path
/// `wide` end with a vanilla save suffix (ASCII, case-insensitive)? This single-sources the suffix set
/// with [`coop_save_path`] through [`VANILLA_SUFFIXES`], so the binding's fast-reject provably can't be
/// narrower than what the rewrite matches — a drift that would silently leak a real save to the vanilla
/// file. The cdylib only converts to a `String` and calls [`coop_save_path`] when this returns true.
pub fn wide_has_vanilla_suffix(wide: &[u16]) -> bool {
    VANILLA_SUFFIXES.iter().any(|suffix| wide_ends_with_ci(wide, suffix))
}

/// [`str::ends_with`] for a UTF-16 slice against an ASCII `suffix`, case-insensitive. A wide unit is
/// compared as a byte only when it's ASCII (`< 0x80`), so a non-ASCII unit can never truncate into a
/// false match. `suffix` must be ASCII (all our save suffixes are).
fn wide_ends_with_ci(wide: &[u16], suffix: &str) -> bool {
    debug_assert!(suffix.is_ascii(), "wide_ends_with_ci requires an ASCII suffix");
    let s = suffix.as_bytes();
    let Some(start) = wide.len().checked_sub(s.len()) else {
        return false;
    };
    wide[start..].iter().zip(s).all(|(&w, &b)| w < 0x80 && (w as u8).eq_ignore_ascii_case(&b))
}

/// If `path` names a vanilla save file (`*.sl2` or `*.sl2.bak`, case-insensitive), return the co-op
/// path with the vanilla suffix swapped to `ext` (`*.<ext>` / `*.<ext>.bak`). Otherwise `None` — the
/// caller passes the path through to the real `CreateFileW` untouched.
///
/// `ext` is the bare, already-validated extension (config guarantees 1..=120 ASCII alphanumerics).
/// Only the trailing vanilla suffix is replaced, so the directory and `ER0000` stem are preserved
/// byte-for-byte; the `.bak` tail is kept after the new extension so the game's backup write still
/// lands on its matching co-op backup.
pub fn coop_save_path(path: &str, ext: &str) -> Option<String> {
    for suffix in VANILLA_SUFFIXES {
        if let Some(stem) = strip_suffix_ci(path, suffix) {
            let bak = if suffix.ends_with(".bak") { ".bak" } else { "" };
            return Some(format!("{stem}.{ext}{bak}"));
        }
    }
    None
}

/// [`str::strip_suffix`] but ASCII-case-insensitive — Windows file extensions are case-insensitive,
/// and while Elden Ring writes lowercase `.sl2` we don't want a `.SL2` to slip past into the vanilla
/// save. Returns `None` (rather than panicking) if the split would land inside a multi-byte char,
/// which also can't be an ASCII-suffix match anyway.
fn strip_suffix_ci<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    let split = s.len().checked_sub(suffix.len())?;
    if !s.is_char_boundary(split) {
        return None;
    }
    let (head, tail) = s.split_at(split);
    tail.eq_ignore_ascii_case(suffix).then_some(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_active_save_and_backup() {
        let dir = r"C:\Users\me\AppData\Roaming\EldenRing\76561190000000000\";
        assert_eq!(
            coop_save_path(&format!("{dir}ER0000.sl2"), "co2"),
            Some(format!("{dir}ER0000.co2"))
        );
        assert_eq!(
            coop_save_path(&format!("{dir}ER0000.sl2.bak"), "co2"),
            Some(format!("{dir}ER0000.co2.bak"))
        );
    }

    #[test]
    fn proton_prefix_path_is_rewritten() {
        // The rig path: the same tail inside the Wine prefix. Literal oracle (not a `.replace`), so a
        // bug that wrongly rewrote a mid-path `.sl2` would be caught rather than mirrored.
        let dir = "/home/u/.steam/x/compatdata/1245620/pfx/drive_c/users/steamuser/AppData/Roaming/EldenRing/7656/";
        assert_eq!(
            coop_save_path(&format!("{dir}ER0000.sl2"), "co2"),
            Some(format!("{dir}ER0000.co2"))
        );
    }

    #[test]
    fn handles_extended_length_and_bare_suffix_paths() {
        // A \\?\ extended-length path still rewrites (suffix-only match, prefix preserved verbatim).
        assert_eq!(
            coop_save_path(r"\\?\C:\EldenRing\7656\ER0000.sl2", "co2"),
            Some(r"\\?\C:\EldenRing\7656\ER0000.co2".into())
        );
        // A path that IS exactly the suffix (split lands at index 0).
        assert_eq!(coop_save_path(".sl2", "co2"), Some(".co2".into()));
        assert_eq!(coop_save_path(".sl2.bak", "co2"), Some(".co2.bak".into()));
    }

    #[test]
    fn custom_extension_composes_with_the_backup_tail() {
        assert_eq!(coop_save_path(r"X\ER0000.sl2.bak", "coop"), Some(r"X\ER0000.coop.bak".into()));
    }

    #[test]
    fn trailing_separator_is_not_a_save() {
        // A directory-looking path ending in a separator must not match (it's not a file open).
        assert_eq!(coop_save_path(r"X\ER0000.sl2\", "co2"), None);
        assert_eq!(coop_save_path("X/ER0000.sl2/", "co2"), None);
    }

    #[test]
    fn wide_pre_filter_matches_vanilla_suffixes_case_insensitively() {
        let w = |s: &str| -> Vec<u16> { s.encode_utf16().collect() };
        assert!(wide_has_vanilla_suffix(&w(r"X\ER0000.sl2")));
        assert!(wide_has_vanilla_suffix(&w(r"X\ER0000.SL2")));
        assert!(wide_has_vanilla_suffix(&w(r"X\ER0000.sl2.bak")));
        assert!(wide_has_vanilla_suffix(&w(r"X\ER0000.Sl2.Bak")));
        assert!(!wide_has_vanilla_suffix(&w(r"X\foo.txt")));
        assert!(!wide_has_vanilla_suffix(&w("ab"))); // shorter than any suffix
    }

    #[test]
    fn wide_pre_filter_rejects_non_ascii_unit_that_byte_truncates_to_a_match() {
        // 0x0132 is >= 0x80 but its low byte is 0x32 == '2'. A truncating compare would wrongly match
        // ".sl2"; the `w < 0x80` guard must reject it. This pins that guard (the prior ASCII-only
        // tests pass even if the guard is deleted).
        let mut wide: Vec<u16> = "X.sl".encode_utf16().collect();
        wide.push(0x0132);
        assert!(!wide_has_vanilla_suffix(&wide));
    }

    #[test]
    fn extension_is_case_insensitive() {
        assert_eq!(coop_save_path(r"X\ER0000.SL2", "co2"), Some(r"X\ER0000.co2".into()));
        assert_eq!(coop_save_path(r"X\ER0000.Sl2.Bak", "co2"), Some(r"X\ER0000.co2.bak".into()));
    }

    #[test]
    fn honors_a_custom_extension() {
        assert_eq!(coop_save_path(r"X\ER0000.sl2", "coop"), Some(r"X\ER0000.coop".into()));
    }

    #[test]
    fn leaves_non_saves_untouched() {
        // Already a co-op save, unrelated files, and the substring-not-suffix trap.
        assert_eq!(coop_save_path(r"X\ER0000.co2", "co2"), None);
        assert_eq!(coop_save_path(r"X\steam_api64.dll", "co2"), None);
        assert_eq!(coop_save_path(r"X\config.sl2x", "co2"), None);
        assert_eq!(coop_save_path(r"X\a.sl2\b.txt", "co2"), None); // ".sl2" mid-path, not a suffix
        assert_eq!(coop_save_path("", "co2"), None);
    }

    #[test]
    fn preserves_a_non_ascii_directory() {
        // A non-ASCII char right before the suffix must not panic the boundary split, and the prefix
        // is kept verbatim.
        assert_eq!(coop_save_path("C:\\Üsér\\ER0000.sl2", "co2"), Some("C:\\Üsér\\ER0000.co2".into()));
        assert_eq!(coop_save_path("naïve.sl2", "co2"), Some("naïve.co2".into()));
    }

    #[test]
    fn isolation_active_unless_empty_or_sl2() {
        assert!(isolates_saves("co2"));
        assert!(isolates_saves(".co2"));
        assert!(isolates_saves("coop"));
        assert!(!isolates_saves(""));
        assert!(!isolates_saves("   "));
        assert!(!isolates_saves("sl2"));
        assert!(!isolates_saves("SL2"));
        assert!(!isolates_saves(".sl2"));
        assert!(!isolates_saves(".SL2")); // dot-trim and case-fold both apply
    }
}
