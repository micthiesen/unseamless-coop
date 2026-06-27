//! Two-peer side-channel harness — the fast test loop that needs no game and no Steam.
//!
//! It wires a host + client (both running the real `unseamless_core::peer::Peer` coordination
//! logic) over a transport and runs scenarios, printing the result so an operator (or an assistant
//! driving this loop) can read what happened. This exercises the side channel end to end: the
//! version handshake, host->client config sync, action authorization, client->host log forwarding,
//! and convergence over a lossy/unordered channel. It does NOT exercise the game's own player/world
//! sync (that needs the game; see docs/RIG-RUNBOOK.md and the /test-loop skill).
//!
//! Two backends:
//!   * **in-memory** (`Loopback`) — the default scenarios, single process, instant.
//!   * **TCP** (`tcp-listen` / `tcp-connect`) — two real processes over localhost, real sockets and
//!     serialization (also the host half of the planned layer-3 debug bridge).
//!
//! Run on the host (the workspace default target is windows-gnu, which can't execute here):
//!   scripts/harness.sh [scenario]      # handshake | version-mismatch | config-sync |
//!                                      # session-action | log-forward | lossy | all (default)
//!   scripts/harness-tcp.sh             # spawns tcp-listen + tcp-connect over localhost

mod steam;
mod tcp;

use std::net::TcpListener;
use std::time::Duration;

use unseamless_core::config::Config;
use unseamless_core::diagnostics::LogLevel;
use unseamless_core::peer::{Peer, Session};
use unseamless_core::protocol::{AUTH_NONCE_LEN, AuthNonce, SessionAction};
use unseamless_core::transport::{FaultModel, Loopback, PeerId};
use unseamless_core::util::Version;

use crate::tcp::TcpTransport;

const HOST: PeerId = 1;
const CLIENT: PeerId = 2;
// Fixed, distinct per-peer handshake nonces. The harness is a single-shot driver, so replay-freshness
// (which a real session gets from the CSPRNG) doesn't matter here — only that the two peers use
// distinct nonces and matching passwords, so the auth proof links.
const HOST_NONCE: AuthNonce = [0x11; AUTH_NONCE_LEN];
const CLIENT_NONCE: AuthNonce = [0x22; AUTH_NONCE_LEN];
/// The mod wire version both peers run — the canonical one, so the harness and the live mod's bridge
/// can't drift apart and spuriously fail the handshake's version-compat check.
const V: Version = unseamless_core::protocol::PROTOCOL_VERSION;

fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "all".into());

    // Steam lobby-discovery prototype (rung 4) — off-rig, links steamworks-rs (the cdylib can't).
    // `steam-init` is the gate (prove init on this host); the others take an optional password.
    if which.starts_with("steam-") {
        match which.as_str() {
            "steam-init" => steam::run_init(),
            "steam-lobby" => steam::run_lobby(&steam::password_or_default(std::env::args().nth(2))),
            "steam-host" => steam::run_host(&steam::password_or_default(std::env::args().nth(2))),
            "steam-join" => steam::run_join(&steam::password_or_default(std::env::args().nth(2))),
            other => {
                eprintln!("unknown steam mode '{other}'. options: steam-init, steam-lobby, steam-host, steam-join");
                std::process::exit(2);
            }
        }
        return;
    }

    // TCP modes take a port and run as one end of a two-process exchange. `bridge-host` points the
    // host end at a live mod's debug bridge (layer 3): the mod is the client, we push it config.
    if which == "tcp-listen" || which == "tcp-connect" || which == "bridge-host" {
        let port = std::env::args().nth(2).unwrap_or_else(|| "47620".into());
        let addr = format!("127.0.0.1:{port}");
        match which.as_str() {
            "tcp-listen" => run_tcp_host(&addr),
            "tcp-connect" => run_tcp_client(&addr),
            _ => run_bridge_host(&addr),
        }
        return;
    }

    let scenarios: &[(&str, fn())] = &[
        ("handshake", scenario_handshake),
        ("version-mismatch", scenario_version_mismatch),
        ("config-sync", scenario_config_sync),
        ("session-action", scenario_session_action),
        ("log-forward", scenario_log_forward),
        ("lossy", scenario_lossy),
    ];

    if which == "all" {
        for (_, run) in scenarios {
            run();
            println!();
        }
        return;
    }
    match scenarios.iter().find(|(name, _)| *name == which) {
        Some((_, run)) => run(),
        None => {
            eprintln!(
                "unknown scenario '{which}'. options: {}, all, tcp-listen, tcp-connect, bridge-host",
                scenarios.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
            );
            std::process::exit(2);
        }
    }
}

// --- harness plumbing -------------------------------------------------------------------------

/// Build a host + client pair over a given set of loopback endpoints, with the given versions and
/// configs. Take `mesh` or `mesh_with_faults` endpoints to choose a perfect or lossy channel.
fn pair_on(
    ends: Vec<Loopback>,
    host_v: Version,
    client_v: Version,
    host_cfg: Config,
    client_cfg: Config,
) -> (Session<Loopback>, Session<Loopback>) {
    let mut it = ends.into_iter();
    let host = Session::new(Peer::new(HOST, HOST, host_v, host_cfg, HOST_NONCE), it.next().unwrap());
    let client = Session::new(Peer::new(CLIENT, HOST, client_v, client_cfg, CLIENT_NONCE), it.next().unwrap());
    (host, client)
}

