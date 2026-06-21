# Audit chain (receipts)

Every state-changing event (send, approval, suppression, owner-notification)
is recorded as a signed **receipt** in an append-only hash chain. This is the
system's proof-of-what-it-did, for compliance and dispute resolution.

Implementation: `crates/salesman-receipts/src/lib.rs`. Storage: Postgres
(`receipts` table; hashes/signatures as `BYTEA`, `created_at` as `timestamptz`).

## What a receipt is

```
id              ReceiptId (uuid)         stable id
event_kind      String                   "send.email", "suppression", ...
event_payload   JSON                     the event details
prev_hash       32 bytes                 hash of the previous receipt (zeros = genesis)
hash            32 bytes                 SHA-256 of the canonical full-receipt preimage (v2)
signature       64 bytes                 Ed25519 over `hash`
signing_key_id  String                   which key signed it
created_at      timestamptz (µs)         when it was signed
```

## Signing scheme — v2 (full-receipt authentication)

The signature authenticates the **entire receipt**, not just the payload.
`signing_hash()` builds a deterministic preimage as the canonical JSON of:

```
{ "v":2, "id", "event_kind", "signing_key_id",
  "created_at" (RFC3339, microseconds), "prev_hash" (hex), "payload" }
```

and the Ed25519 signature is over `SHA-256(preimage)`. Consequences:

- Mutating **any** field — `event_kind`, `created_at`, `id`, `signing_key_id`,
  `prev_hash`, or `payload` — invalidates the signature. (v1 signed only
  `prev_hash || payload`, leaving the other fields forgeable; that is fixed.)
- `created_at` is hashed at **microsecond** precision so a receipt still
  verifies after the Postgres `timestamptz` round-trip (which truncates
  sub-microsecond digits). This is exercised by the DB integration test.
- Canonical JSON (serde_json, no `preserve_order`) sorts keys, so the hash is
  stable across platforms and key-insertion order.

> **Migration:** v2 was adopted pre-first-send, when no real receipts existed,
> so there is no v1 data to migrate. If you ever change the preimage again
> *after* receipts exist, add a `v` dispatch in `signing_hash`/`verify_receipt`
> so old receipts keep verifying. The `"v":2` marker is already in the preimage
> for exactly this.

## Verification

- `verify_receipt(receipt, vk)` — recomputes `signing_hash` and checks the
  Ed25519 signature. Rejects on any field mismatch or bad length.
- `verify_chain(receipts, vk, initial_prev)` — walks `prev_hash` linkage
  (first must equal `initial_prev`; genesis = 32 zero bytes), verifies each
  receipt, and enforces **per-key scoping**: it rejects a chain whose
  `signing_key_id` changes mid-stream. Key rotation ⇒ start a new chain.

### Truncation / deletion and the anchor requirement

A hash chain alone **cannot** detect that the *most recent* receipts were
deleted, or that the whole table was emptied — a cleanly-truncated chain still
links and verifies. Detecting that requires knowing what the head *should* be.

`verify_chain_anchored(receipts, vk, initial_prev, expected_head, expected_count)`
closes the gap: given a trusted `expected_head` (the latest receipt's `hash`)
and/or `expected_count`, it flags truncation, deletion, and insertion.

**Threat model — read this before relying on it:** the anchor only helps if it
lives somewhere the attacker who can edit the `receipts` table **cannot also
edit**. An anchor stored in the same Postgres provides *no* guarantee (delete
the receipts, update the anchor). A real anchor must be **off-box and/or
append-only** — e.g. periodic head+count shipped to an external append-only log
or a second-party witness.

> **Follow-up (not yet wired):** the state layer does not yet persist/check an
> external anchor — `verify_chain_anchored` is available but callers pass
> `None`. Wiring a real off-box anchor (and the concurrency/ordering around the
> chain head) is tracked separately; half-building it in the same DB would give
> false assurance, so it was deliberately deferred.

## CLI

- `salesman audit` — verify each receipt's signature (OK/FAIL per row).
- `salesman audit-chain` — walk the full chain (linkage + per-receipt verify).

## Genesis & rotation

- Genesis receipt's `prev_hash` is 32 zero bytes (`zero_hash()`).
- The signing seed is a 32-byte file, mode 0600 (`Signer::load_or_generate`),
  held in memory in `Zeroizing`. Back it up. Rotating the key starts a new
  per-key chain.

See also: [SECURITY.md](../SECURITY.md), ADR-0006 (audit-chain v2),
[HUMAN_IN_THE_LOOP.md](../HUMAN_IN_THE_LOOP.md).
