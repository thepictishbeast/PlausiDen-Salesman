# Telemetry & observability

Salesman is a reputation- and compliance-sensitive system that sends real email
on PlausiDen's behalf. We need to know **exactly what it did, why, and how** —
both in development and at runtime. Observability is a first-class requirement,
not an afterthought.

**Hard rule:** telemetry must never contain PII or secrets. Log counts, ids,
booleans, model/backend names — never raw prospect data or credentials. (The
redaction-boundary telemetry logs a *count* + backend, never the values;
secrets are zeroized + redacted in `Debug`; see
[PII_REDACTION_BOUNDARY.md](PII_REDACTION_BOUNDARY.md), [SECURITY.md](../SECURITY.md).)

## The signals

### Structured logs (`tracing`)
All logging is `tracing`. The CLI initializes `tracing_subscriber::fmt` with an
`EnvFilter` (control via `RUST_LOG`; default `info`). Decision paths emit
events — e.g. the cold-draft path logs the redaction count + backend/model and
warns on a surviving placeholder; the LLM router logs fallbacks.

### LLM cost ledger (`llm_calls` table)
Every inference call is persisted to `llm_calls` with backend, model, purpose,
token counts, latency, and `cost_micro_usd`. Query it:
- `salesman costs [--by model|purpose] [--since-hours N]` — spend breakdown.
- `salesman campaign-costs [--since-hours N]` — per-campaign spend vs cap.
  **Caveat:** per-campaign attribution depends on `related_id`/`related_kind`
  tagging on `llm_calls`, which is NOT yet wired (same follow-up as
  [MODEL_RESILIENCE.md](MODEL_RESILIENCE.md) §1), so per-campaign `calls`/`spent`
  currently read 0.

### Provenance on every artifact (`produced_by`)
Drafts/replies record `produced_by = { backend, model, via_fallback, purpose }`
on the touch row, so any generated artifact traces back to the exact model and
whether a fallback was used.

### Receipts / audit chain
Every state-changing event is signed into the receipt chain (proof of what the
system did). Verify with `salesman audit` / `salesman audit-chain`. See
[AUDIT_CHAIN.md](AUDIT_CHAIN.md).

### Ops surfaces
- `salesman summary [--since-hours N]` — 24h triage banner (sends, drafts,
  replies, spend).
- `salesman status` — current pipeline state.
- `salesman doctor` — preflight health with a `VERDICT: RED|YELLOW|GREEN` line;
  `scripts/salesman-doctor-watch.sh` polls it and fires the alert webhook on the
  transition to GREEN.
- `salesman next-best-actions` — what the operator should do next.

### Alerting
`SALESMAN_ALERT_WEBHOOK_URL` (Slack/Discord/etc., shape auto-detected by host)
receives operational alerts; the daily run (`scripts/salesman-daily.sh`) emits a
summary.

## Principle: regressions must be visible

Quality/cost/deliverability regressions must show up in the signals above
(detector scores in the review queue, cost ledger, doctor verdict, daily
summary) rather than failing silently — see
[MODEL_RESILIENCE.md](MODEL_RESILIENCE.md).

## Follow-ups

- A consolidated metrics export (Prometheus/OTel) is not yet wired; today
  observability is via the Postgres ledgers + CLI + logs + webhook.
- Wire an external/off-box anchor for the audit chain (see AUDIT_CHAIN.md) so
  truncation is observable, not just tamper of existing rows.
