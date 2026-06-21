# ARCHITECTURE.md — PlausiDen-Salesman

## High-level

```
                  ┌─────────────────────────────────┐
                  │  salesman-orchestrator (Rust)   │
                  │  — schedules + dispatches jobs  │
                  └─────────────────────────────────┘
       ┌──────────────┬──────────┬───────────┬──────────────┐
       │              │          │           │              │
   discovery      enrichment   gen        delivery       reply-ingest
   ──────────    ──────────   ─────       ────────       ───────────
   Crawler        merge       LFI          lettre        imap-poll
   (TS/Playwright) public      (sovereign) +SMTP        + classifier
                  + APIs       templates   provenance    + state machine
                                          tags

         ┌────────────────────────────────────────────┐
         │  Postgres (state) + systemd timers (sched) │
         └────────────────────────────────────────────┘

         ┌────────────────────────────────────────────┐
         │  Caddy → admin UI (basic-auth, IP allow)   │
         └────────────────────────────────────────────┘
```

## Crate layout

15 workspace crates, each a hardenable boundary (AVP-2 layering):

```
crates/
  salesman-core/         # shared types, error model, tracing setup, config loader
  salesman-state/        # Postgres schema + migrations + typed queries (sqlx)
  salesman-llm/          # multi-LLM router (claude, gemini, future lfi); tool-use, cache, cost ledger
  salesman-tools/        # tool trait + registry; tools register declarative schemas the LLM invokes
  salesman-discovery/    # OSINT discovery: search APIs, site enumeration, tech detection, public-data adapters
  salesman-osint/        # per-prospect intel: site scrape (crawler RPC), people enrichment, social signals
  salesman-competitor/   # identify + characterize competitors per prospect; side-by-side angle
  salesman-content/      # brand content: comparison pages, case studies, SEO articles, LinkedIn posts
  salesman-outreach/     # multi-channel sender; phase 1 SMTP via lettre + provenance tagging
  salesman-reply/        # IMAP reply ingest + classifier + state-machine update
  salesman-orchestrator/ # the agentic loop: plan → act → observe → reflect → next
  salesman-cli/          # operator surface: campaign create, dry-run, queue inspect, kill switch
  salesman-api/          # HTTP API for the dashboard + CRM integration (axum)
  salesman-detector/     # "is this AI?" detector ensemble (outreach pre-send gate)
  salesman-receipts/     # crypto-receipt service (Merkle log of every send + state change)
```

Rough mapping from the earlier planned names: `enrichment` split into
`discovery` + `osint`; `generator` became `content`; `delivery` became
`outreach`; `admin-ui` became `api`. Scheduling is done with systemd
timers (`deploy/systemd/salesman-{daily,classify,audit-chain,inbox-poll,doctor-watch}.timer`),
not a queue — there is no Redis in any crate.

## Sequence (planned, 0.1 happy path)

```
operator: salesman-cli draft --input prospects.csv --pitch pitch.md
       ↓
orchestrator: enqueue N discovery jobs
       ↓
discovery worker: scrape each URL via Crawler
       ↓
enrichment worker: merge scrape + cached data
       ↓
generator worker: LFI personalization → markdown draft per prospect
       ↓
operator: review drafts/, edit as needed, run send-batch
       ↓
delivery worker: lettre + SMTP, tagged with provenance + signed receipt
       ↓
reply-ingest (continuous): IMAP poll → classifier → state update
       ↓
operator: salesman-cli pipeline → see who's where in the funnel
```

## To be filled in

- [ ] Concrete data model (prospect, touch, sequence, receipt)
- [ ] Postgres schema + migration strategy
- [ ] LFI invocation contract (gRPC? HTTP? local crate?)
- [ ] Reply-classifier model: heuristics first, LFI later
- [ ] Provenance-tag format (custom DKIM header? subject prefix? footer link?)
- [ ] Receipt-signing key management
- [ ] Backup / restore (Postgres dumps to where?)
- [ ] Disaster recovery scenario (VPS dies — how fast can we restore?)
