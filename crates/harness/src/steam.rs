//! Rung-4 (Steam lobby discovery) harness prototype — **off-rig, no game, no cdylib**.
//!
//! This is build-order step 2 from [`docs/COOP-CONNECTION.md`] ("Harness prototype"): the `harness`
//! crate is a normal native exe and *can* link `steamworks-rs` (the cdylib can **not** — see that
//! doc's "Steam integration" section), so we prove the password-keyed lobby flow end to end here
//! before the DLL hand-binds the flat C ABI in `coop/steam.rs`.
//!
//! The flow we prove (matches the doc's rung-4 spec):
//! ```text
//!   host:   CreateLobby
//!        -> LobbyCreated -> SetLobbyData("usc_pw", token(password)) + SetLobbyData("usc_ver", ver)
//!   joiner: AddRequestLobbyListStringFilter("usc_pw", token(password))
//!        -> RequestLobbyList -> LobbyMatchList -> GetLobbyByIndex -> JoinLobby -> LobbyEnter
//!        -> read the host SteamID from lobby_owner()
//! ```
//! In the DLL this resolved peer + derived host/client role seeds the rung-2 side-channel (lobby
//! discovery is the only pairing path; there is no manual SteamID entry); the harness just prints
//! what it resolved.
//!
//! Subcommands (driven from `main.rs`, mirroring the tcp-* modes):
//! ```text
//!   steam-init        the GATE: prove steamworks-rs inits on this host against appid 480
//!                     (Spacewar) with a running Steam client. Run this FIRST.
//!   steam-lobby [pw]  single-process end-to-end self-discovery: create -> set data -> filter ->
//!                     list -> find our own lobby -> read owner. Proves the whole discovery scheme
//!                     with ONE Steam account (no second machine needed).
//!   steam-host [pw]   create + advertise a lobby, then idle pumping callbacks so a second machine
//!                     running steam-join can find + join it. Prints joiners.
//!   steam-join [pw]   filter the lobby list by the password token, join the first match, read the
//!                     host SteamID from the owner. The two-machine counterpart.
//! ```
//!
//! `steamworks-rs` is callback-driven: `CreateLobby`/`RequestLobbyList`/`JoinLobby` are async and
//! deliver via call-results that only fire while `client.run_callbacks()` is pumped. We bridge that
//! to straight-line code with [`pump_until`] (pump + poll a channel until a result lands or we time
//! out). The DLL won't do this — it piggybacks the game's own callback pump (the doc's crux) — but
//! for an off-rig exe owning the pump is exactly right.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use steamworks::{Client, DistanceFilter, LobbyId, LobbyType, SteamId};
use steamworks::{LobbyKey, StringFilter, StringFilterKind};

/// Spacewar — Valve's public test appid. The roadmap flagged "does steamworks-rs init on *this*
/// Linux host?" as an unverified assumption; we deliberately use 480, **never** the ELDEN RING
/// appid, for an off-rig prototype (the DLL runs under the real appid because the game already
/// called `SteamAPI_Init`).
const APP_ID: u32 = 480;

/// Lobby-data key carrying the password discovery token. Joiners filter on an exact match of this.
const KEY_PW: &str = "usc_pw";
/// Lobby-data key carrying the mod's protocol version, so a lobby is identifiable as ours and a
/// joiner *could* reject an incompatible major before bothering to join. (We only read it back here;
/// real version-mismatch handling lives in the side-channel handshake, not at discovery time.)
const KEY_VER: &str = "usc_ver";

