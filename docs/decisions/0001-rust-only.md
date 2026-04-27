# 0001 — Rust as the only implementation language (decided)

## Context

PlausiDen-Salesman could plausibly be written in Python (faster
to-market, more LLM ecosystem) or Go (decent middle ground) or Rust
(slowest to-market, sharpest types + zero-cost abstractions +
cargo). The PlausiDen ecosystem repos already standardise on Rust
(per CLAUDE.md). New repos that diverge create cross-repo friction
in shared types, error model, AVP-2 audit pipeline, and operator
muscle-memory.

The owner-stated AVP-2 doctrine treats every component as an
adversarial-validated artefact; Rust's borrow checker, exhaustive
match, and `#![forbid(unsafe_code)]` are the cheapest
specification-by-types we have.

## Decision

We will use Rust 2024 edition for every Salesman crate. No Python,
no Go, no JavaScript even for tooling. Where we genuinely need a
non-Rust component (Postfix, Caddy, opendkim) we treat it as a
configured external dependency, not an embedded subsystem.

## Consequences

- ✅ Shared types + error model + cargo workspace across PlausiDen
- ✅ Compile-time enforcement of state-machine and tool-call shapes
- ✅ One `cargo test` runs the whole quality gate
- ⚠️  Slower iteration on quick scripts. We accept this — quick
   scripts that need to live become Rust binaries.
- ❌ We do not ship a Python SDK for third-party integration. Use
   the HTTP API.

## Alternatives considered

- **Python with FastAPI + SQLAlchemy** — fast to-market, broadest
  LLM ecosystem; lost on type safety + cross-repo consistency +
  AVP-2 doctrine alignment.
- **Go** — solid type story, fewer footguns than Python; lost on
  expressiveness (no enum-with-data, no real ADTs).

## Status

`decided 2026-04-26 by claude-code session`

## References

- `CLAUDE.md` (operating principles)
- `AVP2_SUPERSOCIETY_PROTOCOL.md` (doctrine)
- All other PlausiDen repos
