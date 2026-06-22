# PlausiDen-Salesman — to-do for later

> Hidden from the active task list 2026-05-04 to declutter while
> Forge work runs. Re-add to TaskList when revisiting. Order
> below matches priority.

## Owner-blocked / pending

- **N1** — integrate Originality.ai (or GPTZero) AI-text detector API
  (an EXTERNAL-API detector; distinct from — and additive to — the
  shipped in-house heuristic `salesman-detector`, which already flags
  AI-tell phrasing at draft time)
- **O1** — scaffold PlausiDen-Mail repo + workspace
- **O2** — Postfix + Dovecot config templates
- **O3** — IMAP IDLE bridge → Salesman API webhook
- **O4** — per-campaign mailbox provisioning script
- **R3** — register sender domain with Postmaster Tools + SNDS
- **R4** — first-real-send dry-run + go-live checklist + execute

## Already resolved (kept here for context)

- ~~**R1**~~ — SPF + DKIM + DMARC records on outreach.plausiden.com (resolved 2026-05-01)
- ~~**R2**~~ — first 25-prospect warm-up campaign config (resolved 2026-05-01)

## How to revive

When ready to resume, batch-create from this file:

```bash
# Manual: open this file, copy each bullet, run TaskCreate via Claude.
# Or write a small script that parses the bullets and calls the API.
```

## See also

- `OWNER_BLOCKERS.md` — same content, owner-facing framing
- `CLAUDE.md` — doctrine
- `docs/SUBSCRIBER_LOGIN.md` — Path B LLM auth (resolved 2026-05-01)
