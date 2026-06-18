//! salesman-receipts — Ed25519-signed Merkle hash chain over every
//! state-changing event (sends, approvals, suppressions).
//!
//! The chain gives us:
//! - tamper-evidence: every field of a receipt (id, event_kind,
//!   signing_key_id, created_at, prev_hash, payload) is authenticated by the
//!   signature (scheme v2), so any modification of a past receipt invalidates
//!   it and every later one.
//! - cryptographic proof of send: sender held the signing key at the
//!   time of send.
//! - replayable audit log: dump (event_payload, prev_hash, hash,
//!   signature) and re-verify offline.
//!
//! LIMITATION: end-of-chain truncation and full-table deletion are NOT
//! detectable from the receipts alone (a cleanly-truncated chain still
//! links). [`verify_chain_anchored`] detects them given a trusted head/count
//! stored OUTSIDE the receipts store (off-box / append-only); an anchor in
//! the same DB the attacker can edit gives no guarantee. See docs/AUDIT_CHAIN.md.
//!
//! BUG ASSUMPTION: the chain is per-key — [`verify_chain`] rejects a chain
//! whose `signing_key_id` changes mid-stream. If you rotate keys, start a
//! new chain.
//!
//! BUG ASSUMPTION: prev_hash for the genesis receipt is 32 zero bytes.
//!
//! SECURITY:
//! - Signing key is loaded from a 32-byte seed file, mode 0600. If
//!   the file does not exist, `Signer::load_or_generate` will create
//!   it (and write 0600). Caller is responsible for storing the file
//!   somewhere durable + backed up.
//! - The seed is held in-memory wrapped in `Zeroizing` so it's wiped
//!   on drop.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier, VerifyingKey};
use rand::RngCore;
use rand::rngs::OsRng;
use salesman_core::{Error, ReceiptId, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// Length in bytes of a SHA-256 chain hash.
pub const HASH_LEN: usize = 32;
/// Length in bytes of an Ed25519 signature.
pub const SIG_LEN: usize = 64;

/// One link in the chain. Hashes and signature are kept as `Vec<u8>`
/// (not fixed-size arrays) so the type plays nicely with serde's
/// derive AND with the Postgres BYTEA mapping. Lengths are
/// invariants enforced by constructors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    /// Stable identifier for this receipt.
    pub id: ReceiptId,
    /// The kind of event recorded (e.g. `send.email`, `suppression`).
    pub event_kind: String,
    /// The event payload that was signed.
    pub event_payload: serde_json::Value,
    /// 32-byte hash of the previous receipt. Zeros for genesis.
    pub prev_hash: Vec<u8>,
    /// 32-byte SHA-256 over the canonical encoding of the FULL receipt
    /// (id, event_kind, signing_key_id, created_at, prev_hash, payload) —
    /// scheme v2; see [`signing_hash`]. Authenticates every field, not just
    /// prev_hash||payload.
    pub hash: Vec<u8>,
    /// 64-byte Ed25519 signature over `hash`.
    pub signature: Vec<u8>,
    /// The id of the signing key that produced `signature`.
    pub signing_key_id: String,
    /// When this receipt was created.
    pub created_at: DateTime<Utc>,
}

/// Holds an Ed25519 signing key and signs receipts onto its chain.
#[derive(Debug)]
pub struct Signer {
    signing_key: SigningKey,
    key_id: String,
}

