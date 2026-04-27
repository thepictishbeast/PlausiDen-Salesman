# Sample inputs

Templates the operator can copy + edit before ingesting via
`salesman discover`.

## prospects-warmup-template.csv

Five fictional rows showing every supported column. The columns
`display_name` is **required**; everything else is optional but
improves draft quality (industry + description give the LLM something
real to anchor on).

Validate before ingesting:

```sh
salesman validate-csv --from-csv samples/prospects-warmup-template.csv
```

You should see `5` parsable rows with 100% homepage / industry /
description coverage on the sample.

When you replace these with real prospects:

- **NEVER include addresses you haven't researched.** The
  --ack-new-domains gate will refuse a batch with too many fresh
  domains; that's reputation insurance, not a bug.
- Keep the **first** real campaign at 25 rows or fewer. Volume is the
  enemy of warmup.
- The `display_name` is what the operator sees in the review queue
  and what the LLM uses verbatim in greetings — name it as the
  COMPANY, not a person.

When ready, ingest:

```sh
salesman discover --campaign warmup-2026-04 \
    --from-csv samples/prospects-warmup-template.csv
```
