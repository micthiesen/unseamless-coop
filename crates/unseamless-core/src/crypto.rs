//! Password-keyed hash primitives, kept side by side so their **domain-separation invariant** is
//! impossible to miss.
//!
//! The mod derives two distinct values from the same shared co-op password:
//! - [`lobby_discovery_token`] — published **world-readable** as the `usc_pw` Steam-lobby datum so
//!   two players with the same password find each other.
//! - [`auth_proof`] — a per-session handshake proof that authenticates a peer's password knowledge
//!   over the private side-channel.
//!
//! Because the discovery token is public, the two **must** be cryptographically separated: even for
//! the same password the values must differ, so grabbing the public token tells an attacker nothing
//! about a valid proof. That separation is enforced by giving each hash a **distinct domain tag**
//! ([`LOBBY_DISCOVERY_DOMAIN`] vs [`PEER_AUTH_DOMAIN`]). **Keep the two tags distinct** — merging
//! them would let the public token stand in for an auth proof. The whole reason both live in this one
//! module is so that invariant is visible at a glance; the
//! `auth_proof_domain_is_separated_from_the_public_discovery_token` test below pins it.

use crate::protocol::{AUTH_PROOF_LEN, AuthNonce, AuthProofBytes};
use crate::transport::PeerId;

/// Domain separator for the **public** lobby-discovery token. Ends with a literal NUL (`\0`) before
/// the password bytes — the framing convention both hashes share. **Deliberately distinct** from
/// [`PEER_AUTH_DOMAIN`] (see the module docs): this value is published world-readable, so it must
/// never collide with the private auth proof.
const LOBBY_DISCOVERY_DOMAIN: &[u8] = b"unseamless-coop/lobby-discovery/v1\0";

/// Domain separator for the **private** peer-authentication proof. **Deliberately distinct** from
/// [`LOBBY_DISCOVERY_DOMAIN`] (see the module docs): the discovery token is published world-readable,
/// so the auth proof must be cryptographically separated from it — even for the same password the two
/// values must differ, so grabbing the public token tells an attacker nothing about a valid proof.
/// Ends with a literal NUL before the nonces, matching the discovery token's framing convention.
const PEER_AUTH_DOMAIN: &[u8] = b"unseamless-coop/peer-auth/v1\0";