// ============================================================================================
// The discovery-token scheme — THIS IS THE CONTRACT THE DLL HAND-BIND MUST REUSE VERBATIM.
// ============================================================================================
//
// `token(password)` is what goes into lobby data under `usc_pw`, and what a joiner filters on. Both
// sides must compute it identically or discovery silently finds nothing.
//
//   token = lowercase-hex( SHA-256( DOMAIN || password_utf8 )[0..16] )   // 128-bit, 32 hex chars
//
// Design decisions (don't "simplify" these away in the DLL):
//   * **SHA-256, not the raw password.** Public lobby data is queryable by anyone scraping the lobby
//     list, so we never put the plaintext password there. (The token is a *discovery* filter, not a
//     secret: a low-entropy password is still brute-forceable from a scraped token. The actual
//     session secrecy comes from rung 3 deriving the AES key from the password — see the doc. The
//     DOMAIN separation below guarantees this discovery token is NOT equal to that key material, so
//     leaking the token can't leak the key directly.)
//   * **Domain-separated.** Prefix a fixed, versioned tag so this digest can never collide with any
//     other password-derived value (e.g. the rung-3 key). Bump `v1` -> `v2` only as a deliberate,
//     coordinated wire break (it changes every token, so old and new builds stop finding each other).
//   * **Truncated to 128 bits.** Steam caps a lobby-data value well above this; 32 hex chars is tiny
//     and 128 bits is far more than enough to avoid filter collisions. Keep the *full* hex of the
//     first 16 bytes — don't re-truncate.
//   * **Pure Rust (sha2).** Chosen so the DLL can compute the identical token on `windows-gnu`
//     without a C dep; this module is the reference implementation.
//   * **Raw password bytes, verbatim — no normalization.** Hash the configured password's exact
//     UTF-8 bytes: NO trimming, NO case-folding, NO Unicode NFC. The token is therefore case- and
//     whitespace-sensitive (`token(" pw ") != token("pw") != token("PW")`). The harness reads the
//     password from argv as-is; the DLL must hash the raw TOML value the same way — a stray `.trim()`
//     or `.to_lowercase()` in the config layer would silently break discovery against this reference.
const TOKEN_DOMAIN: &[u8] = b"unseamless-coop/lobby-discovery/v1\0";

