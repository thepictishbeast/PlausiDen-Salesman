-- Track which template a Touch was generated from. Drives the
-- template-performance report (L4) and feeds the bandit (L5).
-- NULL is permitted for touches that were drafted without a
-- template (early-life data + future ad-hoc one-offs).

ALTER TABLE touches
    ADD COLUMN template_key TEXT;

CREATE INDEX touches_template_key_idx ON touches (template_key)
    WHERE template_key IS NOT NULL;
