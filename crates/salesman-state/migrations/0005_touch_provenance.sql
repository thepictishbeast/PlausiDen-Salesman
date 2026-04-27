-- Per-touch model provenance.
--
-- Records which LLM backend + model produced each draft so the
-- operator can answer "show me everything Gemini-Flash drafted while
-- Anthropic was rate-limited" or "did fallback drafts ever convert
-- worse than primary drafts." Foundation for quality-regression
-- analytics + the model-resilience contract in MODEL_RESILIENCE.md.
--
-- Shape:
--   {
--     "backend": "claude" | "gemini" | "lfi",
--     "model":   "claude-opus-4-7" | "gemini-2.5-pro" | "lfi-7b" | ...,
--     "via_fallback": false,            -- true if primary was unavailable
--     "purpose": "draft_cold_email"     -- chat_for() purpose tag
--   }
--
-- Nullable for backwards-compat — touches drafted before this
-- migration get NULL and surface as "(unknown provenance)" in audit
-- queries.

ALTER TABLE touches ADD COLUMN produced_by JSONB;
CREATE INDEX touches_produced_backend_idx
    ON touches ((produced_by->>'backend'))
    WHERE produced_by IS NOT NULL;
CREATE INDEX touches_via_fallback_idx
    ON touches (((produced_by->>'via_fallback')::boolean))
    WHERE produced_by IS NOT NULL;
