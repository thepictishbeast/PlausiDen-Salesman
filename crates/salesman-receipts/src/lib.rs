//! salesman-receipts — Ed25519-signed Merkle hash chain over every
//! state-changing event (sends, approvals, suppressions).
//!
//! The chain gives us:
//! - tamper-evidence: any modification of a past receipt invalidates
//!   every later one.
//! - cryptographic proof of send: sender held the signing key at the
//!   time of send.
//! - replayable audit log: dump (event_payload, prev_hash, hash,
//!   signature) and re-verify offline.
//!
//! BUG ASSUMPTION: the chain is per-key. If you rotate keys, start a
//! new chain — tools verify within a single key id.
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

pub const HASH_LEN: usize = 32;
pub const SIG_LEN: usize = 64;

/// One link in the chain. Hashes and signature are kept as `Vec<u8>`
/// (not fixed-size arrays) so the type plays nicely with serde's
/// derive AND with the Postgres BYTEA mapping. Lengths are
/// invariants enforced by constructors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub id: ReceiptId,
    pub event_kind: String,
    pub event_payload: serde_json::Value,
    /// 32-byte hash of the previous receipt. Zeros for genesis.
    pub prev_hash: Vec<u8>,
    /// 32-byte SHA-256 over (prev_hash || canonical_payload).
    pub hash: Vec<u8>,
    /// 64-byte Ed25519 signature over `hash`.
    pub signature: Vec<u8>,
    pub signing_key_id: String,
    pub created_at: DateTime<Utc>,
}

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
            let bytes = fs::read(seed_path).map_err(Error::Io)?;
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
        let mut f = opts.open(seed_path).map_err(Error::Io)?;
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
        let payload_bytes = canonical_json(&event_payload);
        let mut hasher = Sha256::new();
        hasher.update(prev_hash);
        hasher.update(&payload_bytes);
        let hash_arr: [u8; HASH_LEN] = hasher.finalize().into();
        let sig: Signature = self.signing_key.sign(&hash_arr);
        Ok(Receipt {
            id: ReceiptId::new(),
            event_kind: event_kind.into(),
            event_payload,
            prev_hash: prev_hash.to_vec(),
            hash: hash_arr.to_vec(),
            signature: sig.to_bytes().to_vec(),
            signing_key_id: self.key_id.clone(),
            created_at: Utc::now(),
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
    let payload_bytes = canonical_json(&receipt.event_payload);
    let mut hasher = Sha256::new();
    hasher.update(&receipt.prev_hash);
    hasher.update(&payload_bytes);
    let recomputed: [u8; HASH_LEN] = hasher.finalize().into();
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

/// Verify a sequence of receipts. Each receipt's `prev_hash` must
/// equal the previous receipt's `hash`. The first receipt's `prev_hash`
/// is checked against the supplied `initial_prev` (zeros for genesis).
pub fn verify_chain(receipts: &[Receipt], vk: &VerifyingKey, initial_prev: &[u8]) -> Result<()> {
    if initial_prev.len() != HASH_LEN {
        return Err(Error::Validation("initial_prev wrong length".into()));
    }
    let mut expected_prev: Vec<u8> = initial_prev.to_vec();
    for (idx, r) in receipts.iter().enumerate() {
        if r.prev_hash != expected_prev {
            return Err(Error::Validation(format!(
                "chain break at index {idx}: prev_hash does not match previous receipt's hash"
            )));
        }
        verify_receipt(r, vk)?;
        expected_prev = r.hash.clone();
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
}