/// Build a pair over a fresh perfect loopback.
fn pair(
    host_v: Version,
    client_v: Version,
    host_cfg: Config,
    client_cfg: Config,
) -> (Session<Loopback>, Session<Loopback>) {
    pair_on(Loopback::mesh(&[HOST, CLIENT]), host_v, client_v, host_cfg, client_cfg)
}

/// Step both sessions until no frames remain in flight (perfect channel only).
fn converge(host: &mut Session<Loopback>, client: &mut Session<Loopback>) {
    for _ in 0..100 {
        if host.pump() + client.pump() == 0 {
            return;
        }
    }
    panic!("session did not converge");
}

fn header(title: &str) {
    println!("=== {title} ===");
}

fn show_notifications(label: &str, session: &Session<Loopback>) {
    let n = session.peer().notifications();
    for b in n.banners() {
        println!("  [{label}] banner({:?}): {}", b.severity, b.message);
    }
    for t in n.toasts() {
        println!("  [{label}] toast({:?}): {}", t.severity, t.message);
    }
}

// --- in-memory scenarios ----------------------------------------------------------------------

fn scenario_handshake() {
    header("handshake: two peers exchange versions");
    let (mut host, mut client) = pair(V, V, Config::default(), Config::default());
    host.connect();
    client.connect();
    converge(&mut host, &mut client);
    println!("  host knows peers: {:?}", host.peer().known_peers());
    println!("  client knows peers: {:?}", client.peer().known_peers());
    println!("  -> handshake complete, versions compatible");
}

fn scenario_version_mismatch() {
    header("version-mismatch: incompatible majors warn the user");
    let (mut host, mut client) =
        pair(Version::new(1, 2, 0), Version::new(2, 0, 0), Config::default(), Config::default());
    host.connect();
    client.connect();
    converge(&mut host, &mut client);
    show_notifications("client", &client);
    show_notifications("host", &host);
}

fn scenario_config_sync() {
    header("config-sync: host's shared settings converge to the client");
    let mut host_cfg = Config::default();
    host_cfg.scaling.boss_health = 250;
    host_cfg.gameplay.crit_coop = false;
    let (mut host, mut client) = pair(V, V, host_cfg, Config::default());

    println!(
        "  client BEFORE: boss_health={}, crit_coop={}",
        client.peer().config().scaling.boss_health,
        client.peer().config().gameplay.crit_coop
    );
    host.connect();
    client.connect();
    converge(&mut host, &mut client);
    println!(
        "  client AFTER:  boss_health={}, crit_coop={}",
        client.peer().config().scaling.boss_health,
        client.peer().config().gameplay.crit_coop
    );
    show_notifications("client", &client);
}

fn scenario_session_action() {
    header("session-action: host-only actions are authorized by sender role");
    let (mut host, mut client) = pair(V, V, Config::default(), Config::default());
    host.connect();
    client.connect();
    converge(&mut host, &mut client);

    println!("  client sends LockWorld (host-only)...");
    let lock = client.peer_mut().session_action(SessionAction::LockWorld);
    client.broadcast(lock);
    converge(&mut host, &mut client);
    println!("  host accepted action: {:?}", host.peer().last_action());
    show_notifications("host", &host);

    println!("  client sends JoinWorld (allowed for anyone)...");
    let join = client.peer_mut().session_action(SessionAction::JoinWorld);
    client.broadcast(join);
    converge(&mut host, &mut client);
    println!("  host accepted action: {:?}", host.peer().last_action());
}

fn scenario_log_forward() {
    header("log-forward: client debug logs aggregate on the host");
    let mut client_cfg = Config::default();
    client_cfg.debug.forward_to_host = true;
    let (mut host, mut client) = pair(V, V, Config::default(), client_cfg);
    host.connect();
    client.connect();
    converge(&mut host, &mut client);

    for (lvl, msg) in [
        (LogLevel::Info, "client loaded config"),
        (LogLevel::Warn, "WorldChrMan looked odd"),
        (LogLevel::Error, "decode failed once"),
    ] {
        let out = client.peer_mut().forward_log(lvl, msg);
        client.broadcast(out);
    }
    converge(&mut host, &mut client);
    println!("  host's aggregated bundle:");
    for line in host.peer().log_bundle().render().lines() {
        println!("    {line}");
    }
}