/// The rung-4 lobby-discovery **password token** — the value a host publishes as the `usc_pw` lobby
/// datum and a joiner filters the lobby list by. This is the cross-implementation **contract**: the DLL
/// hand-bind ([`crate`]'s sibling `coop/steam.rs`) and the `harness` lobby prototype must produce the
/// **byte-identical** token or two players with the same password never find each other.
///
/// `token = lowercase_hex( SHA-256(LOBBY_DISCOVERY_DOMAIN || password_bytes)[0..16] )`
///
/// Load-bearing details, each one a silent-discovery-break if violated:
/// - the domain-separator prefix ([`LOBBY_DISCOVERY_DOMAIN`]) ends with a **literal NUL** (`\0`)
///   before the password bytes;
/// - the password is hashed **verbatim** — the caller must pass the raw configured bytes with **no**
///   trim, case-fold, or Unicode normalization (a stray normalize in the config layer breaks this);
/// - only the **first 16 bytes** of the digest are taken, rendered **lowercase** hex (32 chars).
///
/// SHA-256 here is for a stable, well-specified, collision-resistant keying of a shared secret into a
/// public lobby field — not a confidentiality primitive (lobby data is world-readable). Pinned by the
/// known-answer test below; the harness carries the matching KAT.
pub fn lobby_discovery_token(password: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(LOBBY_DISCOVERY_DOMAIN);
    hasher.update(password.as_bytes());
    let digest = hasher.finalize();
    // First 16 bytes, lowercase hex (no external hex crate — format each byte).
    let mut token = String::with_capacity(32);
    for byte in &digest[..16] {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

/// The password-keyed handshake proof a **prover** (the peer sending an [`crate::protocol::ModMessage::Auth`])
/// presents to a **verifier** (the recipient), binding both peers' identities and per-session nonces
/// to the shared co-op password:
///
/// `proof = SHA-256(PEER_AUTH_DOMAIN || verifier_id || prover_id || verifier_nonce || prover_nonce || password)`
///
/// Both sides feed the **same** `(verifier, prover)` ordering so the value matches: the prover passes
/// the verifier's id+nonce (learned from its `Hello` / the transport) and its own; the verifier
/// recomputes with itself as `verifier` and the sender as `prover`. Two properties matter:
/// - **Replay resistance** — the verifier's fresh nonce is mixed in, so a proof captured from a past
///   session won't verify against this session's verifier nonce.
/// - **Reflection resistance** — the *directed pair* `(verifier_id, prover_id)` is part of the hash,
///   and the ids come from the transport (`from`/`self.id`), **not** from attacker-chosen wire data.
///   Without the ids, the two handshake directions are symmetric under swapping the two nonces, so an
///   attacker that has no password could advertise a `Hello` nonce equal to the victim's, capture the
///   victim's outgoing proof, and reflect it back as a valid-looking inbound proof. Including the
///   id pair (which an attacker cannot equalize — it can't be both peers) makes the prover→verifier
///   and verifier→prover inputs differ even when the nonces collide, defeating the reflection.
///
/// The ids/nonces are fixed-length so the concatenation is unambiguous; the password is hashed
/// verbatim (no trim/case-fold), matching the discovery token. SHA-256 keyed by the shared secret is
/// sufficient here (no length-extension exposure: an attacker has no valid proof to extend, and the
/// secret is last). Pinned by a known-answer test below.
///
/// **Known limits** (acceptable for this threat model, documented so they're a conscious choice):
/// - *Bounded by password entropy.* This is a hash challenge-response, not a PAKE: an attacker who
///   sniffs one `Hello` pair + `Auth` (or just reads the world-readable discovery token) can grind a
///   weak password offline. The auto-generated default password has ample entropy; a user-chosen one
///   is only as strong as it is long (the startup guard enforces a minimum). This does not regress
///   relative to the pre-existing discovery token, which is likewise a fast hash of the password.
/// - *One-shot proof, no session key.* Verifying the proof authenticates the peer's password
///   knowledge once and marks it linked **by transport id**; subsequent frames are not individually
///   MAC'd. Ongoing integrity therefore rests on the transport authenticating sender ids (Steam P2P),
///   which is also what makes the `(verifier_id, prover_id)` binding above unspoofable.
pub fn auth_proof(
    verifier_id: PeerId,
    prover_id: PeerId,
    verifier_nonce: &AuthNonce,
    prover_nonce: &AuthNonce,
    password: &str,
) -> AuthProofBytes {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(PEER_AUTH_DOMAIN);
    hasher.update(verifier_id.to_be_bytes());
    hasher.update(prover_id.to_be_bytes());
    hasher.update(verifier_nonce);
    hasher.update(prover_nonce);
    hasher.update(password.as_bytes());
    let digest = hasher.finalize();
    let mut proof = [0u8; AUTH_PROOF_LEN];
    proof.copy_from_slice(&digest[..AUTH_PROOF_LEN]);
    proof
}

/// Constant-time equality for two proofs: compares every byte (no early-out on first mismatch) so a
/// verifier doesn't leak how many leading bytes matched. Belt-and-suspenders here (the per-session
/// nonces already stop an attacker from iterating against a fixed challenge), but cheap and correct.
pub fn proofs_match(a: &AuthProofBytes, b: &AuthProofBytes) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::AUTH_NONCE_LEN;

    #[test]
    fn lobby_discovery_token_matches_the_pinned_contract() {
        // Known-answer test: these must match the harness's KAT byte-for-byte (the DLL hand-bind and
        // the harness both call this fn, but the values are pinned independently so a future edit to
        // the domain string / digest slice / hex casing is caught as the discovery-breaking change it
        // is). Values computed from SHA-256(LOBBY_DISCOVERY_DOMAIN || password)[0..16].
        assert_eq!(lobby_discovery_token("swordfish"), "e1ae25ea4eab35799470c31622b014b8");
        assert_eq!(lobby_discovery_token(""), "997351a38b7ef8eecef4d5c57de65ff4");
        assert_eq!(lobby_discovery_token("hunter2"), "1ad477bb65bcc83f7235160ee4b63883");
        // 16 bytes -> 32 lowercase hex chars, and the password is keyed verbatim (case-sensitive).
        assert_eq!(lobby_discovery_token("hunter2").len(), 32);
        assert_ne!(lobby_discovery_token("hunter2"), lobby_discovery_token("Hunter2"));
    }

    #[test]
    fn auth_proof_is_deterministic_and_order_sensitive() {
        // Same inputs -> same proof (so both sides agree); swapping the (id, nonce) roles -> different
        // proof (the verifier/prover ordering is load-bearing, not symmetric).
        let (a, b) = (10u64, 20u64);
        let v = [1u8; AUTH_NONCE_LEN];
        let p = [2u8; AUTH_NONCE_LEN];
        assert_eq!(auth_proof(a, b, &v, &p, "pw"), auth_proof(a, b, &v, &p, "pw"));
        assert_ne!(auth_proof(a, b, &v, &p, "pw"), auth_proof(b, a, &p, &v, "pw"), "role order matters");
        assert_ne!(auth_proof(a, b, &v, &p, "pw"), auth_proof(a, b, &v, &p, "x"), "password keys it");
        assert!(proofs_match(&auth_proof(a, b, &v, &p, "pw"), &auth_proof(a, b, &v, &p, "pw")));
        assert!(!proofs_match(&auth_proof(a, b, &v, &p, "pw"), &auth_proof(a, b, &v, &p, "x")));
    }

    #[test]
    fn auth_proof_is_reflection_resistant_under_equal_nonces() {
        // The attack the id-binding defends against: even when the two nonces are IDENTICAL (an
        // attacker mirroring the victim's nonce), the directed id pair makes the prover->verifier and
        // verifier->prover inputs differ, so a victim's outgoing proof is not a valid inbound proof.
        let (victim, attacker) = (1u64, 99u64);
        let n = [7u8; AUTH_NONCE_LEN];
        // What the victim hands the attacker (victim is prover, attacker is verifier).
        let victim_outgoing = auth_proof(attacker, victim, &n, &n, "pw");
        // What the victim would accept from the attacker (victim verifier, attacker prover).
        let victim_expects = auth_proof(victim, attacker, &n, &n, "pw");
        assert_ne!(victim_outgoing, victim_expects, "a reflected proof must not verify");
    }

    #[test]
    fn auth_proof_domain_is_separated_from_the_public_discovery_token() {
        // The security property is the *domain separation* itself: the proof's domain must never equal
        // the discovery token's, so the two can't collide for the same password (the discovery token is
        // published world-readable on the public Steam lobby). Assert that directly — this is what
        // guards the "don't fix the domain back" regression. Co-locating the two constants here is what
        // makes the invariant legible; this test pins it. (The rendered values also differ, but that
        // inequality alone is incidental: the proof interposes ids+nonces between domain and password,
        // so it would differ from the token even if the domains were identical, which is why the value
        // check below can't stand in for the domain assertion.)
        assert_ne!(
            PEER_AUTH_DOMAIN, LOBBY_DISCOVERY_DOMAIN,
            "the auth proof domain must stay distinct from the discovery-token domain"
        );
        let pw = "shared-secret";
        let token = lobby_discovery_token(pw); // 32 lowercase-hex chars (16 bytes)
        let proof = auth_proof(1, 2, &[0u8; AUTH_NONCE_LEN], &[0u8; AUTH_NONCE_LEN], pw);
        let proof_hex: String = proof[..16].iter().map(|b| format!("{b:02x}")).collect();
        assert_ne!(proof_hex, token, "auth proof must not render to the public discovery token");
    }
}
