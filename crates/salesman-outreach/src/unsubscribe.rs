//! RFC 8058 one-click unsubscribe token primitive.
//!
//! Mints a per-recipient URL of the form
//!
//!   {base_url}?t={base64url(email)}.{base64url(hmac_sha256(secret, email))}
//!
//! and verifies an inbound `t=` parameter, returning the email it was
//! bound to. The MAC compare is constant-time via `subtle`.
//!
//! BUG ASSUMPTION: `base_url` MUST be HTTPS in production. We do not
//! enforce that here (so dev/test can use http://localhost:...) but the
//! caller wires it from env config; the doctor command warns if scheme
//! is `http` and the host is not localhost.
//!
//! SECURITY: secret is held in `Zeroizing<Vec<u8>>` and zeroed on drop.
//! The token does not encode any state — it is purely a signed
//! envelope around the recipient address. A leaked token only allows
//! unsubscribing that one recipient, which is the worst-case the
//! adversary already wants to cause.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use salesman_core::{Error, Result};

type HmacSha256 = Hmac<Sha256>;

/// Minimum acceptable secret length (bytes). 32 = 256 bits, matching
/// the HMAC-SHA256 block-size floor recommended by RFC 4868.
pub const MIN_SECRET_BYTES: usize = 32;

/// Mints + verifies per-recipient RFC 8058 one-click unsubscribe URLs,
/// authenticated with an HMAC-SHA256 over the recipient address.
#[derive(Clone)]
pub struct UnsubscribeTokens {
    secret: Zeroizing<Vec<u8>>,
    base_url: String,
}

impl std::fmt::Debug for UnsubscribeTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnsubscribeTokens")
            .field("secret", &"<zeroized>")
            .field("base_url", &self.base_url)
            .finish()
    }
}

impl UnsubscribeTokens {
    /// Build from an explicit secret + base URL.
    pub fn new(secret: Vec<u8>, base_url: impl Into<String>) -> Result<Self> {
        if secret.len() < MIN_SECRET_BYTES {
            return Err(Error::Config(format!(
                "unsubscribe secret must be at least {MIN_SECRET_BYTES} bytes ({} given)",
                secret.len()
            )));
        }
        let base_url = base_url.into();
        if base_url.is_empty() {
            return Err(Error::Config("unsubscribe base_url is empty".into()));
        }
        Ok(Self {
            secret: Zeroizing::new(secret),
            base_url,
        })
    }

    /// Build from environment.
    /// - `SALESMAN_UNSUBSCRIBE_BASE_URL` (required, e.g. `https://outreach.plausiden.com/unsubscribe`)
    /// - `SALESMAN_UNSUBSCRIBE_HMAC_SECRET` (required, hex- or base64url-encoded ≥32 bytes)
    pub fn from_env() -> Result<Self> {
        let base_url = std::env::var("SALESMAN_UNSUBSCRIBE_BASE_URL")
            .map_err(|_| Error::Config("SALESMAN_UNSUBSCRIBE_BASE_URL not set".into()))?;
        let raw = std::env::var("SALESMAN_UNSUBSCRIBE_HMAC_SECRET")
            .map_err(|_| Error::Config("SALESMAN_UNSUBSCRIBE_HMAC_SECRET not set".into()))?;
        let secret = decode_secret(&raw)?;
        Self::new(secret, base_url)
    }

    /// The public base URL one-click unsubscribe links are minted
    /// against (e.g. `https://outreach.plausiden.com/unsubscribe`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Mint the full URL for a recipient. The recipient address is
    /// lower-cased for stable HMAC input — verification re-applies the
    /// same lower-case so trivial casing variation doesn't break the
    /// link.
    pub fn url_for(&self, email: &str) -> String {
        let token = self.token_for(email);
        // The base URL may already have a `?` (e.g. for analytics
        // params). Pick the right separator.
        let sep = if self.base_url.contains('?') {
            '&'
        } else {
            '?'
        };
        format!("{}{sep}t={token}", self.base_url)
    }

    /// Mint just the `t=` value — useful for tests or alternate URL
    /// shapes.
    pub fn token_for(&self, email: &str) -> String {
        let normalized = email.trim().to_ascii_lowercase();
        let email_b64 = URL_SAFE_NO_PAD.encode(normalized.as_bytes());
        let mac = self.mac(normalized.as_bytes());
        let mac_b64 = URL_SAFE_NO_PAD.encode(mac);
        format!("{email_b64}.{mac_b64}")
    }

