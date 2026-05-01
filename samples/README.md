# Sample inputs

Templates the operator can copy + edit before ingesting.

## prospects-warmup-template.csv  *(headers only — fill this in)*

The actual file you fill with real prospects. Header row only by design
— so a stale copy can't be accidentally `import-csv`d into a real
campaign and try to send to `.example` addresses.

Open it, paste your real prospects below the header. **Required:**
`display_name`. **Optional but recommended:** `homepage`, `industry`,
`region`, `description`, `legal_name`, `size_band`.

## prospects-warmup-EXAMPLE.csv  *(reference only — DO NOT IMPORT)*

Five fictional rows showing every supported column. All hostnames use
`.example` (RFC 2606 reserved) so a slip-up that imports this file
will be caught by `salesman dns-check` / homepage validation, NOT by
sending real email to fictional people.

## What "warmup" means

The first 25 prospects in a brand-new sender domain set the
deliverability narrative. Mailbox providers (especially Gmail) score
us on the engagement of these initial emails. Bad list = permanent
reputation hit.

Concretely, for warmup-25:

- Prospects you actually know something about (recent news, mutual
  contact, public statement on a topic our product addresses).
- Avoid generic info@ / sales@ addresses for the warmup batch — go to
  named decision-makers when you can.
- Spread across at least 5 distinct domains; the `--ack-new-domains`
  gate refuses a batch with too many fresh ones at once.
- Don't include a single prospect you wouldn't be happy to hand-write
  to. The model uses these prospects as STRUCTURAL examples, but if a
  bad one slips through the warmup pass it costs the whole campaign.

## Validate before ingesting

```sh
salesman validate-csv --from-csv samples/prospects-warmup-template.csv
```

You should see N parsable rows (where N = your real count) with
high homepage / industry / description coverage. Errors print
inline with row numbers; fix and re-run.

## Ingest

```sh
salesman import-csv \
    --campaign warmup-2026-05 \
    --path samples/prospects-warmup-template.csv \
    --dry-run    # preview first; no DB writes
```

Drop `--dry-run` for the real import. Idempotent on
`(campaign, company)` so re-runs collapse instead of duplicating.

## Other samples in this folder

- `pricing.toml`     — pricing-question reply drafter consults this.
- `meeting-slots.toml` — meeting-question reply drafter consults this.
- `objections.toml`  — objection-handling library for the reply drafter.
- `competitors.toml` — competitor-mention detector for `salesman alerts`.
- `products.toml`    — product catalog for `salesman pick-angle`.
- `operator-brief.md` — owner-curated 200-300 word brand brief; load
  via `SALESMAN_OPERATOR_BRIEF=samples/operator-brief.md`. Keeps
  fallback LLMs tone-aligned per `docs/MODEL_RESILIENCE.md`.
