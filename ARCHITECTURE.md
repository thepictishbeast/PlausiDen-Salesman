# ARCHITECTURE.md — PlausiDen-Salesman

> Stub. Rewritten when phase 0.1 scope is locked (see SCOPE.md owner-decision points).

## High-level (planned)

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
         │  Postgres (state) + Redis (queue)          │
         └────────────────────────────────────────────┘

         ┌────────────────────────────────────────────┐
         │  Caddy → admin UI (basic-auth, IP allow)   │
         └────────────────────────────────────────────┘
```

## Crate layout (planned)

```
crates/
  salesman-core/         # types, traits, state machine, signing
  salesman-orchestrator/ # main daemon, job dispatcher
  salesman-discovery/    # lead-source adapters (crawler, APIs)
  salesman-enrichment/   # deterministic data merging
  salesman-generator/    # LFI + template integration
  salesman-delivery/     # lettre wrapper + provenance tagging
  salesman-reply/        # IMAP poller + classifier
  salesman-cli/          # operator interface
  salesman-admin-ui/     # web UI (axum + leptos? — TBD)
```

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