    /// Verify a `t=` parameter. Returns the email if MAC checks out.
    /// Constant-time MAC compare; leaks only structure (b64 decode
    /// error vs. MAC mismatch) — both surface as the same `Error`.
    pub fn verify_token(&self, token: &str) -> Result<String> {
        let (email_b64, mac_b64) = token
            .split_once('.')
            .ok_or_else(|| Error::Validation("unsubscribe token: missing separator".into()))?;
        let email_bytes = URL_SAFE_NO_PAD
            .decode(email_b64.as_bytes())
            .map_err(|_| Error::Validation("unsubscribe token: invalid email b64".into()))?;
        let provided_mac = URL_SAFE_NO_PAD
            .decode(mac_b64.as_bytes())
            .map_err(|_| Error::Validation("unsubscribe token: invalid mac b64".into()))?;
        let email = std::str::from_utf8(&email_bytes)
            .map_err(|_| Error::Validation("unsubscribe token: email not utf-8".into()))?;
        // Re-normalize on verify side for the same casing safety net.
        let normalized = email.trim().to_ascii_lowercase();
        let expected_mac = self.mac(normalized.as_bytes());
        if expected_mac.ct_eq(&provided_mac).into() {
            Ok(normalized)
        } else {
            Err(Error::Validation("unsubscribe token: bad MAC".into()))
        }
    }

    fn mac(&self, msg: &[u8]) -> Vec<u8> {
        // SAFETY: HmacSha256::new_from_slice only fails on zero-length
        // keys, which we forbid in `new()` via MIN_SECRET_BYTES.
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).expect("HMAC accepts any non-empty key");
        mac.update(msg);
        mac.finalize().into_bytes().to_vec()
    }
}

