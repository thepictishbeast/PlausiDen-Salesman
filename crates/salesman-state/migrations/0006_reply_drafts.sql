-- Reply-draft linkage.
--
-- When the operator runs `salesman draft-replies`, the system pulls
-- classified replies that need a response and produces a NEW touch
-- in the `awaiting_approval` queue. We need a way to know which
-- replies have been drafted (so we don't re-draft) and which touch
-- carries the response (so the operator + audit can trace it).
--
-- Approach: add `response_touch_id` to `replies`. Nullable; set when
-- a draft is created. Foreign-key with ON DELETE SET NULL so a
-- rejected/cleared draft doesn't break the chain.

ALTER TABLE replies ADD COLUMN response_touch_id UUID
    REFERENCES touches(id) ON DELETE SET NULL;
CREATE INDEX replies_response_touch_idx ON replies (response_touch_id)
    WHERE response_touch_id IS NOT NULL;
