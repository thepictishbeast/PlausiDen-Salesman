-- Owner audit-notifications: one row per outbound contact, summarising
-- who / how / what-was-said so the operator has a durable, receipt-backed
-- record keyed by the prospect (see salesman-outreach::owner_notify).
--
-- `delivered_at` stays NULL until the operator mailbox actually receives
-- the notification — delivery is gated behind the send-approval path, so
-- rows accumulate as a pending queue + permanent audit log regardless of
-- whether delivery is wired up yet.
CREATE TABLE owner_notifications (
    id              UUID PRIMARY KEY,
    -- The touch this notification is about. SET NULL if the touch is
    -- later deleted — we keep the audit record either way.
    touch_id        UUID REFERENCES touches(id) ON DELETE SET NULL,
    prospect_id     UUID NOT NULL REFERENCES prospects(id) ON DELETE CASCADE,
    -- The prospect's name/business, captured at send time (goes in the
    -- notification subject).
    prospect_label  TEXT NOT NULL,
    to_address      CITEXT NOT NULL,
    channel         TEXT NOT NULL,
    sent_at         TIMESTAMPTZ NOT NULL,
    subject         TEXT,
    body            TEXT NOT NULL,
    receipt_id      UUID,
    campaign        TEXT,
    queued_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    delivered_at    TIMESTAMPTZ
);
CREATE INDEX owner_notifications_prospect_idx ON owner_notifications (prospect_id);
-- Fast lookup of the pending (undelivered) queue, oldest first.
CREATE INDEX owner_notifications_pending_idx
    ON owner_notifications (queued_at)
    WHERE delivered_at IS NULL;
