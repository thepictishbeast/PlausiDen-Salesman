# PlausiDen-Salesman — Master Plan (v1, supersedes SCOPE 0.1-only roadmap)

> **Owner directive 2026-04-26 evening:**
> "We want a very capable sales agent that can make cold sales using
> [Claude] and Gemini to do so. It should be able to access tools and
> apps and scrap info and run OSINT operations, compare us to
> competitors and try to get us clients. Reach out to them and get us
> well known."

This document is the operative plan. The earlier `SCOPE.md` "draft
generator only" framing is now Phase 5 inside a much larger autonomous
sales engine.

## Vision in one paragraph

PlausiDen-Salesman is an autonomous AI sales engine. Given a market
focus ("cybersecurity SMBs in US/EU"), it discovers prospects, runs
OSINT to qualify them, compares PlausiDen's products against the
prospect's incumbents, drafts personalized outreach, runs that draft
past adversarial AI-detection, sends it across channels (email first,
LinkedIn / forms / DMs later), tracks replies, classifies them, and
either advances the prospect through the funnel or pauses with the
right reason. Simultaneously it builds PlausiDen's market presence by
generating comparison pages, case studies, and content targeted at the
same prospect segments. Two LLM backends — Claude (default for
reasoning) and Gemini (default for cheap bulk + grounding) — power
the agent loop, with LFI added later as a sovereignty option.

## Non-negotiable constraints

- **Owner-in-the-loop** for every first send to a new domain. Drafts
  show in a queue; owner approves or edits before the first contact
  with each new company. After domain is "owner-cleared", subsequent
  touches in the same sequence can auto-send subject to rate caps.
- **CAN-SPAM + GDPR + CASL compliance baked in.** Every outbound mail
  has a real PlausiDen physical address, working unsubscribe link,
  identifies sender accurately, no deceptive subject lines.
- **Hard rate caps** per recipient (max 4 touches in 30 days), per
  domain (max 8/day), per sender mailbox (per-mailbox configurable),
  per channel.
- **No AI-content shipping with detectability above threshold.** Every
  generated message is scored by an adversarial detector pipeline
  before it can be queued. Threshold is configurable but defaults to
  "low risk" — failures route back to the LLM for rewrite or to human
  edit.
- **All sends are crypto-receipted.** Hash + timestamp + content +
  recipient persisted to a Merkle log (PlausiDen-Obs format).
- **Kill switch.** A single `salesman-cli halt` command immediately
  pauses every active campaign and flushes outbound queues. Logged
  with reason.
- **Reply-driven ethics.** Negative reply, opt-out, or any phrase
  matching the "stop" classifier → instant suppression list, sequence
  pause, owner notified.

## Architecture

```
              ┌────────────────────────────────────────────┐
              │            salesman-orchestrator           │
              │  agentic loop: plan → act → observe →      │
              │  reflect → next                            │
              └─────┬──────────────────────────────────┬───┘
                    │                                  │
       ┌────────────▼──────────┐         ┌─────────────▼────────┐
       │     salesman-llm      │         │    salesman-tools    │
       │  multi-LLM router     │         │  registered actions  │
       │  - claude (default    │         │  - search engines    │
       │    reasoning)         │         │  - crawler RPC       │
       │  - gemini (bulk +     │         │  - osint adapters    │
       │    grounding)         │         │  - outreach senders  │
       │  - lfi (future,       │         │  - state queries     │
       │    sovereignty mode)  │         │  - content publisher │
       │  cost tracking,       │         │                      │
       │  retries, caching     │         │                      │
       └───────────┬───────────┘         └──────────┬───────────┘
                   │                                 │
                   └────────────┬────────────────────┘
                                │
                  ┌─────────────▼──────────────┐
                  │      salesman-state        │
                  │  Postgres (companies,      │
                  │  contacts, campaigns,      │
                  │  touches, replies,         │
                  │  signals, suppressions,    │
                  │  receipts)                 │
                  └────────────────────────────┘
```

Sub-systems are discrete crates so we can swap or harden each
independently (AVP-2 layering principle).

## Crate map

