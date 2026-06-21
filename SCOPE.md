# SCOPE.md — PlausiDen-Salesman

**Status:** v0 strawman locked to the *cold-sales-now* mission. Spoof-
training / compliance-testing dropped from v0 — those land later as
opt-in modes once the core sales engine is earning.

## Mission

Get PlausiDen more clients. **Build the supersociety stack of AI-driven
market intelligence + cold sales automation, ship the smallest useful
slice fast, layer defenses over time.**

## Operating constraints

- Owner needs revenue. Phase 0.1 must produce *useful sales output for
  real prospects* within days, not weeks.
- Sovereignty: drafting/reply use SaaS Claude/Gemini, but prospect PII
  (email, phone, company name, homepage) is redacted before the call and
  rehydrated after — a redaction boundary; residual free-text names are an
  accepted v1 limitation. Local-only LFI is deferred — see ADR-0003 and
  docs/PII_REDACTION_BOUNDARY.md.
- Compute split: the openclaw VPS (`207.148.30.162`; re-provisioned
  2026-05-31, old `45.77.217.37` retired — see HANDOFF.md) hosts the
  orchestrator + workers; laptop hosts dev + CI runners.
- AVP-2 doctrine applies — but pragmatically. Tier 1 by 0.1, Tier 3
  by 0.5. Don't gold-plate an unproven workflow.

## Phased delivery

| Phase | Deliverable | Slice goal | Done-when |
|---|---|---|---|
| **0.1** | **Draft generator** | CSV of company URLs in → personalized email drafts out (markdown). Owner reviews + sends manually. | First real send earns a reply. |
| **0.2** | **Pipeline state + tracker** | IMAP-poll + reply classification + per-prospect state machine (`new → contacted → engaged → qualified → won/lost`). | First reply auto-classified correctly. |
| **0.3** | **Sequencing + cadence** | Multi-touch sequences with throttling + per-recipient send-rate cap + automatic pause on negative reply. | First multi-touch sequence completes without trips. |
| **0.4** | **CRM integration** | PlausiDen-CRM (when built) is the system of record; Salesman writes pipeline events to it. Until then: local SQLite/Postgres. | CRM round-trip works (Salesman writes, CRM reads, dashboard renders). |
| **0.5** | **Adversarial-AI bench in CI** | Every template + every generated message runs through "is this AI?" detectors (origin-lens / GPTZero-style heuristics) before send is allowed. | CI fails when output is detector-positive. |
| **0.6+** | Spoof-training / compliance-testing modes (former v0 scope, deferred) | Per-campaign `intent` flag enables the dual-use path. | Separate decision. |

## Layered defenses (supersociety stack for outreach)

| Layer | Name | What it does | Who provides it |
|---|---|---|---|
| L1 | Multi-source discovery | No single dep on one data source | PlausiDen-Crawler + custom + APIs |
| L2 | Multi-stage enrichment | Deterministic data merging BEFORE any LLM inference | Custom Rust + LFI |
| L3 | Multi-template personalization | LFI primary, deterministic templates fallback, human-edit always supported | Custom + LFI |
| L4 | Per-recipient rate cap | Hard ceiling on touches per prospect per window | Orchestrator (Rust) |
| L5 | Per-domain rate cap | Don't burn deliverability with a single target domain | Orchestrator |
| L6 | Adversarial validation | Generated messages tested against "is this AI?" detectors in CI before send | CI gate |
| L7 | Cryptographic receipts | Every send signed + persisted; replayable + auditable | PlausiDen-Obs + signing key |
| L8 | Reply classification + auto-pause | Negative reply → pause sequence + alert owner | Custom + LFI classifier |
| L9 | Provenance tags | Hidden but verifiable origin marker in every message | Custom (DKIM extension) |
| L10 | RFC compliance | SPF + DKIM + DMARC + List-Unsubscribe + clear sender identity | DNS + mail config |

## Stack decisions

| Layer | Choice | Why |
|---|---|---|
| Core language | **Rust** | Mirrors PlausiDen ecosystem, AVP-2 alignment |
| Persistence | **PostgreSQL on VPS** | Concurrent workers + structured queries; SQLite would limit later phases |
| Scheduling | **systemd timers** | daily / classify / audit-chain / inbox-poll / doctor-watch units on the VPS; Redis is a declared-but-unused dependency, not wired |
| Mail send | **`lettre` crate** + direct SMTP | No SaaS dep; we control egress |
| Reply ingest | Custom `imap` poller | Native Rust IMAP exists |
| Web scraping | **PlausiDen-Crawler** (TS/Playwright) over a worker queue | Already exists |
| LLM/personalization | **LFI** (PlausiDen-AI) | Sovereignty requirement |
| Browser automation (LinkedIn etc) | Crawler + per-account isolation, opt-in only | TOS surface; default OFF |
| Container runtime | **Docker** (already on VPS) | Easy ops; isolates workers |
| Reverse proxy | **Caddy** (already on VPS) | TLS handled |

## VPS layout

```
/opt/salesman/
  ├── bin/                    # release binaries
  ├── data/                   # postgres data, runtime state
  ├── etc/                    # config (TOML, owned by salesman user)
  ├── log/                    # structured logs (PlausiDen-Obs format)
  └── docker-compose.yml      # postgres + orchestrator (Redis declared but unused)
```

Runs as a dedicated `salesman` system user (not root, not openclaw).
systemd unit `plausiden-salesman.service`. Fronted by Caddy at a
subdomain (TBD); admin UI behind basic-auth + IP allowlist.

## What's NOT in v0

- Auto-send without human review (until 0.3)
- LinkedIn / X automation (TOS surface, opt-in only later)
- B2C outreach (B2B + opt-in only)
- Dark patterns / fake urgency / fake social proof
- Re-targeting based on tracking pixels (anti-pattern + privacy violation)
- Selling / sharing harvested contact data

## Owner-decision points (please answer before 0.1 begins)

1. **First prospect list** — do you have one? CSV of company names + URLs + (optional) contact email is the minimum input for 0.1. If not, 0.1 needs to add a discovery step which roughly doubles its scope.
2. **Sender identity** — what email + display name should outreach come from? Need this to set up SPF/DKIM/DMARC on the relevant domain.
3. **Pitch template starting point** — do you have an existing email pitch we can use as the template seed? If not, I'll draft a generic PlausiDen pitch for you to edit.
4. **Phase 0.1 deadline** — "ASAP" or specific date?
5. **OpenClaw cohabitation** — confirm Salesman runs *alongside* the existing OpenClaw service on this VPS (not replacing it).

## Once 0.1 is locked

ARCHITECTURE.md is now current: it documents the real 15-crate workspace
layout and high-level shape. (The scope has since grown well past this
0.1-only framing — see PLAN.md for the larger autonomous-sales-engine plan.)
