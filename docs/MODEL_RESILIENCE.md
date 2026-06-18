# MODEL_RESILIENCE.md — LLM router contract under degradation

Source-of-truth for how Salesman's LLM layer behaves when a model is
rate-limited, billing-exhausted, returning 5xx, or replaced.

The over-arching principle:

> **Quality regression must be visible. Silence is the failure mode.**

Falling back to a smaller model is fine. Falling back to a smaller
model and silently shipping a worse-quality draft is a P0.

---

## The contract

### 1. Every call is tagged

Every LLM call records `(backend, model, purpose, related_id)` in the
`llm_calls` table. The `purpose` is the chat_for tag (e.g.
`draft_cold_email`, `classify_reply`, `seo_meta`). The `related_id`
points at the artifact (touch, reply, page) that the call produced.

Operators can therefore answer at any time:

> "Show me every draft produced by Gemini-Flash in the last 24h
> while Anthropic was rate-limiting us."

…via a single SQL query against `llm_calls` joined with `touches`.

### 2. Artifacts carry their provenance

`touches` has a `produced_by` JSONB column that records `{ "backend":
"gemini", "model": "gemini-2.5-flash", "via_fallback": true }` at the
moment of draft creation. The web dashboard + CLI surface this.

`via_fallback: true` means the primary preference (per `RouteHint`) was
unavailable and the router degraded. The operator sees this in the
review queue and can choose to re-draft when the primary recovers.

### 3. Fallback chains are explicit, not silent

`LlmRouter::chat_for(hint, purpose, req)` selects backends in
preference order:

| RouteHint        | Primary         | Fallback chain                    |
|------------------|-----------------|------------------------------------|
| DeepReasoning    | Claude Opus 4.7 | Claude Sonnet → Gemini Pro → fail |
| Reasoning        | Claude Sonnet   | Gemini Pro → Gemini Flash → fail   |
| Bulk             | Gemini Flash    | Claude Haiku → fail                |
| LocalOnly        | LFI             | (no fallback — fail loud)          |

On a fallback hop, the call is recorded with the actual backend used
and `via_fallback=true`. Operator sees the degradation in the cost
report (`salesman costs --by purpose --since 1h` shows the model split).

### 4. Sensitive ops gate on backend health

These commands refuse to run if any required backend is unavailable:

- `salesman send-pending --for-real` — must have at least the campaign's
  declared minimum-quality backend reachable. (Default: must have
  Claude OR Gemini reachable; without either, exit non-zero.)
- `salesman draft` — same.
- `salesman preflight --campaign X` — surfaces backend-health line in
  the verdict; flips READY → BLOCKED on unhealthy.

`salesman doctor` always runs and reports — it's the diagnostic.

### 5. Prompt freshness across model swaps

A fresh-context model produces tone-disconnected output. To mitigate:

- `SALESMAN_OPERATOR_BRIEF` env var points to a markdown file that
  gets prepended to every system prompt under a section header
  `## Operator brief (do not echo verbatim)`.
- The brief is the operator's curated 200-300 word context: company
  name, sender identity, who we are NOT, tone guide, banned phrases.
- Because it's prepended to every call, swapping the model does not
  swap the context. The brief enforces consistency.

### 6. Quality gate of last resort

Even when 1-5 fail, the existing gates catch quality regression:

- Detector ensemble runs on every draft before it reaches the approval
  queue (`max_retries` regenerate on detector failure).
- Human approval is the final gate. The reviewer sees `produced_by`
  + `via_fallback`.
- `audit-chain` daily timer attests the hash chain. v2 signs the full
  receipt, so mutating any field after the fact is detectable — but
  end-of-chain truncation or full-table deletion needs an external
  anchor to catch (see `docs/AUDIT_CHAIN.md`).

---

## What to build (implementation map)

### Pass A — touch-tagging  (THIS COMMIT)
- Schema: `ALTER TABLE touches ADD COLUMN produced_by JSONB`
- Draft handler: insert `{ "backend", "model", "via_fallback" }` at
  draft time
- Web dashboard + CLI review surface the tag

### Pass B — backend-health gate  (NEXT)
- `LlmRouter::backend_status() -> Vec<(BackendKind, Health)>`
- `Health = Healthy | RateLimited { retry_after } | Degraded { reason }
  | Unavailable { reason }`
- `salesman doctor` exposes
- `salesman preflight` checks campaign-required backends + flips
  verdict
- `salesman send-pending` refuses on Unavailable for required hint

### Pass C — operator-brief preamble
- `LlmRouter::with_operator_brief(path: PathBuf) -> Self`
- On every chat_for, prepend `## Operator brief...` section to the
  first system message
- `salesman whoami` reports the brief path + first 100 chars
- Sample brief shipped at `samples/operator-brief.md`

### Pass D — explicit fallback chain wiring
- `chat_for` tries primary, on rate-limit / 5xx tries next in chain
- Each fallback hop logs at WARN with the reason
- `via_fallback` boolean threaded through the LlmResponse

### Pass E — daily summary email + Slack webhook
- Cron-fired summary mail at 09:00 local
- `SALESMAN_ALERT_WEBHOOK` for Slack/Discord on positive replies
- Formatter handles both targets via a small abstraction
