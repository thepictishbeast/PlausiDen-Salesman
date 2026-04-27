-- Reply-side tags: "what's interesting about this reply beyond
-- its kind?" — competitor mentions, intent signals, urgency
-- markers. Stored as a JSONB array of strings keyed by source.
--
-- Shape examples:
--   { "competitors": ["outreach", "lemlist"] }
--   { "competitors": ["splunk"], "urgency": ["high"] }
--   { "intent_signals": ["evaluating-now"] }
--
-- The competitor-mention detector populates `competitors`. Future
-- detectors (intent / urgency / etc.) co-exist as additional keys.
-- Defaults to empty object so legacy rows just have no tags.

ALTER TABLE replies ADD COLUMN tags JSONB NOT NULL DEFAULT '{}';
CREATE INDEX replies_competitor_idx ON replies ((tags->'competitors'))
    WHERE tags ? 'competitors';
