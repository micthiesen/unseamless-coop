//! Two-peer side-channel harness — the fast test loop that needs no game and no Steam.
//!
//! It wires a host + client (both running the real `unseamless_core::peer::Peer` coordination
//! logic) over an in-memory `Loopback` transport and runs scenarios, printing the result so an
//! operator (or an assistant driving this loop) can read what happened. This exercises the side
//! channel end to end: the version handshake, host->client config sync, action authorization, and
//! client->host log forwarding. It does NOT exercise the game's own player/world sync (that needs
//! the game; see docs/RIG-RUNBOOK.md and the /test-loop skill).
//!
//! Run on the host (the workspace default target is windows-gnu, which can't execute here):
//!   scripts/harness.sh [scenario]      # scenario: handshake | version-mismatch | config-sync |
//!                                      #           session-action | log-forward | all (default)

use unseamless_core::config::Config;
use unseamless_core::diagnostics::LogLevel;
use unseamless_core::peer::{Peer, Session};
use unseamless_core::protocol::{ModMessage, SessionAction};
use unseamless_core::transport::{Loopback, PeerId};
use unseamless_core::util::Version;

const HOST: PeerId = 1;
const CLIENT: PeerId = 2;
const V: Version = Version::new(0, 1, 0);

fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    let scenarios: &[(&str, fn())] = &[
        ("handshake", scenario_handshake),
        ("version-mismatch", scenario_version_mismatch),
        ("config-sync", scenario_config_sync),
        ("session-action", scenario_session_action),
        ("log-forward", scenario_log_forward),
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
                "unknown scenario '{which}'. options: {}, all",
                scenarios.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
            );
            std::process::exit(2);
        }
    }
}

// --- harness plumbing -------------------------------------------------------------------------

/// Build a host + client pair over a shared loopback, with the given versions and configs.
fn pair(
    host_v: Version,
    client_v: Version,
    host_cfg: Config,
    client_cfg: Config,
) -> (Session<Loopback>, Session<Loopback>) {
    let ends = Loopback::mesh(&[HOST, CLIENT]);
    let mut it = ends.into_iter();
    let host = Session::new(Peer::new(HOST, HOST, host_v, host_cfg), it.next().unwrap());
    let client = Session::new(Peer::new(CLIENT, HOST, client_v, client_cfg), it.next().unwrap());
    (host, client)
}

/// Step both sessions until no frames remain in flight.
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

// --- scenarios --------------------------------------------------------------------------------

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
    host_cfg.gameplay.allow_invaders = false;
    let (mut host, mut client) = pair(V, V, host_cfg, Config::default());

    println!(
        "  client BEFORE: boss_health={}, allow_invaders={}",
        client.peer().config().scaling.boss_health,
        client.peer().config().gameplay.allow_invaders
    );
    host.connect();
    client.connect();
    converge(&mut host, &mut client);
    println!(
        "  client AFTER:  boss_health={}, allow_invaders={}",
        client.peer().config().scaling.boss_health,
        client.peer().config().gameplay.allow_invaders
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
    client.broadcast(vec![ModMessage::SessionAction(SessionAction::LockWorld)]);
    converge(&mut host, &mut client);
    println!("  host accepted action: {:?}", host.peer().last_action());
    show_notifications("host", &host);

    println!("  client sends GiveEmber (allowed for anyone)...");
    client.broadcast(vec![ModMessage::SessionAction(SessionAction::GiveEmber)]);
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
