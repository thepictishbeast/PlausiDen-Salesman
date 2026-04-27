-- Trigger events — recent reasons to reach out to a prospect TODAY.
--
-- Populated by `salesman triggers scan`, which polls OSINT sources
-- (GDELT, GitHub, HN, Wikipedia, custom feeds) for activity that
-- justifies a fresh cold-touch THIS WEEK rather than a cold cold-
-- touch.
--
-- The drafter reads these when present and uses them as the
-- personalization anchor — "saw your team's announcement that you
-- raised a Series A last Tuesday" beats "I noticed you have a
-- security stack" by an order of magnitude.
--
-- Recency_score 0..1 — higher is fresher (decays exponentially).
-- Relevance_score 0..1 — higher is more on-topic (scored by the
-- adapter using its own signals).
-- Combined = recency_score * relevance_score; the operator-facing
-- ranking sorts by this.

CREATE TABLE trigger_events (
    id              UUID PRIMARY KEY,
    prospect_id     UUID NOT NULL REFERENCES prospects(id) ON DELETE CASCADE,
    source          TEXT NOT NULL,            -- 'gdelt' | 'github' | 'hn' | 'manual' | ...
    headline        TEXT NOT NULL,
    url             TEXT,
    recency_score   REAL NOT NULL DEFAULT 0.5,
    relevance_score REAL NOT NULL DEFAULT 0.5,
    raw             JSONB NOT NULL DEFAULT '{}',
    used_in_touch   UUID REFERENCES touches(id) ON DELETE SET NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Idempotency: same prospect+source+url is one row, not many.
    UNIQUE (prospect_id, source, url)
);
CREATE INDEX trigger_events_prospect_idx ON trigger_events (prospect_id);
CREATE INDEX trigger_events_recency_idx
    ON trigger_events ((recency_score * relevance_score) DESC, created_at DESC);
CREATE INDEX trigger_events_unused_idx
    ON trigger_events (prospect_id)
    WHERE used_in_touch IS NULL;
