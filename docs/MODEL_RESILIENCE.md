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

Every LLM call records `(backend, model, purpose)` in the
`llm_calls` table. The `purpose` is the chat_for tag (e.g.
`draft_cold_email`, `classify_reply`, `seo_meta`). The
`related_id`/`related_kind` columns are **reserved but NOT yet
populated** — the sink never sets them, so they are always NULL today
(like `via_fallback`, see §2). The per-artifact join (pointing a call at
the touch, reply, or page it produced) is **planned, not wired**.

Once that tagging lands, operators will be able to answer at any time:

> "Show me every draft produced by Gemini-Flash in the last 24h
> while Anthropic was rate-limiting us."

…via a single SQL query against `llm_calls` joined with `touches`. Until
then the join key does not exist.

### 2. Artifacts carry their provenance

`touches` has a `produced_by` JSONB column that records `{ "backend":
"gemini", "model": "gemini-1.5-flash", "via_fallback": false }` at the
moment of draft creation. The web dashboard + CLI surface this.

`via_fallback` is a reserved field for the planned multi-backend
fallback work (see Pass D below). Today it is hardcoded `false`
everywhere — the router does not degrade to a secondary backend, so no
artifact is ever produced "via fallback". When fallback ships, a `true`
value will mean the primary preference (per `RouteHint`) was unavailable
and the router degraded, and the operator will see this in the review
queue and be able to re-draft when the primary recovers.

### 3. One backend per hint — no automatic fallback (yet)

`LlmRouter::chat_for(hint, purpose, req)` maps the `RouteHint` to
**exactly one** registered backend and uses it. There is no fallback
chain, no next-hop, and no automatic retry against a different backend:
if the selected backend is not registered, the call errors out loudly
rather than degrading to a secondary.

| RouteHint        | Backend (default)                |
|------------------|-----------------------------------|
| DeepReasoning    | Claude (`claude-opus-4-8`)        |
| Reasoning        | Claude (`claude-opus-4-8`)        |
| Grounded         | Gemini (`gemini-1.5-flash`)       |
| Bulk             | Gemini (`gemini-1.5-flash`)       |
| Sovereign        | LFI (deferred — see ADR-0003)     |

Both `default_reasoning` and `default_deep_reasoning` resolve to the
**same** Claude backend — reasoning vs deep-reasoning do not currently
select different models. The call is recorded with the backend actually
used; because there is no fallback, `via_fallback` is always `false`
(see §2).

Multi-backend fallback (primary → secondary on rate-limit / 5xx) is
**planned but unbuilt** — see Pass D in the implementation map below.

### 4. Sensitive ops gate on backend registration

These commands refuse to run if a required backend is not **registered**
(i.e. its key/transport is configured and the backend was wired into the
router). The gate is a *registration* check, not a reachability or
health check — it does not probe whether the backend is actually
answering, just that one exists:

- `salesman draft` — must have at least one usable backend registered.
  (Default: Claude OR Gemini registered; without either, exit non-zero.)
- `salesman preflight --campaign X` — surfaces a backend-registration
  line in the verdict; flips READY → BLOCKED when NO backend at all is
  registered.

`salesman send-pending --for-real` has **no** backend-registration check
of its own — that gate lives upstream on `draft` and `preflight`, which
must have run (and passed) before there are approved drafts to send.

`salesman doctor` always runs and reports — it's the diagnostic.

> Live health probing — distinguishing *registered* from *reachable /
> rate-limited / degraded* — is **not built yet**. There is no
> `backend_status()` accessor and no `Health` type today; both are part
> of the planned Pass B work (see the implementation map below). Until
> then, a registered-but-unreachable backend will pass these gates and
> fail at call time.

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
- `SALESMAN_ALERT_WEBHOOK_URL` for Slack/Discord on positive replies
- Formatter handles both targets via a small abstraction
