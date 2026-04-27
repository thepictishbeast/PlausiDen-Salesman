# 0003 — Claude + Gemini default; LFI deferred (decided)

## Context

Original SCOPE.md (pre-2026-04-26) said: "LFI is the personalization
brain. No SaaS LLM in the data path." This was the sovereignty-first
framing.

The owner's directive on the evening of 2026-04-26 superseded that:
"We want a very capable sales agent that can make cold sales using
[Claude] and Gemini." Capability now > sovereignty for v1; LFI lands
as a third backend later.

LFI itself isn't production-ready for cold-sales personalization
yet (it's still in foundational-corpus-ingestion phase per the LFI
roadmap), so even if we wanted sovereignty-first today, we'd be
choosing between "ship nothing" and "ship with SaaS LLMs."

## Decision

`salesman-llm::LlmRouter` ships with two backends: Claude
(default for reasoning) and Gemini (default for bulk + grounding).
LFI is a third `BackendKind::Lfi` variant that exists in the type
system but has no implementation. When LFI is production-ready,
register a third backend via the same trait — no router changes
needed.

## Consequences

- ✅ Salesman ships with capable LLMs from day one.
- ✅ Routing rules abstract the choice: caller passes
  `RouteHint::Reasoning` etc., router picks. Switching default
  backends is a one-line config change.
- ⚠️  PII (prospect names, emails, contexts) leaves the cluster
  with every draft + classification call. Owner consciously
  accepted this as the trade for capability.
- ⚠️  Cost surface: Claude + Gemini API calls add up. Cost ledger
  (M1-M4) makes this visible.
- ❌ We are NOT positioning Salesman as a sovereignty product
  today. The marketing line is honest about what's in the data
  path.

## Alternatives considered

- **LFI-only from day one** — original framing; lost because LFI
  isn't ready and the owner needs revenue.
- **Single backend (Claude only)** — simpler routing; lost because
  Gemini is dramatically cheaper for bulk classification, and not
  having a fallback when one provider degrades is operational risk.

## Status

`decided 2026-04-26 by claude-code session`

## References

- `crates/salesman-llm/src/router.rs` (RouteHint + LlmRouter)
- `crates/salesman-llm/src/{claude,gemini}.rs`
- Owner directive 2026-04-26 evening (in chat)
