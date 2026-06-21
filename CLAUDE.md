# CLAUDE.md — PlausiDen-Salesman

## Mission
Get PlausiDen more clients. AI-driven market intelligence + cold sales
automation. Ship the smallest useful slice fast, layer defenses over time.

## Read first
0. `HANDOFF.md` — **current runtime + where we left off (READ THIS FIRST)**
1. `SCOPE.md` — current strawman + owner decision points
2. `ARCHITECTURE.md` — current shape (15-crate workspace layout)
3. PlausiDen-Meta operating principles
4. PlausiDen-AVP-Doctrine validation tier targets

## Stack
- Rust core (mirrors PlausiDen-Engine pattern)
- Postgres on the VPS (not SQLite — concurrent workers); Redis is a declared dependency, not yet wired; scheduling via systemd timers
- `lettre` for SMTP, `imap` for reply ingest
- Crawler (TS/Playwright) for web scraping
- Drafting/reply use SaaS Claude/Gemini, but prospect PII (email, phone, company name, homepage) is redacted before the call and rehydrated after (a redaction boundary; residual free-text names are an accepted v1 limitation). Local-only LFI is deferred — see ADR-0003 and `docs/PII_REDACTION_BOUNDARY.md`.

## Compute split
- **VPS (207.148.30.162, Debian 13 trixie)**: orchestrator + workers + Postgres (Redis declared but unused; scheduling via systemd timers). Re-provisioned 2026-05-31 — the old `45.77.217.37` is DEAD (see `HANDOFF.md`). Cohabits with the OpenClaw service.
- **Laptop**: dev environment + self-hosted CI runner.

## Hard rules
- No B2C outreach. B2B + opt-in only.
- No dark patterns (fake urgency, fake social proof, fake countdown).
- No selling or sharing scraped contact data.
- No auto-send without human review (until phase 0.3).
- No LinkedIn / X automation in v0 (TOS surface; opt-in later).
- Redact prospect PII (email, phone, company name, homepage) before any SaaS LLM call, rehydrate after — PII must not leave the box in the clear.

## Rate-limit defaults
- Per-recipient: 5 touches max per 30 days
- Per-domain: 10 sends max per hour
- Per-prospect: pause sequence on negative reply, alert owner

## Receipts + provenance
- Every send signed (Ed25519) + persisted
- Receipts replayable for audit
- Hidden but verifiable provenance tag in every message (custom header + footer link)

## Code standards
- Rust edition 2024
- `thiserror` for library errors. Never `unwrap()` in lib code.
- Every public function gets a `///` doc comment.
- 80% coverage minimum. `proptest` for invariants.
- No custom crypto. Use `ring`, `ed25519-dalek`, `chacha20poly1305`.
- Zeroize secrets. No secrets in logs.
- `tracing` for all logging.
- Dependencies: minimize. `cargo audit` before adding.

## Never
- Touch openclaw user data on the VPS (mode 700, off-limits)
- Run as root in production (use the dedicated `salesman` user)
- Send without a per-batch operator approval, until phase 0.3
- Post personal political beliefs in code or docs

## Frame as
"Civil rights tool for PlausiDen's go-to-market." Plausible deniability,
sovereign data, presumption of innocence. Avoid: "spam," "blast,"
"manipulate," "trick."