/// Compute the lobby discovery token for a password. See the contract block above.
pub fn token(password: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let mut h = Sha256::new();
    h.update(TOKEN_DOMAIN);
    h.update(password.as_bytes());
    let digest = h.finalize();
    let mut out = String::with_capacity(32);
    for b in &digest[..16] {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The protocol version we advertise / expect in `usc_ver`. Canonical, so the harness and the live
/// mod tag lobbies identically.
fn version_tag() -> String {
    unseamless_core::protocol::PROTOCOL_VERSION.to_string()
}

// ============================================================================================
// Callback-pump bridge
// ============================================================================================

/// Pump Steam callbacks until `rx` yields a value or `timeout` elapses. Returns `None` on timeout.
///
/// `steamworks-rs` only delivers async call-results while `run_callbacks()` is being called, so any
/// `create_lobby`/`request_lobby_list`/`join_lobby` must be followed by this. We poll a channel the
/// callback closure sends into.
fn pump_until<T>(client: &Client, rx: &Receiver<T>, timeout: Duration) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        client.run_callbacks();
        match rx.try_recv() {
            Ok(v) => return Some(v),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Init Steam against appid 480. Fatal (process exit) on failure — every subcommand needs it, and a
/// failure here is the one assumption the roadmap flagged, so surface it loudly.
fn init_or_die() -> Client {
    match Client::init_app(APP_ID) {
        Ok(client) => {
            let me = client.user().steam_id();
            println!("[steam] init OK against appid {APP_ID} (Spacewar)");
            println!("[steam] our SteamID (raw u64): {}", me.raw());
            client
        }
        Err(e) => {
            eprintln!("[steam] FAILED to init steamworks against appid {APP_ID}: {e}");
            eprintln!("[steam] is the Steam client running and logged in? (off-rig prototype gate)");
            std::process::exit(1);
        }
    }
}

// ============================================================================================
// Subcommand: steam-init (the gate)
// ============================================================================================

/// The gate: prove `steamworks-rs` inits on this host. Inits, prints our SteamID, pumps a few
/// callbacks to show the loop is alive, exits 0. If this exits non-zero, the whole rung-4 path is
/// blocked on this host and we stop and report rather than guess.
pub fn run_init() {
    let client = init_or_die();
    for _ in 0..10 {
        client.run_callbacks();
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("[steam] callback pump alive; init gate PASSED");
}

// ============================================================================================
// Subcommand: steam-lobby (single-process end-to-end self-discovery)
// ============================================================================================

/// Create + advertise a lobby, then (in the same process / same Steam account) filter the lobby list
/// by the password token and confirm we find our own lobby and can read its owner. This proves the
/// entire discovery scheme without a second Steam account or machine — the off-rig deliverable.
pub fn run_lobby(password: &str) {
    let client = init_or_die();
    let tok = token(password);
    println!("[lobby] password token (usc_pw) = {tok}");
    println!("[lobby] version tag  (usc_ver)  = {}", version_tag());

    let lobby = create_advertised_lobby(&client, &tok);
    println!("[lobby] created + advertised lobby {} (owner {})", lobby.raw(), client.matchmaking().lobby_owner(lobby).raw());

    // Now act as a joiner against our own advertisement: filter by the token and list. A newly
    // created lobby's data isn't indexed by Valve's matchmaking backend instantly, so the first list
    // can come back empty — retry a few times before giving up (the DLL joiner will want the same
    // tolerance).
    let found = find_lobby_by_token_retry(&client, &tok, 6);
    match found {
        Some(id) if id == lobby => {
            let owner = client.matchmaking().lobby_owner(id);
            let ver = client.matchmaking().lobby_data(id, KEY_VER).unwrap_or_default();
            println!("[lobby] discovery MATCH: found our lobby {} via the password filter", id.raw());
            println!("[lobby] host SteamID from owner (raw u64) = {}", owner.raw());
            println!("[lobby] usc_ver read back = {ver}");
            // Also exercise the JoinLobby call-result path. We already own/occupy this lobby, so this
            // is a smoke of the call, not a true cross-account join (that needs a second account —
            // use `steam-host`/`steam-join` across two machines). Soft: a quirk here isn't a failure
            // of the discovery scheme we're proving.
            let (tx, rx) = channel();
            client.matchmaking().join_lobby(id, move |res| {
                let _ = tx.send(res);
            });
            match pump_until(&client, &rx, Duration::from_secs(10)) {
                Some(Ok(j)) => println!("[lobby] JoinLobby OK (LobbyEnter for {})", j.raw()),
                Some(Err(())) => println!("[lobby] JoinLobby on our own lobby returned err (benign for a self-join)"),
                None => println!("[lobby] JoinLobby self-smoke timed out (benign; cross-account join is the real test)"),
            }
            println!("[lobby] -> end-to-end discovery scheme PROVEN off-rig");
        }
        Some(id) => {
            // Another modded lobby with the same password exists; still proves the filter works.
            println!("[lobby] discovery returned a DIFFERENT lobby {} (someone else's, same password)", id.raw());
            println!("[lobby] filter works; couldn't self-match (a real second peer is advertising)");
        }
        None => {
            eprintln!("[lobby] FAILED: filtered list did not return our own lobby");
            eprintln!("[lobby] (lobby data can lag a beat after CreateLobby; this is the thing to watch)");
            leave_and_settle(&client, lobby);
            std::process::exit(1);
        }
    }
    leave_and_settle(&client, lobby);
}

// ============================================================================================
// Subcommand: steam-host (advertise, then idle for a real second-machine joiner)
// ============================================================================================

/// Create + advertise a lobby and idle, pumping callbacks, so a `steam-join` on another machine can
/// discover and join it. Prints the member roster whenever it changes.
pub fn run_host(password: &str) {
    let client = init_or_die();
    let tok = token(password);
    let lobby = create_advertised_lobby(&client, &tok);
    let me = client.user().steam_id();
    println!("[host] advertising lobby {} as {} (token {tok})", lobby.raw(), me.raw());
    println!("[host] waiting for a joiner — run `steam-join {password}` on another machine. Ctrl-C to stop.");

    let mut last_count = 0usize;
    loop {
        client.run_callbacks();
        let members = client.matchmaking().lobby_members(lobby);
        if members.len() != last_count {
            last_count = members.len();
            println!("[host] roster ({} member(s)):", members.len());
            for m in &members {
                let tag = if *m == me { " (host/us)" } else { " (joiner)" };
                println!("[host]   {}{tag}", m.raw());
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ============================================================================================
// Subcommand: steam-join (filter, join, read host SteamID)
// ============================================================================================

/// Filter the lobby list by the password token, join the first match, and read the host SteamID from
/// the owner — the value that would seed the rung-2 side-channel in the DLL.
pub fn run_join(password: &str) {
    let client = init_or_die();
    let tok = token(password);
    println!("[join] searching for a lobby with token {tok} …");

    let Some(id) = find_lobby_by_token_retry(&client, &tok, 6) else {
        eprintln!("[join] FAILED: no lobby found for that password (is a host advertising it?)");
        std::process::exit(1);
    };
    println!("[join] found lobby {}; joining …", id.raw());

    let (tx, rx) = channel();
    client.matchmaking().join_lobby(id, move |res| {
        let _ = tx.send(res);
    });
    match pump_until(&client, &rx, Duration::from_secs(15)) {
        Some(Ok(joined)) => {
            let owner = client.matchmaking().lobby_owner(joined);
            let ver = client.matchmaking().lobby_data(joined, KEY_VER).unwrap_or_default();
            println!("[join] joined lobby {}", joined.raw());
            println!("[join] host SteamID from owner (raw u64) = {}", owner.raw());
            println!("[join] lobby usc_ver = {ver} (ours = {})", version_tag());
            println!("[join] -> this SteamID + is_host=false is what seeds the rung-2 side-channel");
            leave_and_settle(&client, joined);
        }
        Some(Err(())) => {
            eprintln!("[join] FAILED: JoinLobby returned an error");
            std::process::exit(1);
        }
        None => {
            eprintln!("[join] FAILED: JoinLobby timed out");
            std::process::exit(1);
        }
    }
}

// ============================================================================================
// Shared building blocks
// ============================================================================================

/// Create a lobby and advertise it with the password token + version tag. Fatal on failure.
fn create_advertised_lobby(client: &Client, tok: &str) -> LobbyId {
    let (tx, rx) = channel();
    // A small max_members; `LobbyType::Public` so RequestLobbyList can find it. (The DLL may prefer
    // Invisible + friends-only depending on the auth/NAT answer; for the prototype Public is what
    // makes the list-filter path observable.)
    client.matchmaking().create_lobby(LobbyType::Public, 4, move |res| {
        let _ = tx.send(res);
    });
    let lobby = match pump_until(client, &rx, Duration::from_secs(15)) {
        Some(Ok(id)) => id,
        Some(Err(e)) => {
            eprintln!("[steam] CreateLobby failed: {e}");
            std::process::exit(1);
        }
        None => {
            eprintln!("[steam] CreateLobby timed out (no LobbyCreated call-result)");
            std::process::exit(1);
        }
    };
    let mm = client.matchmaking();
    if !mm.set_lobby_data(lobby, KEY_PW, tok) {
        eprintln!("[steam] WARNING: SetLobbyData({KEY_PW}) returned false");
    }
    if !mm.set_lobby_data(lobby, KEY_VER, &version_tag()) {
        eprintln!("[steam] WARNING: SetLobbyData({KEY_VER}) returned false");
    }
    lobby
}

/// Like [`find_lobby_by_token`] but retries up to `attempts` times with a short settle delay, to ride
/// out the propagation lag between advertising a lobby's data and the matchmaking backend indexing it.
fn find_lobby_by_token_retry(client: &Client, tok: &str, attempts: u32) -> Option<LobbyId> {
    for attempt in 1..=attempts {
        if let Some(id) = find_lobby_by_token(client, tok) {
            return Some(id);
        }
        if attempt < attempts {
            println!("[steam] no match yet (attempt {attempt}/{attempts}); waiting for lobby data to propagate…");
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    None
}

/// Apply the password-token string filter and request the lobby list, returning the first match.
/// Returns `None` if the list comes back empty (no lobby advertising that token).
fn find_lobby_by_token(client: &Client, tok: &str) -> Option<LobbyId> {
    let mm = client.matchmaking();
    // Search worldwide, not just our own Steam region: the two-machine steam-host/steam-join path
    // can otherwise come back empty if the peers resolve to different regions (the single-process
    // steam-lobby test is region-local and wouldn't notice). Harmless for the self-test.
    mm.set_request_lobby_list_distance_filter(DistanceFilter::Worldwide);
    // Exact-match filter on the password token. Must be applied immediately before each
    // request_lobby_list — filters aren't sticky across requests.
    mm.add_request_lobby_list_string_filter(StringFilter(
        LobbyKey::new(KEY_PW),
        tok,
        StringFilterKind::Equal,
    ));
    let (tx, rx) = channel();
    mm.request_lobby_list(move |res| {
        let _ = tx.send(res);
    });
    match pump_until(client, &rx, Duration::from_secs(15)) {
        Some(Ok(list)) => {
            println!("[steam] RequestLobbyList -> {} match(es) for the token", list.len());
            list.into_iter().next() // GetLobbyByIndex(0) — the SDK already returns indexed ids
        }
        Some(Err(e)) => {
            eprintln!("[steam] RequestLobbyList failed: {e}");
            None
        }
        None => {
            eprintln!("[steam] RequestLobbyList timed out");
            None
        }
    }
}

/// Leave a lobby and pump a few callbacks so the LeaveLobby actually reaches Steam before the client
/// is dropped / the process exits — makes teardown deterministic rather than racing the drop. (Steam
/// also auto-reaps a lobby once its last member disconnects, so this is belt-and-suspenders.)
fn leave_and_settle(client: &Client, lobby: LobbyId) {
    client.matchmaking().leave_lobby(lobby);
    for _ in 0..5 {
        client.run_callbacks();
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Convenience for `main.rs`: pick a default-ish password when the operator doesn't pass one, so the
/// quick `steam-lobby` smoke test needs no args. A real run should pass a shared password.
pub fn password_or_default(arg: Option<String>) -> String {
    arg.unwrap_or_else(|| "harness-default-password".to_string())
}

#[allow(dead_code)]
fn _assert_steamid_is_u64(_: SteamId) {} // doc: SteamId::raw() is the u64 PeerId the side-channel uses

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_deterministic_and_not_plaintext() {
        let a = token("hunter2");
        let b = token("hunter2");
        assert_eq!(a, b, "token must be deterministic across peers");
        assert_eq!(a.len(), 32, "128-bit token => 32 lowercase-hex chars");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "hex only");
        assert_ne!(a, "hunter2", "must never be the plaintext password");
        assert!(!a.contains("hunter2"));
    }

    #[test]
    fn distinct_passwords_give_distinct_tokens() {
        assert_ne!(token("alpha"), token("beta"));
    }

    #[test]
    fn token_matches_known_vector() {
        // Pin the scheme so a DLL reimplementation can assert byte-for-byte parity against this.
        // SHA-256("unseamless-coop/lobby-discovery/v1\0" + "swordfish")[0..16], lowercase hex.
        assert_eq!(token("swordfish"), KNOWN_VECTOR_SWORDFISH);
    }
}

#[cfg(test)]
/// Pinned reference value for `token("swordfish")`. Filled in from the first green run (see test).
const KNOWN_VECTOR_SWORDFISH: &str = "e1ae25ea4eab35799470c31622b014b8";
