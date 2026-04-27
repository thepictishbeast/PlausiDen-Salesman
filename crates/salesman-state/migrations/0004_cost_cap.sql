-- Per-campaign LLM cost cap. NULL = no cap.
-- When a cap is set, the orchestrator checks
-- SUM(llm_calls.cost_micro_usd) for that campaign before each call;
-- once exceeded, the campaign is auto-paused.

ALTER TABLE campaigns
    ADD COLUMN cost_cap_micro_usd BIGINT;

-- Helpful for the cap check.
CREATE INDEX llm_calls_related_idx ON llm_calls (related_kind, related_id)
    WHERE related_id IS NOT NULL;
