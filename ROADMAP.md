# PlausiDen-Salesman — Execution Roadmap

> Companion to `PLAN.md`. PLAN describes the *system*; ROADMAP
> describes the *order of work* and the gating criteria between
> phases. Tracked as Claude task IDs — when a task ships, mark it
> done in the conversation's task list.

## Tier 0 — Foundation **(SHIPPED)**

| | What | Status |
|---|---|---|
| 1.0  | Workspace + 15 crates + types + LLM trait + agent loop | ✅ commit `bcfde5c` |
| 1.0.5| Real Claude + Gemini wire formats + CLI auto-registers | ✅ commit `8b3e95e` |
| 1.1  | Discovery (CSV + homepage) + state batch ops + CLI | ✅ commit `fc701bb` |
| 1.2  | LLM-driven cold-email drafting + owner review queue | ✅ commit `4da2e39` |
| 1.3a | salesman-receipts crate (Ed25519 + hash chain)        | ✅ in-flight commit |
| 1.3b | salesman-outreach crate (SMTP via lettre + RFC 8058)  | ✅ in-flight commit |

## P0 — First real send (10 tasks, IDs 242–251)

End state: an operator can run `salesman send-pending --campaign foo --for-real` and a real signed-and-receipted email leaves the VPS, with suppression and rate-cap enforcement.

| # | Task | Touches |
|---|---|---|
| 242 | state: insert_receipt + get_last_hash + list_receipts | salesman-state |
| 243 | state: approve_touch + mark_touch_sent + reject_touch | salesman-state |
| 244 | state: is_suppressed + add_suppression + list_suppressions | salesman-state |
| 245 | CLI: approve --touch / reject --touch | salesman-cli |
| 246 | CLI: suppress --email/--domain --reason | salesman-cli |
| 247 | CLI: send-pending (default dry-run, --for-real to send) | salesman-cli |
| 248 | SmtpSender: pre-flight suppression + rate-cap checks | salesman-outreach |
| 249 | Receipt logging on send (chain continuation) | salesman-cli + receipts |
| 250 | VPS Postgres: install citext + pgcrypto extensions | ops (VPS shell) |
| 251 | Run salesman migrate against VPS Postgres | ops (VPS shell) |

**Gate to P1:** one real send completes end-to-end + receipt verifies + appears as sent in `salesman review`.

## P1 — Defenses + replies + sequencing (9 tasks, IDs 252–260)

| # | Task |
|---|---|
| 252 | salesman-detector: heuristic ensemble (cliché, banned phrases, em-dash density, etc.) |
| 253 | Pre-send detector gate (block approve if score > threshold; --force-override available) |
| 254 | Detector unit tests with good/bad sample corpus |
| 255 | salesman-reply: IMAP poller (async-imap, IDLE) |
| 256 | LLM reply classifier → ReplyKind (Bulk hint = Gemini Flash) |
| 257 | Reply → FunnelState transitions + auto-suppress on opt-out |
| 258 | CLI: inbox --campaign |
| 259 | Multi-touch sequence schema + state ops |
| 260 | Rate-cap enforcement layer (per-recipient, per-domain, per-mailbox) |

**Gate to P2:** a sent email triggers a real classified reply that advances funnel state correctly + at least one auto-suppression triggered by an opt-out reply.

## P2 — Brand building + OSINT extensions + production deploy (8 tasks, IDs 261–268)

| # | Task |
|---|---|
| 261 | ComparisonPageTool (LLM-driven brand-building) |
| 262 | CaseStudyDraftTool (template + fill) |
| 263 | SEO meta-tag generator for marketing pages |
| 264 | Brave Search API adapter (with quota tracking) |
| 265 | Email-pattern guesser tool |
| 266 | LinkedIn company-page read-only scraper (polite) |
| 267 | Cross-compile salesman binary (musl) + ship to VPS |
| 268 | systemd unit for salesman-orchestrator on VPS |

**Gate to P3:** salesman runs as a systemd-managed service on VPS + at least 3 owner-approved comparison pages live + Brave Search adapter has produced ≥1 fresh discovered company batch.

## P3 — Quality + observability (3 tasks, IDs 269–271)

| # | Task |
|---|---|
| 269 | Real-Postgres integration test (testcontainers preferred) |
| 270 | Property tests for FunnelState transitions |
| 271 | Daily email summary cron (state of the pipeline) |

**Gate to P4 (future):** AVP-2 Tier 1–3 coverage + zero `unwrap()` in lib code.

## Out-of-scope this roadmap (future P4+)

- Web dashboard (axum API) — adds operator UI, but CLI is fine to ship first
- LinkedIn DM automation (write-side) — TOS surface, opt-in only
- Web-form auto-fill — needs Crawler RPC, scope creep
- LFI as third backend — sovereignty option, after first revenue
- Closed-loop optimization (multi-armed bandit, A/B promotion)
- Multi-mailbox sender pool (deliverability optimization)
- DKIM/DMARC operator runbook (one-pager, included in deploy notes when shipping)

## Owner-decision gates (still open — do NOT block code work)

| | Decision | Why blocking |
|---|---|---|
| OD-1 | First prospect list (CSV) | First `discover` needs real data |
| OD-2 | Sender identity (email + display name + domain) | DKIM setup + SmtpConfig |
| OD-3 | Pitch template seed | First draft has a quality floor |
| OD-4 | Phase 0.1 deadline | Owner-side prioritization signal |
| OD-5 | Salesman cohabits with OpenClaw on VPS — confirm | Already true; just confirm |

The 30 tasks above can ship without these decisions. The decisions block only the **first real send**.

## Operating principles

1. **Owner-in-the-loop until phase 1.6.** No auto-send to a new domain.
2. **Receipts on every state-changing event.** Tamper-evident audit log.
3. **Hard rate caps.** Better to under-send than to burn a domain.
4. **Reply-driven ethics.** Any opt-out signal → instant suppression.
5. **Compliance (CAN-SPAM, GDPR, CASL) baked in.** Not a postscript.
6. **No auto-send anything detectable as AI** above the configured
   detector threshold.
7. **No B2C, no purchased lists, no dark patterns.**
