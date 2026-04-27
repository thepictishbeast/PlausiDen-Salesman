# 0004 — Owner-in-the-loop is the default; auto-send is opt-in
> (decided)

## Context

A cold-sales engine that auto-sends without human approval is one
mistake away from a domain-blocklist + a CAN-SPAM violation +
reputation damage that takes months to recover from. CrowdStrike's
2024 incident, Mailchimp's recurring blocklist flaps, and any
number of post-mortems on AI-generated outreach gone wrong all
agree: the cost of a bad batch is asymmetric to the cost of a
human-review step.

Salesman generates drafts via LLM. LLM output is non-deterministic
even with seeded prompts. A first-touch to a new domain is
high-risk; a follow-up to an already-engaged prospect is lower-risk.

## Decision

Every draft lands in the `awaiting_approval` queue. The default
path requires explicit operator approval (`salesman approve --touch
<id>`) before a touch can transition to `approved`, after which
`send-pending --for-real` sends it. The `--for-real` flag is itself
an explicit gate; the default of `send-pending` is dry-run.

The detector gate runs BEFORE approval is even possible — drafts
scoring above the threshold cannot be approved without explicit
`--force-override "<reason>"` from the operator.

## Consequences

- ✅ No batch leaves the system without human eyes on at least one
  message (the first-touch to a new domain).
- ✅ The `--for-real` flag means even a misfired script can't send.
- ⚠️  Throughput cap — an operator's review time is the bottleneck.
  Acceptable: we'd rather under-send than burn deliverability.
- ⚠️  When auto-send for established sequences lands (Phase 1.6+),
  the owner-in-loop guarantee narrows to "first touch only."

## Alternatives considered

- **Auto-send everything** — fastest throughput; lost because of
  the asymmetric cost of one bad batch.
- **Auto-send when detector passes** — sounds reasonable; lost
  because the detector is a heuristic, not a classifier. False
  negatives happen.

## Status

`decided 2026-04-26 by claude-code session`

## References

- `crates/salesman-cli/src/main.rs` (Approve / SendPending / detector gate)
- `crates/salesman-detector/src/lib.rs` (heuristic ensemble)
- `CLAUDE.md` Hard rules ("No auto-send without human review until phase 0.3")
