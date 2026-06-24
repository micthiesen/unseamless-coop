//! Pure policy for loading other DLL mods.
//!
//! Our shipped artifact is the game's `dinput8.dll` (a proxy the game auto-loads), which makes
//! this mod the **parent loader**: besides itself, it loads other simple DLL mods dropped in
//! `mods/`, the way Elden Mod Loader does — so a user runs our co-op mod *and* their other mods
//! from one install. The cdylib does the filesystem walk + `LoadLibrary`; the **ordering policy**
//! lives here so it's host-tested (load order matters: some mods must hook before others).

/// Decide the order to load `discovered` mod filenames in.
///
/// Names listed in `configured` load **first, in that order** (so a user can pin "load X before
/// Y"), then everything else loads in alphabetical order for a stable default. Matching is
/// **ASCII-case-insensitive** (enough for Windows filenames in practice; a non-ASCII name configured
/// in a different case just falls to the alphabetical rest, never dropped), a configured name not
/// actually present is skipped, and duplicates (in either list) collapse to one load. The returned
/// names are the actual `discovered` spellings, so the caller can use them as-is for the path.
pub fn mod_load_order(discovered: &[String], configured: &[String]) -> Vec<String> {
    let key = |s: &str| s.to_ascii_lowercase();

    let mut ordered = Vec::with_capacity(discovered.len());
    let mut taken = std::collections::BTreeSet::new();

    // Configured names first, in their given order, matched case-insensitively against what's
    // actually present.
    for want in configured {
        let want_key = key(want);
        if taken.contains(&want_key) {
            continue;
        }
        if let Some(actual) = discovered.iter().find(|d| key(d) == want_key) {
            ordered.push(actual.clone());
            taken.insert(want_key);
        }
    }

    // Then the remaining discovered mods, alphabetical (case-insensitive) for a stable order. The
    // `taken.insert` guard alone skips already-placed configured names and collapses duplicates, so
    // no pre-filter is needed.
    let mut rest: Vec<&String> = discovered.iter().collect();
    rest.sort_by_key(|d| key(d));
    for d in rest {
        if taken.insert(key(d)) {
            ordered.push(d.clone());
        }
    }

    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn configured_first_then_alphabetical_rest() {
        let discovered = v(&["zebra.dll", "alpha.dll", "middle.dll"]);
        let configured = v(&["middle.dll"]);
        assert_eq!(mod_load_order(&discovered, &configured), v(&["middle.dll", "alpha.dll", "zebra.dll"]));
    }

    #[test]
    fn empty_config_is_pure_alphabetical() {
        let discovered = v(&["b.dll", "C.dll", "a.dll"]);
        assert_eq!(mod_load_order(&discovered, &[]), v(&["a.dll", "b.dll", "C.dll"]));
    }

    #[test]
    fn configured_name_not_present_is_skipped() {
        let discovered = v(&["a.dll"]);
        let configured = v(&["ghost.dll", "a.dll"]);
        assert_eq!(mod_load_order(&discovered, &configured), v(&["a.dll"]));
    }

    #[test]
    fn matching_is_case_insensitive_and_keeps_discovered_spelling() {
        let discovered = v(&["MyMod.DLL"]);
        let configured = v(&["mymod.dll"]);
        // Matched case-insensitively, but the actual on-disk spelling is returned for the path.
        assert_eq!(mod_load_order(&discovered, &configured), v(&["MyMod.DLL"]));
    }

    #[test]
    fn duplicates_collapse_in_both_lists() {
        let discovered = v(&["a.dll", "b.dll"]);
        let configured = v(&["a.dll", "a.dll", "b.dll"]);
        assert_eq!(mod_load_order(&discovered, &configured), v(&["a.dll", "b.dll"]));
    }

    #[test]
    fn case_variant_configured_entries_collapse() {
        // Two configured entries differing only in case map to the same on-disk mod: load it once
        // (exercises the case-insensitive `taken` guard on the configured pass, not just the rest).
        let discovered = v(&["a.dll"]);
        let configured = v(&["A.dll", "a.DLL"]);
        assert_eq!(mod_load_order(&discovered, &configured), v(&["a.dll"]));
    }

    #[test]
    fn every_discovered_mod_loads_exactly_once() {
        let discovered = v(&["a.dll", "b.dll", "c.dll", "d.dll"]);
        let configured = v(&["c.dll", "a.dll"]);
        let order = mod_load_order(&discovered, &configured);
        assert_eq!(order.len(), discovered.len());
        let mut sorted = order.clone();
        sorted.sort();
        let mut want = discovered.clone();
        want.sort();
        assert_eq!(sorted, want, "no mod dropped or duplicated");
        assert_eq!(&order[..2], &v(&["c.dll", "a.dll"])[..], "configured order honored");
    }
}
