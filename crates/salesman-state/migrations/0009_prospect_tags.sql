-- 0009_prospect_tags.sql — per-prospect tags JSONB for personalization
-- inputs (interests, custom labels, anything operator wants the drafter
-- to remember about a prospect across sequences).
--
-- Shape convention:
--   tags = {
--     "interests":   ["data residency", "case study"],
--     "do_not_pitch": ["billing"],
--     "notes":       ["spoke at re:Invent 2025"]
--   }
--
-- Anything the operator (or U52's interest extractor) merges in
-- accumulates here; the drafter sees the whole bag and can cite
-- whichever bits anchor the next touch.

ALTER TABLE prospects
    ADD COLUMN IF NOT EXISTS tags JSONB NOT NULL DEFAULT '{}';