impl Signer {
    /// Load a signing key from a seed file (mode 0600). Generate +
    /// persist one if the file does not exist.
    pub fn load_or_generate(seed_path: &Path, key_id: impl Into<String>) -> Result<Self> {
        let key_id = key_id.into();
        if seed_path.exists() {
            // seed_path is operator-controlled config (a fixed signing-seed
            // location, mode 0600), never agent/network input. nosemgrep
            let bytes = fs::read(seed_path).map_err(Error::Io)?; // nosemgrep
            if bytes.len() != 32 {
                return Err(Error::Config(format!(
                    "signing seed file `{}` must be exactly 32 bytes (was {})",
                    seed_path.display(),
                    bytes.len()
                )));
            }
            // SAFETY: the surrounding `if bytes.len() != 32` check
            // already returned on mismatch — so the slice is exactly
            // 32 bytes here and try_from cannot fail.
            let seed = Zeroizing::new(<[u8; 32]>::try_from(bytes.as_slice()).expect("len==32"));
            let signing_key = SigningKey::from_bytes(&seed);
            return Ok(Self {
                signing_key,
                key_id,
            });
        }

        // Generate new.
        if let Some(parent) = seed_path.parent() {
            fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let mut seed = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(seed.as_mut());
        let signing_key = SigningKey::from_bytes(&seed);

        // Write 0600.
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        // seed_path is operator-controlled config (see note above). nosemgrep
        let mut f = opts.open(seed_path).map_err(Error::Io)?; // nosemgrep
        f.write_all(seed.as_ref()).map_err(Error::Io)?;
        f.sync_all().map_err(Error::Io)?;
        tracing::info!(path = %seed_path.display(), %key_id, "generated new signing key");

        Ok(Self {
            signing_key,
            key_id,
        })
    }

    /// The key id stamped onto every receipt this signer produces
    /// (`Receipt::signing_key_id`); chains are verified per key id.
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// The public verifying key for this signer — hand to
    /// [`verify_receipt`] / [`verify_chain`] to check signatures offline.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Append a receipt to the chain. Caller supplies prev_hash
    /// (must be 32 bytes — zero-filled for genesis).
    pub fn sign_event(
        &self,
        event_kind: impl Into<String>,
        event_payload: serde_json::Value,
        prev_hash: &[u8],
    ) -> Result<Receipt> {
        if prev_hash.len() != HASH_LEN {
            return Err(Error::Validation(format!(
                "prev_hash must be {HASH_LEN} bytes, got {}",
                prev_hash.len()
            )));
        }
        let id = ReceiptId::new();
        let event_kind = event_kind.into();
        let created_at = Utc::now();
        let hash_arr = signing_hash(
            &id,
            &event_kind,
            &self.key_id,
            &created_at,
            prev_hash,
            &event_payload,
        );
        let sig: Signature = self.signing_key.sign(&hash_arr);
        Ok(Receipt {
            id,
            event_kind,
            event_payload,
            prev_hash: prev_hash.to_vec(),
            hash: hash_arr.to_vec(),
            signature: sig.to_bytes().to_vec(),
            signing_key_id: self.key_id.clone(),
            created_at,
        })
    }
}

/// Verify a single receipt against a known verifying key. Does NOT
/// check chain linkage — caller is responsible for sequencing.
pub fn verify_receipt(receipt: &Receipt, vk: &VerifyingKey) -> Result<()> {
    if receipt.hash.len() != HASH_LEN || receipt.prev_hash.len() != HASH_LEN {
        return Err(Error::Validation("receipt hash length wrong".into()));
    }
    if receipt.signature.len() != SIG_LEN {
        return Err(Error::Validation("receipt signature length wrong".into()));
    }
    let recomputed = signing_hash(
        &receipt.id,
        &receipt.event_kind,
        &receipt.signing_key_id,
        &receipt.created_at,
        &receipt.prev_hash,
        &receipt.event_payload,
    );
    if recomputed.as_slice() != receipt.hash.as_slice() {
        return Err(Error::Validation("receipt hash mismatch".into()));
    }
    let hash_arr: [u8; HASH_LEN] = recomputed;
    let sig_arr: [u8; SIG_LEN] = <[u8; SIG_LEN]>::try_from(receipt.signature.as_slice())
        .map_err(|_| Error::Validation("signature length wrong".into()))?;
    let sig = Signature::from_bytes(&sig_arr);
    vk.verify(&hash_arr, &sig)
        .map_err(|e| Error::Validation(format!("ed25519 verify: {e}")))?;
    Ok(())
}

/// Verify a sequence of receipts. Each receipt's `prev_hash` must equal the
/// previous receipt's `hash`; the first is checked against `initial_prev`
/// (zeros for genesis); all must share one `signing_key_id` (a chain is
/// per-key). NOTE: without an external anchor this CANNOT detect end-of-chain
/// truncation or full deletion — a cleanly-truncated chain still links. Use
/// [`verify_chain_anchored`] with a trusted head/count to close that gap.
pub fn verify_chain(receipts: &[Receipt], vk: &VerifyingKey, initial_prev: &[u8]) -> Result<()> {
    verify_chain_anchored(receipts, vk, initial_prev, None, None)
}

/// [`verify_chain`] plus optional truncation/deletion detection against a
/// trusted external anchor: when `expected_head` is given, the last receipt's
/// `hash` must equal it (empty chain ⇒ `initial_prev`); when `expected_count`
/// is given, the receipt count must match. The anchor MUST live somewhere an
/// attacker who can edit the receipts table cannot also edit (off-box /
/// append-only) — an anchor in the same DB gives no guarantee. See
/// docs/AUDIT_CHAIN.md.
pub fn verify_chain_anchored(
    receipts: &[Receipt],
    vk: &VerifyingKey,
    initial_prev: &[u8],
    expected_head: Option<&[u8]>,
    expected_count: Option<usize>,
) -> Result<()> {
    if initial_prev.len() != HASH_LEN {
        return Err(Error::Validation("initial_prev wrong length".into()));
    }
    if let Some(n) = expected_count
        && receipts.len() != n
    {
        return Err(Error::Validation(format!(
            "chain length {} != expected {n} (truncation, insertion, or deletion?)",
            receipts.len()
        )));
    }
    let mut expected_prev: Vec<u8> = initial_prev.to_vec();
    let mut chain_key: Option<&str> = None;
    for (idx, r) in receipts.iter().enumerate() {
        match chain_key {
            None => chain_key = Some(&r.signing_key_id),
            Some(k) if k != r.signing_key_id => {
                return Err(Error::Validation(format!(
                    "chain break at index {idx}: signing_key_id changed within a single chain"
                )));
            }
            _ => {}
        }
        if r.prev_hash != expected_prev {
            return Err(Error::Validation(format!(
                "chain break at index {idx}: prev_hash does not match previous receipt's hash"
            )));
        }
        verify_receipt(r, vk)?;
        expected_prev = r.hash.clone();
    }
    if let Some(head) = expected_head {
        if head.len() != HASH_LEN {
            return Err(Error::Validation("expected_head wrong length".into()));
        }
        // `expected_prev` is now the last receipt's hash, or `initial_prev`
        // for an empty chain — so a deleted tail / emptied table is caught.
        if expected_prev.as_slice() != head {
            return Err(Error::Validation(
                "chain head mismatch: tail truncation or deletion detected".into(),
            ));
        }
    }
    Ok(())
}

/// Canonical JSON: serde_json with sorted keys + no whitespace.
fn canonical_json(v: &serde_json::Value) -> Vec<u8> {
    // serde_json sorts map keys when the `preserve_order` feature is
    // not enabled, and it defaults to no whitespace via to_vec. That
    // gives us a deterministic byte representation across platforms.
    serde_json::to_vec(v).unwrap_or_default()
}

/// The 32-byte hash that is signed for a receipt (scheme v2). Authenticates
/// the FULL receipt — id, event_kind, signing_key_id, created_at, prev_hash,
/// and payload — by hashing the canonical JSON of all of them, so none can be
/// mutated after signing without invalidating the signature. `created_at` is
/// emitted at microsecond precision so it survives the Postgres `timestamptz`
/// round-trip (which truncates sub-microsecond digits and would otherwise make
/// a loaded receipt fail verification). Both [`Signer::sign_event`] and
/// [`verify_receipt`] go through this one function — the single source of truth.
fn signing_hash(
    id: &ReceiptId,
    event_kind: &str,
    signing_key_id: &str,
    created_at: &DateTime<Utc>,
    prev_hash: &[u8],
    payload: &serde_json::Value,
) -> [u8; HASH_LEN] {
    let preimage = serde_json::json!({
        "v": 2,
        "id": id,
        "event_kind": event_kind,
        "signing_key_id": signing_key_id,
        "created_at": created_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        "prev_hash": hex::encode(prev_hash),
        "payload": payload,
    });
    let mut hasher = Sha256::new();
    hasher.update(&canonical_json(&preimage));
    hasher.finalize().into()
}

/// The all-zero 32-byte hash used as the genesis receipt's `prev_hash`.
pub fn zero_hash() -> Vec<u8> {
    vec![0u8; HASH_LEN]
}

/// Hex-encode a hash / `prev_hash` for display, logging, or storage.
pub fn hash_to_hex(h: &[u8]) -> String {
    hex::encode(h)
}

/// Decode a hex string (the inverse of [`hash_to_hex`]) back into raw
/// bytes. Errors if the input is not valid hex.
pub fn hex_to_hash(s: &str) -> Result<Vec<u8>> {
    hex::decode(s).map_err(|e| Error::Validation(format!("hex: {e}")))
}

/// Default on-disk location of the Ed25519 signing seed (mode 0600).
/// Callers may override this; see [`Signer::load_or_generate`].
pub fn default_seed_path() -> PathBuf {
    PathBuf::from("/opt/salesman/config/signing.seed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::SubsecRound;
    use serde_json::json;

    fn tmp_signer() -> Signer {
        let dir = std::env::temp_dir();
        let unique = format!(
            "salesman_signer_test_{}.seed",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let path = dir.join(unique);
        let s = Signer::load_or_generate(&path, "test-key-1").unwrap();
        std::fs::remove_file(&path).ok();
        s
    }

    #[test]
    fn sign_and_verify_single_receipt() {
        let s = tmp_signer();
        let r = s
            .sign_event(
                "send.email",
                json!({"to": "a@b", "subject": "x"}),
                &zero_hash(),
            )
            .unwrap();
        let vk = s.verifying_key();
        verify_receipt(&r, &vk).unwrap();
    }

    #[test]
    fn detects_payload_tampering() {
        let s = tmp_signer();
        let mut r = s.sign_event("x", json!({"v":1}), &zero_hash()).unwrap();
        r.event_payload = json!({"v":2});
        let vk = s.verifying_key();
        assert!(verify_receipt(&r, &vk).is_err());
    }

    #[test]
    fn chains_links_correctly() {
        let s = tmp_signer();
        let r1 = s.sign_event("a", json!({"v":1}), &zero_hash()).unwrap();
        let r2 = s.sign_event("b", json!({"v":2}), &r1.hash).unwrap();
        let r3 = s.sign_event("c", json!({"v":3}), &r2.hash).unwrap();
        let vk = s.verifying_key();
        verify_chain(&[r1, r2, r3], &vk, &zero_hash()).unwrap();
    }

    #[test]
    fn detects_chain_break() {
        let s = tmp_signer();
        let r1 = s.sign_event("a", json!({"v":1}), &zero_hash()).unwrap();
        let mut r2 = s.sign_event("b", json!({"v":2}), &r1.hash).unwrap();
        let r3 = s.sign_event("c", json!({"v":3}), &r2.hash).unwrap();
        // Break the link between r1 and r2 by replacing r2.prev_hash.
        r2.prev_hash = zero_hash();
        let vk = s.verifying_key();
        assert!(verify_chain(&[r1, r2, r3], &vk, &zero_hash()).is_err());
    }

    #[test]
    fn detects_signature_tampering() {
        let s = tmp_signer();
        let mut r = s.sign_event("x", json!({"v":1}), &zero_hash()).unwrap();
        // Flip one bit in the signature; the length stays SIG_LEN so the
        // mutated receipt reaches the ed25519 verify (not the length
        // guard) and must be rejected there.
        r.signature[0] ^= 0x01;
        assert_eq!(r.signature.len(), SIG_LEN);
        assert!(verify_receipt(&r, &s.verifying_key()).is_err());
    }

    #[test]
    fn rejects_a_different_signers_key() {
        let s = tmp_signer();
        let other = tmp_signer();
        let r = s.sign_event("x", json!({"v":1}), &zero_hash()).unwrap();
        // The same receipt under the wrong public key must not verify...
        assert!(verify_receipt(&r, &other.verifying_key()).is_err());
        // ...but the matching key still does (sanity).
        verify_receipt(&r, &s.verifying_key()).unwrap();
    }

    #[test]
    fn rejects_malformed_field_lengths() {
        let s = tmp_signer();
        let vk = s.verifying_key();

        // Signature too short → length guard, before ed25519.
        let mut r = s.sign_event("x", json!({"v":1}), &zero_hash()).unwrap();
        r.signature.truncate(SIG_LEN - 1);
        assert!(verify_receipt(&r, &vk).is_err());

        // Hash wrong length.
        let mut r = s.sign_event("x", json!({"v":1}), &zero_hash()).unwrap();
        r.hash.push(0);
        assert!(verify_receipt(&r, &vk).is_err());

        // prev_hash wrong length.
        let mut r = s.sign_event("x", json!({"v":1}), &zero_hash()).unwrap();
        r.prev_hash.pop();
        assert!(verify_receipt(&r, &vk).is_err());
    }

    #[test]
    fn verify_chain_detects_tampered_middle_receipt() {
        let s = tmp_signer();
        let r1 = s.sign_event("a", json!({"v":1}), &zero_hash()).unwrap();
        let mut r2 = s.sign_event("b", json!({"v":2}), &r1.hash).unwrap();
        let r3 = s.sign_event("c", json!({"v":3}), &r2.hash).unwrap();
        // Tamper r2's payload but leave its hash/prev_hash intact: the
        // chain LINKAGE still appears valid, yet r2's hash no longer
        // matches its payload — verify_chain must still reject it, proving
        // it validates content per-receipt, not just linkage.
        r2.event_payload = json!({"v":99});
        let vk = s.verifying_key();
        assert!(verify_chain(&[r1, r2, r3], &vk, &zero_hash()).is_err());
    }

    #[test]
    fn verify_chain_rejects_wrong_initial_prev() {
        let s = tmp_signer();
        let r1 = s.sign_event("a", json!({"v":1}), &zero_hash()).unwrap();
        let vk = s.verifying_key();
        // Genesis receipt was signed against zero_hash; asserting a
        // non-zero initial_prev must fail the index-0 linkage check.
        let mut wrong = zero_hash();
        wrong[0] = 1;
        assert!(verify_chain(&[r1], &vk, &wrong).is_err());
    }

    #[test]
    fn verify_chain_rejects_bad_initial_prev_length() {
        let s = tmp_signer();
        let r1 = s.sign_event("a", json!({"v":1}), &zero_hash()).unwrap();
        let vk = s.verifying_key();
        assert!(verify_chain(&[r1], &vk, &[0u8; 8]).is_err());
    }

    #[test]
    fn verify_chain_empty_is_ok() {
        let s = tmp_signer();
        // No receipts → vacuously valid (documents the edge).
        verify_chain(&[], &s.verifying_key(), &zero_hash()).unwrap();
    }

    #[test]
    fn canonical_json_is_key_order_independent() {
        // The canonical encoding must be stable regardless of the order in
        // which a payload's keys were constructed (serde_json sorts map keys
        // with no `preserve_order` feature). Tested directly on canonical_json:
        // the full receipt hash now also authenticates the random `id` and the
        // per-receipt `created_at`, so two distinct receipts never share a hash
        // even with identical payloads.
        assert_eq!(
            canonical_json(&json!({"a":1, "b":2})),
            canonical_json(&json!({"b":2, "a":1})),
            "canonical_json must not depend on key insertion order"
        );
    }

    #[test]
    fn full_receipt_fields_are_authenticated() {
        // Scheme v2: mutating ANY signed field — not just the payload — must
        // break verification. This is the core of the audit-chain hardening.
        let s = tmp_signer();
        let vk = s.verifying_key();
        let base = s
            .sign_event("send.email", json!({"v":1}), &zero_hash())
            .unwrap();

        let mut r = base.clone();
        r.event_kind = "suppression".into();
        assert!(
            verify_receipt(&r, &vk).is_err(),
            "event_kind unauthenticated"
        );

        let mut r = base.clone();
        r.signing_key_id = "evil-key".into();
        assert!(
            verify_receipt(&r, &vk).is_err(),
            "signing_key_id unauthenticated"
        );

        let mut r = base.clone();
        r.id = ReceiptId::new();
        assert!(verify_receipt(&r, &vk).is_err(), "id unauthenticated");

        let mut r = base.clone();
        r.created_at += chrono::Duration::seconds(1);
        assert!(
            verify_receipt(&r, &vk).is_err(),
            "created_at unauthenticated"
        );

        // Sanity: the untouched receipt still verifies.
        verify_receipt(&base, &vk).unwrap();
    }

    #[test]
    fn created_at_survives_microsecond_round_trip() {
        // Postgres timestamptz stores microseconds; signing emits microsecond
        // precision so a loaded (truncated) receipt still verifies. Simulate it.
        let s = tmp_signer();
        let mut r = s.sign_event("x", json!({"v":1}), &zero_hash()).unwrap();
        r.created_at = r.created_at.trunc_subsecs(6);
        verify_receipt(&r, &s.verifying_key()).unwrap();
    }

    #[test]
    fn verify_chain_rejects_mixed_signing_key_ids() {
        let s = tmp_signer();
        let r1 = s.sign_event("a", json!({"v":1}), &zero_hash()).unwrap();
        let mut r2 = s.sign_event("b", json!({"v":2}), &r1.hash).unwrap();
        r2.signing_key_id = "other-key".into();
        assert!(verify_chain(&[r1, r2], &s.verifying_key(), &zero_hash()).is_err());
    }

    #[test]
    fn anchored_chain_detects_truncation_and_deletion() {
        let s = tmp_signer();
        let vk = s.verifying_key();
        let r1 = s.sign_event("a", json!({"v":1}), &zero_hash()).unwrap();
        let r2 = s.sign_event("b", json!({"v":2}), &r1.hash).unwrap();
        let r3 = s.sign_event("c", json!({"v":3}), &r2.hash).unwrap();
        let head = r3.hash.clone();
        // Full chain matches the anchored head + count.
        verify_chain_anchored(
            &[r1.clone(), r2.clone(), r3.clone()],
            &vk,
            &zero_hash(),
            Some(&head),
            Some(3),
        )
        .unwrap();
        // Tail truncation: linkage still clean, but head + count don't match.
        assert!(
            verify_chain_anchored(
                &[r1.clone(), r2.clone()],
                &vk,
                &zero_hash(),
                Some(&head),
                Some(3)
            )
            .is_err()
        );
        // Full deletion: empty chain vs a non-zero anchored head.
        assert!(verify_chain_anchored(&[], &vk, &zero_hash(), Some(&head), Some(3)).is_err());
    }

    #[test]
    fn unanchored_chain_cannot_detect_truncation() {
        // Documents WHY the anchor is required: a cleanly-truncated chain still
        // verifies without one.
        let s = tmp_signer();
        let r1 = s.sign_event("a", json!({"v":1}), &zero_hash()).unwrap();
        let r2 = s.sign_event("b", json!({"v":2}), &r1.hash).unwrap();
        let _r3 = s.sign_event("c", json!({"v":3}), &r2.hash).unwrap();
        verify_chain(&[r1, r2], &s.verifying_key(), &zero_hash()).unwrap();
    }
}