| Crate | Purpose |
|---|---|
| `salesman-core` | Shared types, error model, tracing setup, config loader |
| `salesman-state` | Postgres schema + migrations + typed queries (sqlx) |
| `salesman-llm` | Multi-LLM router. Backends: `claude`, `gemini`, future `lfi`. Uniform tool-use, prompt cache, cost ledger |
| `salesman-tools` | Tool trait + registry. Tools register declarative schemas the LLM can invoke |
| `salesman-discovery` | OSINT discovery: search APIs (SerpAPI / Brave Search / DDG), site enumeration, BuiltWith-style tech detection, public-data adapters |
| `salesman-osint` | Per-prospect intel: site scrape (via crawler RPC), people enrichment (Hunter, Apollo, Clearbit-class), social signal collection |
| `salesman-competitor` | Identify and characterize competitors per prospect; produce side-by-side feature/pricing maps; generate "why PlausiDen" angle |
| `salesman-content` | Brand content generator: comparison pages, case studies, SEO-targeted articles, LinkedIn posts |
| `salesman-outreach` | Multi-channel sender. Phase 1: SMTP via `lettre`. Phase 2: LinkedIn (opt-in, browser-automation). Phase 3: forms / X DMs |
| `salesman-reply` | IMAP/JMAP reply ingest, classification (engaged / objection / opt-out / OOO / bounce), state-machine update |
| `salesman-orchestrator` | The agentic loop. Coordinates planner → tools → reflect → next-step. One process per active campaign |
| `salesman-cli` | Human-ops surface: campaign create, dry-run, queue inspect, kill switch |
| `salesman-api` | HTTP API for the dashboard + CRM integration (axum) |
| `salesman-detector` | "Is this AI?" detector ensemble (called from outreach pre-send gate) |
| `salesman-receipts` | Crypto-receipt service (Merkle log of every send + every state change) |

## Phased delivery

| Phase | Deliverable | Feature gate |
|---|---|---|
| **1.0** | Workspace + crates skeleton + DB schema + LLM router with Claude+Gemini + tool trait + CLI bootstrap | `cargo build` succeeds end-to-end; `salesman-cli plan --dry-run` round-trips one fake prospect through the LLM with a fake tool |
| **1.1** | Discovery: real search-API adapter + crawler RPC + persist discovered companies | `salesman-cli discover --query "rust security smb US"` writes ≥10 companies to Postgres |
| **1.2** | OSINT enrichment: per-company scrape + tech-stack inference + people search | Same prospect now has `description`, `industry`, `tech_signals`, `key_people` fields populated |
| **1.3** | Competitor analysis: identify 3 incumbents + side-by-side angle | Generates `competitor_brief.md` per prospect |
| **1.4** | Outreach v1 (email): personalized draft + adversarial detector gate + owner approval queue + manual send | Owner reviews drafts in CLI/web, hits approve, message goes out via VPS SMTP, receipt persisted |
| **1.5** | Reply tracking: IMAP poll + classifier + state machine | First real reply auto-classified + funnel state advanced |
| **1.6** | Multi-touch sequencing with cadence + rate caps + auto-pause on negative | First multi-touch sequence completes without trips |
| **2.0** | Brand content engine: comparison page generator + case-study scaffolder + LinkedIn post drafts (still owner-approval-gated) | First comparison page (e.g. "PlausiDen Sentinel vs CrowdStrike Falcon for SMB") rendered + reviewed |
| **2.1** | Dashboard + API: web UI showing pipeline, campaigns, drafts, receipts | Owner can use it instead of CLI |
| **2.2** | LinkedIn outreach (opt-in, default OFF): browser-automation worker per linked account | First LinkedIn DM sequence runs without TOS-trip indicators |
| **2.3** | Web-form auto-fill: detect contact forms + submit personalized message | First form submission completes successfully |
| **2.4** | LFI integration as third LLM backend (sovereignty option) | Same prompts work routed to LFI |
| **2.5** | Cross-source competitor intel: pricing scrape, review aggregation (G2, Capterra, Trustpilot) | Per-competitor pricing + review summary in dashboard |
| **3.0** | Closed-loop optimization: multi-armed bandit over template variants, per-segment win-rate tracking | A/B winners auto-promoted; losers auto-retired |
| **3.1** | Adversarial bench: every template + every generated message tested against multiple AI detectors in CI | CI fails if detector positivity > threshold |

## LLM strategy

**Claude (default for reasoning):** planning, prospect qualification,
competitor angle generation, draft writing (cold + reply), reply
classification, content generation. Uses prompt caching aggressively.

**Gemini (default for bulk + grounding):** large-batch enrichment,
search-grounded answers (when we want recent web data without scraping
ourselves), bulk classification, cheap qualification pass.

**Routing rules (in `salesman-llm`):**

