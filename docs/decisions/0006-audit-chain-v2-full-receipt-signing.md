# 0006 — Audit-chain v2: sign the full receipt

Date: 2026-06-18
Status: Accepted

## Context

The receipt chain (`salesman-receipts`) signed only `SHA-256(prev_hash ||
canonical_payload)`. A pre-merge audit found that `event_kind`, `created_at`,
`id`, and `signing_key_id` were stored but **not** covered by the signature, so
they could be altered after the fact without invalidating it. Separately,
`verify_chain` did not enforce that a chain is single-key (the module doc
claimed it did), and end-of-chain truncation / full-table deletion were
undetectable from the receipts alone.

This was caught pre-first-send: no real receipts exist yet (R4 first-real-send
is still an owner blocker), so there is no production data to migrate.

## Decision

Adopt **scheme v2**: the Ed25519 signature authenticates the *entire* receipt.
`signing_hash()` hashes the canonical JSON of `{v, id, event_kind,
signing_key_id, created_at, prev_hash, payload}`; both `sign_event` and
`verify_receipt` use it. `created_at` is encoded at microsecond precision to
survive the Postgres `timestamptz` round-trip.

`verify_chain` now enforces per-key scoping. A new `verify_chain_anchored`
accepts an optional trusted `expected_head` / `expected_count` to detect
truncation and deletion.

A `"v":2` marker is embedded in the preimage so a future scheme change can
dispatch by version instead of hard-breaking existing receipts.

## Consequences

- Tamper-evidence now covers every receipt field, closing the audit finding.
- Because `id` and `created_at` are per-receipt, two receipts never share a
  hash even with identical payloads (the old key-order test was reworked to
  test `canonical_json` directly).
- Truncation/deletion detection requires an **off-box / append-only** anchor;
  an anchor in the same DB gives no guarantee. The capability ships now; wiring
  a real external anchor into the state layer is a tracked follow-up (see
  docs/AUDIT_CHAIN.md). Until then `verify_chain` (no anchor) cannot detect a
  cleanly-truncated tail — documented, not hidden.
- Superseded the v1 signing detail in the `salesman-receipts` module docs,
  which now describe scheme v2 only.
