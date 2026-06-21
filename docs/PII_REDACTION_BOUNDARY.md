# PII redaction boundary (SaaS-LLM)

PlausiDen-Salesman uses SaaS LLMs (Claude, Gemini) for drafting and reply
handling (ADR-0003; a fully-local LFI model is deferred). To honor the doctrine
"prospect PII does not leave the box in the clear," every prospect-bearing
prompt crosses a **redaction boundary**: PII is replaced with stable
placeholders before the SaaS call and the real values are restored
(rehydrated) in the model's output. The model reasons over placeholders; the
final draft contains the real names.

Implementation: `crates/salesman-core/src/redact.rs` (`redact` / `rehydrate`)
and `salesman_content::prospect_pii_terms`.

## What is redacted

| Redacted (placeholdered → rehydrated) | How |
|---|---|
| Email addresses | linear scan in `redact()` |
| Phone numbers | conservative scan in `redact()` |
| Company `display_name` (≥ 4 chars) | `prospect_pii_terms` → `extra_terms` |
| `homepage` URL | `prospect_pii_terms` → `extra_terms` |

**Deliberately NOT term-redacted:** free-text fields (`description`, interest
tags, etc.). The model needs them to personalize, and `redact()` still strips
any emails/phones inside them. Residual free-text names (e.g. a person named in
a description) are an **accepted v1 limitation**, not a regression — `main`
previously sent these fields entirely in the clear.

Short company names (< 4 chars) are skipped on purpose: `redact()` matches
terms as raw, case-sensitive substrings, so a 2–3 char name would clobber
unrelated text and corrupt the prompt.

## Covered call sites

All prospect-bearing LLM tools in `salesman-content`:

| Tool | `extra_terms` | Notes |
|---|---|---|
| `draft_email` (cold draft) | company + homepage | richest dossier; redact + rehydrate per retry attempt |
| `draft_reply` | company + homepage | |
| `angle_picker` | company + homepage | |
| `classify_reply` | none | reply subject/body only — no structured identity |
| `extract_interests` | none | reply subject/body only |

`draft_email` also warns if a `[[REDACTED_…]]` placeholder survives rehydrate
(a malformed draft should never ship).

## Telemetry rule

Redaction emits `tracing` telemetry as **counts + backend/model only — never
the redacted values** (the values are the PII). See [TELEMETRY.md](TELEMETRY.md).

## Caveats

- `SALESMAN_LLM_CLI_DEBUG_DIR` (subscriber-CLI transport, see
  [SUBSCRIBER_LOGIN.md](SUBSCRIBER_LOGIN.md)) dumps prompt content to disk for
  debugging. In the draft paths that content is already redacted, but treat the
  dump as sensitive and disable it in production.
- This boundary mitigates — it does not eliminate — SaaS exposure. The
  doctrine-complete option (route drafting to a local LFI model, no redaction
  needed) remains deferred (ADR-0003).

See also: [SECURITY.md](../SECURITY.md), ADR-0003, `redact.rs`.
