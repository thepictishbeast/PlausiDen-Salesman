-- Initial schema for the Salesman pipeline.
-- BUG ASSUMPTION: every uuid uses uuidv7 generated app-side so they
-- sort by creation time. We do not generate uuids server-side here.

CREATE EXTENSION IF NOT EXISTS pgcrypto;

-- Companies discovered through any source. Source-of-truth for
-- "this company exists in our world".
CREATE TABLE companies (
    id              UUID PRIMARY KEY,
    legal_name      TEXT,
    display_name    TEXT NOT NULL,
    homepage        TEXT,
    industry        TEXT,
    size_band       TEXT,
    region          TEXT,
    description     TEXT,
    tech_signals    JSONB NOT NULL DEFAULT '[]',
    discovered_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_enriched_at TIMESTAMPTZ,
    source          TEXT NOT NULL,
    raw             JSONB NOT NULL DEFAULT '{}'
);
CREATE INDEX companies_homepage_idx ON companies (homepage);
CREATE INDEX companies_industry_idx ON companies (industry);

-- Contacts at companies. Either role (info@, sales@) or person.
CREATE TABLE contacts (
    id              UUID PRIMARY KEY,
    company_id      UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL,
    name            TEXT,
    title           TEXT,
    email           CITEXT,
    email_verified  BOOLEAN NOT NULL DEFAULT FALSE,
    linkedin_url    TEXT,
    source          TEXT NOT NULL,
    discovered_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (company_id, email)
);
CREATE INDEX contacts_company_idx ON contacts (company_id);

-- A reusable named outreach effort with a goal.
CREATE TABLE campaigns (
    id              UUID PRIMARY KEY,
    name            TEXT NOT NULL UNIQUE,
    goal            TEXT NOT NULL,
    target_segment  TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'draft',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    paused_at       TIMESTAMPTZ,
    paused_reason   TEXT
);

-- A specific (campaign, company) pair. The trackable funnel unit.
CREATE TABLE prospects (
    id                  UUID PRIMARY KEY,
    campaign_id         UUID NOT NULL REFERENCES campaigns(id) ON DELETE CASCADE,
    company_id          UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    primary_contact_id  UUID REFERENCES contacts(id),
    state               TEXT NOT NULL DEFAULT 'new',
    state_reason        TEXT,
    state_changed_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    fit_score           REAL,
    notes               TEXT,
    UNIQUE (campaign_id, company_id)
);
CREATE INDEX prospects_campaign_idx ON prospects (campaign_id);
CREATE INDEX prospects_state_idx    ON prospects (state);

-- Outbound actions on a prospect.
CREATE TABLE touches (
    id              UUID PRIMARY KEY,
    prospect_id     UUID NOT NULL REFERENCES prospects(id) ON DELETE CASCADE,
    channel         TEXT NOT NULL,
    subject         TEXT,
    body            TEXT NOT NULL,
    queued_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    sent_at         TIMESTAMPTZ,
    outcome         TEXT NOT NULL DEFAULT 'drafted',
    receipt_id      UUID
);
CREATE INDEX touches_prospect_idx ON touches (prospect_id);
CREATE INDEX touches_outcome_idx  ON touches (outcome);

-- Inbound messages we've received. Linked to a touch when threading lines up.
CREATE TABLE replies (
    id              UUID PRIMARY KEY,
    prospect_id     UUID NOT NULL REFERENCES prospects(id) ON DELETE CASCADE,
    touch_id        UUID REFERENCES touches(id) ON DELETE SET NULL,
    from_address    CITEXT NOT NULL,
    subject         TEXT,
    body            TEXT NOT NULL,
    received_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    kind            TEXT NOT NULL DEFAULT 'unclassified',
    raw_headers     JSONB NOT NULL DEFAULT '{}'
);
CREATE INDEX replies_prospect_idx ON replies (prospect_id);

-- Per-domain and per-email do-not-contact list. Global across campaigns.
CREATE TABLE suppressions (
    id              UUID PRIMARY KEY,
    target          CITEXT NOT NULL UNIQUE,   -- email or domain
    target_kind     TEXT NOT NULL,            -- 'email' or 'domain'
    reason          TEXT NOT NULL,
    source          TEXT NOT NULL,            -- 'reply_optout' | 'manual' | 'bounce' | 'compliance'
    added_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Crypto receipts for every state-changing event.
-- Hash chain: each receipt's `prev_hash` references the previous one's `hash`.
CREATE TABLE receipts (
    id              UUID PRIMARY KEY,
    event_kind      TEXT NOT NULL,
    event_payload   JSONB NOT NULL,
    prev_hash       BYTEA,
    hash            BYTEA NOT NULL,
    signature       BYTEA NOT NULL,
    signing_key_id  TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX receipts_event_kind_idx ON receipts (event_kind);

-- LLM call audit log (token cost, latency, prompt-cache hits).
CREATE TABLE llm_calls (
    id              UUID PRIMARY KEY,
    backend         TEXT NOT NULL,           -- 'claude' | 'gemini' | 'lfi'
    model           TEXT NOT NULL,
    prompt_tokens   INTEGER NOT NULL,
    output_tokens   INTEGER NOT NULL,
    cache_hit_tokens INTEGER NOT NULL DEFAULT 0,
    latency_ms      INTEGER NOT NULL,
    cost_micro_usd  BIGINT NOT NULL DEFAULT 0,
    purpose         TEXT NOT NULL,           -- 'plan' | 'qualify' | 'draft' | 'classify' | etc.
    related_id      UUID,                    -- prospect_id / campaign_id / etc.
    related_kind    TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX llm_calls_purpose_idx ON llm_calls (purpose);
CREATE INDEX llm_calls_backend_idx ON llm_calls (backend);

-- Tool call audit log.
CREATE TABLE tool_calls (
    id              UUID PRIMARY KEY,
    tool_name       TEXT NOT NULL,
    args            JSONB NOT NULL,
    result          JSONB,
    ok              BOOLEAN NOT NULL,
    error           TEXT,
    duration_ms     INTEGER NOT NULL,
    related_id      UUID,
    related_kind    TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX tool_calls_tool_idx ON tool_calls (tool_name);