fn decode_secret(raw: &str) -> Result<Vec<u8>> {
    let trimmed = raw.trim();
    // Hex first when the string looks unambiguously hex; otherwise
    // base64url. (A pure-hex string is also valid base64url but
    // decodes to garbage of a different length, so the order matters.)
    let looks_hex = !trimmed.is_empty()
        && trimmed.len().is_multiple_of(2)
        && trimmed.chars().all(|c| c.is_ascii_hexdigit());
    if looks_hex
        && let Ok(b) = hex::decode(trimmed)
        && b.len() >= MIN_SECRET_BYTES
    {
        return Ok(b);
    }
    if let Ok(b) = URL_SAFE_NO_PAD.decode(trimmed.as_bytes())
        && b.len() >= MIN_SECRET_BYTES
    {
        return Ok(b);
    }
    Err(Error::Config(format!(
        "SALESMAN_UNSUBSCRIBE_HMAC_SECRET must decode (hex or base64url) to ≥{MIN_SECRET_BYTES} bytes"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> UnsubscribeTokens {
        let secret = vec![0x42u8; 32];
        UnsubscribeTokens::new(secret, "https://example.test/unsub").unwrap()
    }

    #[test]
    fn round_trip_ascii() {
        let t = fixture();
        let tok = t.token_for("alice@example.com");
        let recovered = t.verify_token(&tok).unwrap();
        assert_eq!(recovered, "alice@example.com");
    }

    #[test]
    fn round_trip_unicode() {
        let t = fixture();
        let tok = t.token_for("Renée@münchen.de");
        let recovered = t.verify_token(&tok).unwrap();
        // ASCII lowercase is intentional — IDN punycoding is the
        // sender's responsibility before we ever see the address.
        assert_eq!(recovered, "renée@münchen.de");
    }

    #[test]
    fn case_insensitive_email() {
        let t = fixture();
        let upper = t.token_for("ALICE@example.com");
        let recovered = t.verify_token(&upper).unwrap();
        assert_eq!(recovered, "alice@example.com");
    }

    #[test]
    fn tampered_mac_rejected() {
        let t = fixture();
        let tok = t.token_for("alice@example.com");
        let mut bytes = tok.into_bytes();
        // Flip the last byte of the MAC; should fail.
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();
        assert!(t.verify_token(&tampered).is_err());
    }

    #[test]
    fn mismatched_email_rejected() {
        let t = fixture();
        // Mint for alice but slip in bob's b64 into the email half.
        let bob_b64 = URL_SAFE_NO_PAD.encode(b"bob@example.com");
        let alice_tok = t.token_for("alice@example.com");
        let alice_mac = alice_tok.split_once('.').unwrap().1;
        let forged = format!("{bob_b64}.{alice_mac}");
        assert!(t.verify_token(&forged).is_err());
    }

    #[test]
    fn missing_separator_rejected() {
        let t = fixture();
        assert!(t.verify_token("nodothere").is_err());
    }

    #[test]
    fn invalid_b64_rejected() {
        let t = fixture();
        assert!(t.verify_token("!!!.???").is_err());
    }

    #[test]
    fn short_secret_rejected() {
        let r = UnsubscribeTokens::new(vec![0u8; 16], "https://example.test/unsub");
        assert!(r.is_err());
    }

    #[test]
    fn empty_base_url_rejected() {
        let r = UnsubscribeTokens::new(vec![0u8; 32], "");
        assert!(r.is_err());
    }

    #[test]
    fn url_uses_correct_separator() {
        let t =
            UnsubscribeTokens::new(vec![0u8; 32], "https://example.test/u?campaign=42").unwrap();
        let url = t.url_for("alice@example.com");
        assert!(url.starts_with("https://example.test/u?campaign=42&t="));
    }

    #[test]
    fn decode_secret_hex_works() {
        let raw = hex::encode([0xabu8; 32]);
        let bytes = decode_secret(&raw).unwrap();
        assert_eq!(bytes, vec![0xabu8; 32]);
    }

    #[test]
    fn decode_secret_b64_works() {
        let raw = URL_SAFE_NO_PAD.encode([0xcdu8; 32]);
        let bytes = decode_secret(&raw).unwrap();
        assert_eq!(bytes, vec![0xcdu8; 32]);
    }

    #[test]
    fn decode_secret_too_short_rejected() {
        let raw = hex::encode([0u8; 16]);
        assert!(decode_secret(&raw).is_err());
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        // ASCII-printable email-like inputs. We don't bother with full
        // RFC 5322 — the contract is "any string we mint a token for,
        // we can verify". Empty is allowed because url_for() doesn't
        // refuse empty input today; verify_token returns whatever
        // the mint produced.
        #[test]
        fn round_trip_property(email in "[a-zA-Z0-9._+\\-]{1,64}@[a-zA-Z0-9.\\-]{1,32}") {
            let t = fixture();
            let tok = t.token_for(&email);
            let recovered = t.verify_token(&tok).unwrap();
            // We lower-case on mint, so compare lower-cased.
            prop_assert_eq!(recovered, email.to_ascii_lowercase());
        }

        // Verify NEVER panics on arbitrary input.
        #[test]
        fn verify_never_panics(s in ".{0,256}") {
            let t = fixture();
            let _ = t.verify_token(&s);
        }

        // Single-byte mutation of a valid token MUST cause verify to
        // fail (or, in the *extreme* coincidence of a colliding HMAC
        // under another input, return the wrong email — but never
        // accept the original email under the mutated token).
        // We test the strict claim: mutated → not Ok(original_email).
        #[test]
        fn one_byte_tamper_rejects(
            email in "[a-z][a-z0-9.]{0,16}@[a-z][a-z0-9.]{0,16}\\.[a-z]{2,4}",
            mut_idx in any::<usize>(),
        ) {
            let t = fixture();
            let tok = t.token_for(&email);
            let bytes = tok.as_bytes();
            let i = mut_idx % bytes.len();
            let mut mutated = bytes.to_vec();
            // XOR with 1 to flip the lowest bit. If the byte was a
            // separator/dot/equals etc. the result might still be
            // valid base64url (or even valid byte) — that's fine; the
            // test is on the OUTPUT, not the structure.
            mutated[i] ^= 1;
            let s = match String::from_utf8(mutated) {
                Ok(s) => s,
                Err(_) => return Ok(()), // mutation made it non-UTF8; not testable
            };
            let result = t.verify_token(&s);
            // Strict: if it verifies, it must NOT be the original
            // (lowercased) email. Same email + different MAC bytes is
            // a security failure.
            if let Ok(recovered) = result {
                prop_assert!(
                    recovered != email.to_ascii_lowercase()
                        || s == tok, // identity case if XOR self-cancelled (won't happen with ^=1)
                    "tampered token verified as original email",
                );
            }
        }
    }
}
