-- Multi-touch sequence support.
-- A sequence belongs to one campaign; campaigns can have multiple
-- sequences (e.g. "v1 outreach" and "v2 follow-up").
-- Steps are ordered by `position`. Each step has a template_key
-- (looked up by the draft generator) and a delay_days from the
-- previous step's send.

CREATE TABLE sequences (
    id              UUID PRIMARY KEY,
    campaign_id     UUID NOT NULL REFERENCES campaigns(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (campaign_id, name)
);

CREATE TABLE sequence_steps (
    id              UUID PRIMARY KEY,
    sequence_id     UUID NOT NULL REFERENCES sequences(id) ON DELETE CASCADE,
    position        INTEGER NOT NULL,        -- 0-indexed
    channel         TEXT NOT NULL DEFAULT 'email',
    template_key    TEXT NOT NULL,           -- referenced by draft generator
    delay_days      INTEGER NOT NULL DEFAULT 0,
    UNIQUE (sequence_id, position)
);

-- Per-prospect sequence state. One prospect can be in at most one
-- sequence at a time (enforced by primary key).
CREATE TABLE prospect_sequence_state (
    prospect_id     UUID PRIMARY KEY REFERENCES prospects(id) ON DELETE CASCADE,
    sequence_id     UUID NOT NULL REFERENCES sequences(id) ON DELETE CASCADE,
    current_step    INTEGER NOT NULL DEFAULT 0,
    next_due_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    paused          BOOLEAN NOT NULL DEFAULT FALSE,
    paused_reason   TEXT,
    last_advanced_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX prospect_sequence_state_due_idx ON prospect_sequence_state (next_due_at) WHERE NOT paused;