- "I need to think hard" → Claude Opus
- "I need to think but it's a normal task" → Claude Sonnet
- "I need a fast cheap classification" → Gemini Flash
- "I need search-grounded info" → Gemini Pro with grounding
- "I need bulk processing of 10k items" → Gemini Flash
- "Sovereignty mode active" → LFI (when integrated)

All backends implement the same `LlmBackend` trait. Tool-use is
abstracted to a uniform `ToolCall { name, args }` shape regardless of
which backend's native format is underneath.

## Tool inventory (the agent can invoke these)

- `search.web(q, limit)` — search engine query
- `search.linkedin_company(name)` — LinkedIn company lookup
- `crawler.scrape(url, depth)` — call PlausiDen-Crawler RPC
- `osint.email_for(person, company)` — Hunter / Apollo / pattern guess
- `osint.tech_stack(domain)` — tech detection via fingerprint patterns
- `competitor.find(domain)` — competitor identification
- `competitor.compare(plausiden_product, competitor)` — generate angle
- `state.get_company(id)` / `state.update_company(...)`
- `state.draft_send(prospect_id, body, subject)` — queues a send (gate)
- `state.list_replies(...)` — pull recent replies
- `outreach.send_email(...)` — only callable post-approval
- `content.publish_page(slug, content)` — push to website (gated)
- `receipt.log(event)` — append to Merkle log

Each tool registers a JSON Schema. The LLM router converts those into
Claude's `tools` format and Gemini's function declarations.

## Database schema (initial)

See `migrations/` (created in Phase 1.0). Top-level tables:
`companies`, `contacts`, `signals`, `competitors`, `campaigns`,
`prospects`, `touches`, `replies`, `suppressions`, `receipts`,
`llm_calls`, `tool_calls`.

## Repos this depends on

- **PlausiDen-Crawler** — RPC mode for `crawler.scrape` tool
- **PlausiDen-Obs** — receipt log storage + dashboard observability
- **PlausiDen-Meta** — schema definitions for inter-repo events
- **PlausiDen-AI** — future LFI backend integration
- **PlausiDen-Mail** (TBD) — sister repo for the inbound mail server
  + IMAP API; Salesman is a client

## Compliance + safety

- Suppression list is global (every domain that opts out, ever)
- Bounce-handling: hard bounce → suppress + alert; soft bounce → cooldown + retry
- Reply that contains stop keywords ("unsubscribe", "remove me", "no
  thanks", "stop") → instant suppress + pause sequence + owner notify
- Every outbound message carries `List-Unsubscribe` header (one-click
  per RFC 8058) + plain-text unsubscribe link in body
- DKIM, SPF, DMARC required to be passing on the sending domain before
  any send is allowed (Phase 1.4 startup check)
- No purchased email lists. Every contact is either OSINT-derived
  (public-source business contact) or owner-supplied
- All discovered personal email patterns flagged separately from
  generic role addresses (info@, sales@) — prefer role addresses for
  first contact unless owner explicitly opts into person-direct

## Open questions (deferred, not blocking)

1. Which search API (SerpAPI / Brave / Google Custom / Kagi)?
   → Decision: pluggable adapter, support multiple, default Brave
2. Which enrichment provider (Hunter, Apollo, Clearbit alt)?
   → Decision: pluggable, start with Hunter (good free tier)
3. Where's the inbound mail server?
   → Decision: VPS + simple Postfix + Dovecot in Phase 1.5
4. What domain for sending?
   → Owner decision still pending (was blocker #2 in original SCOPE)
5. LinkedIn automation — owner risk tolerance?
   → Defer to Phase 2.2; default OFF; opt-in per-account

## What "well known" means

The brand-building track (Phase 2.0+) targets:

- High-quality comparison pages indexable on Google for
  "PlausiDen X vs CompetitorY" searches the prospect actually does
- Case studies anchored to verifiable claims (real customer wins)
- Active LinkedIn presence (long-form posts, not just promotions)
- HN / Reddit / niche-forum participation (substance-first, no spam)
- Open-source visibility on the existing PlausiDen repos (already
  active — Salesman amplifies it)
- Newsletter (low-frequency, high-signal) for warm leads

## Status as of 2026-04-26

- VPS infra ready (Postgres + Redis + `/opt/salesman` + key auth)
- Empty crates `crates/salesman-cli` + `crates/salesman-core` exist
  as stubs; no Cargo.toml at workspace root yet
- This PLAN.md kicks off Phase 1.0