fn scenario_lossy() {
    header("lossy: config self-heals over an 85%-drop, reordering channel");
    let faults = FaultModel { drop_rate: 0.85, reorder: true, ..Default::default() };
    let ends = Loopback::mesh_with_faults(&[HOST, CLIENT], faults, 0xDEAD);
    let mut host_cfg = Config::default();
    host_cfg.scaling.boss_health = 250;
    let (mut host, mut client) = pair_on(ends, V, V, host_cfg, Config::default());

    println!("  channel: 85% drop + reorder (only the host's periodic re-assert heals it)");
    println!("  client BEFORE: boss_health={}", client.peer().config().scaling.boss_health);
    host.peer_mut().mark_config_changed(); // bump generation; rely on maintain() to re-assert
    host.connect();
    client.connect();

    // The host re-asserts each maintenance tick, so a dropped sync heals on a later round.
    let mut converged_at = None;
    for round in 1..=500 {
        host.maintain();
        client.maintain();
        host.pump();
        client.pump();
        if client.peer().config().scaling.boss_health == 250 {
            converged_at = Some(round);
            break;
        }
    }
    println!("  client AFTER:  boss_health={}", client.peer().config().scaling.boss_health);
    match converged_at {
        Some(round) => println!("  -> converged after {round} re-assert round(s) despite loss"),
        None => {
            eprintln!("  -> DID NOT converge within budget");
            std::process::exit(1); // make this a real gate, not just a demo print
        }
    }
}

// --- TCP (two-process) backend ----------------------------------------------------------------

/// Drive a TCP-backed session for a fixed number of maintenance ticks, sleeping between them so the
/// two processes interleave over the real socket.
fn drive_tcp(session: &mut Session<TcpTransport>, ticks: usize) {
    for _ in 0..ticks {
        session.maintain();
        session.pump();
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run_tcp_host(addr: &str) {
    let listener = TcpListener::bind(addr).expect("bind listener");
    println!("[host] listening on {addr}; waiting for a client…");
    let transport = TcpTransport::accept(&listener, HOST).expect("accept client");
    println!("[host] client connected");

    let mut host_cfg = Config::default();
    host_cfg.scaling.boss_health = 250;
    host_cfg.gameplay.crit_coop = false;
    let mut host = Session::new(Peer::new(HOST, HOST, V, host_cfg, HOST_NONCE), transport);
    host.connect();
    let changed = host.peer_mut().mark_config_changed();
    host.broadcast(changed);

    drive_tcp(&mut host, 40);
    println!("[host] final known peers: {:?}", host.peer().known_peers());
    println!("[host] done");
}

/// Connect to a **live mod's** debug bridge as the authoritative host and push it a config change,
/// to exercise the mod's apply path. We pick a recognizable, non-default `max_players` (and a couple
/// other shared fields); the mod (the bridge client) applies the `ConfigSync` to its live config,
/// and its `session-limit` feature re-applies the override — visible in the mod's own log as
/// `session player limit override set to <N>`. Here we just confirm the mod handshaked back.
fn run_bridge_host(addr: &str) {
    let transport = match TcpTransport::connect(addr, HOST) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[bridge-host] could not connect to bridge at {addr}: {e}");
            std::process::exit(1);
        }
    };
    println!("[bridge-host] connected to mod bridge at {addr}");

    // The config we push. max_players=4 is the recognizable change to watch land in the mod's log.
    let mut host_cfg = Config::default();
    host_cfg.session.max_players = 4;
    host_cfg.gameplay.crit_coop = true;
    host_cfg.scaling.boss_health = 250;
    // Match the rig's seed-config password so the peer auth proof links against the live mod (the
    // bridge now authenticates like the real side-channel). `scripts/rig/seed-config.toml` seeds
    // `coop-test`; if you re-seed with a different password, set it here too.
    host_cfg.session.password = "coop-test".to_string();
    let mut host = Session::new(Peer::new(HOST, HOST, V, host_cfg, HOST_NONCE), transport);
    host.connect();
    let changed = host.peer_mut().mark_config_changed();
    host.broadcast(changed);
    println!("[bridge-host] pushing config to the mod (max_players=4, crit_coop=true)…");
    drive_tcp(&mut host, 60);

    println!("[bridge-host] mod handshaked: known_peers = {:?}", host.peer().known_peers());
    if host.peer().known_peers().is_empty() {
        eprintln!("[bridge-host] FAILED: no handshake from the mod (bridge unreachable / not running?)");
        std::process::exit(1);
    }
    println!(
        "[bridge-host] OK: pushed config; check the mod log for `override set to 4` (apply landed)"
    );
}

fn run_tcp_client(addr: &str) {
    let transport = TcpTransport::connect(addr, CLIENT).expect("connect to host");
    println!("[client] connected to {addr}");
    let mut client = Session::new(Peer::new(CLIENT, HOST, V, Config::default(), CLIENT_NONCE), transport);
    println!("[client] boss_health BEFORE = {}", client.peer().config().scaling.boss_health);
    client.connect();

    drive_tcp(&mut client, 40);
    let synced = client.peer().config().scaling.boss_health;
    println!("[client] boss_health AFTER  = {synced}");
    println!("[client] crit_coop          = {}", client.peer().config().gameplay.crit_coop);
    println!("[client] known peers        = {:?}", client.peer().known_peers());
    for b in client.peer().notifications().banners() {
        println!("[client] banner: {}", b.message);
    }
    if synced != 250 {
        eprintln!("[client] FAILED: never synced the host's config over TCP");
        std::process::exit(1); // gate, so scripts/harness-tcp.sh fails on a real regression
    }
    println!("[client] done");
}
