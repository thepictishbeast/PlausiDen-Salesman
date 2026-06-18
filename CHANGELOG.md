# Changelog

All notable changes to PlausiDen-Salesman documented here.

> Status note (2026-06-02): this changelog had drifted badly — it still
> described the repo as "v0.0.0 pre-implementation" long after Tier 0
> shipped. Corrected below from `ROADMAP.md` + `git log`. Nothing has been
> released, and **no real send has ever executed** — first-real-send is
> owner-gated (see `OWNER_BLOCKERS.md`) and no AVP SHIP-DECISION has been
> reached.

## [Unreleased]

### Added — Tier 0 Foundation (on `main`, per ROADMAP.md)
- 15-crate Cargo workspace; domain model + `FunnelState` machine + typed IDs
- Multi-LLM router (Claude + Gemini wire formats) + subscriber-CLI transport
- Discovery (CSV seed + homepage/tech-signal) + OSINT adapters + state batch ops
- LLM cold-email drafting with in-loop AI-tell scoring + owner review queue
- `salesman-receipts` — Ed25519 + SHA-256 hash-chain signing/verification
- `salesman-outreach` — SMTP via `lettre` + RFC 8058 one-click unsubscribe
- `salesman-reply` — IMAP + RFC 8601 Authentication-Results anti-spoof + DSN
- `salesman-api` — Axum: pipeline summary, draft approval, receipts, unsubscribe
- `salesman-cli` — operator binary (40+ subcommands; send is dry-run by default)
- Repo scaffold; SCOPE / ARCHITECTURE / CLAUDE / CONTRIBUTING / SECURITY;
  `.github/workflows/ci.yml` (self-hosted runner); `integrations/avp.toml`

### Added — branch `claude/hardening-2026-06-02` (not yet merged)
- `deny.toml` supply-chain policy; `cargo deny` now passes (was unconfigured,
  so it had been rejecting every license)
- Patched `lettre` 0.11.21 → 0.11.22 (clears RUSTSEC-2026-0141)
- API `/campaigns` + `/drafts` implemented (were documented stubs)

### Status
- Pre-release. Tier 0 is code-complete; **first real send remains
  owner-blocked** (B4.5b unsubscribe reverse-proxy, B6 template voice).
- One open advisory awaiting owner risk-acceptance: RUSTSEC-2023-0071
  (`rsa`, medium, no upstream fix; unreachable in this Postgres-only build).
- Build requires rustc ≥ 1.89 (the workspace lockfile pins newer deps).
