-- Durable, deduplicated operator alerts. Worker logs are transient; these
-- events remain visible to administrators and can optionally be delivered to
-- a configured HTTPS webhook. A fingerprint represents one incident lifecycle.

CREATE TABLE operational_events (
    id                  UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    fingerprint         TEXT        NOT NULL UNIQUE CHECK (
                            char_length(fingerprint) BETWEEN 1 AND 200
                        ),
    severity            TEXT        NOT NULL CHECK (
                            severity IN ('info', 'warning', 'error', 'critical')
                        ),
    component           TEXT        NOT NULL CHECK (
                            char_length(component) BETWEEN 1 AND 100
                        ),
    event_type          TEXT        NOT NULL CHECK (
                            char_length(event_type) BETWEEN 1 AND 100
                        ),
    subject_type        TEXT        CHECK (
                            subject_type IS NULL OR char_length(subject_type) BETWEEN 1 AND 100
                        ),
    subject_id          TEXT        CHECK (
                            subject_id IS NULL OR char_length(subject_id) BETWEEN 1 AND 200
                        ),
    message             TEXT        NOT NULL CHECK (
                            char_length(message) BETWEEN 1 AND 500
                        ),
    details             JSONB       NOT NULL DEFAULT '{}'::jsonb CHECK (
                            jsonb_typeof(details) = 'object'
                        ),
    status              TEXT        NOT NULL DEFAULT 'open' CHECK (
                            status IN ('open', 'acknowledged', 'resolved')
                        ),
    occurrence_count    INTEGER     NOT NULL DEFAULT 1 CHECK (
                            occurrence_count BETWEEN 1 AND 100000000
                        ),
    first_occurred_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_occurred_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    acknowledged_at     TIMESTAMPTZ,
    acknowledged_by     UUID        REFERENCES users(id) ON DELETE SET NULL,
    resolved_at         TIMESTAMPTZ,
    resolved_by         UUID        REFERENCES users(id) ON DELETE SET NULL,
    resolution_note     TEXT        CHECK (
                            resolution_note IS NULL OR char_length(resolution_note) <= 1000
                        ),
    next_delivery_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivery_attempt_count INTEGER  NOT NULL DEFAULT 0 CHECK (
                            delivery_attempt_count BETWEEN 0 AND 20
                        ),
    delivered_at        TIMESTAMPTZ,
    delivery_claim_token UUID,
    delivery_claimed_until TIMESTAMPTZ,
    last_delivery_error TEXT        CHECK (
                            last_delivery_error IS NULL OR char_length(last_delivery_error) <= 1000
                        ),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        (status = 'resolved' AND resolved_at IS NOT NULL)
        OR (status <> 'resolved' AND resolved_at IS NULL)
    ),
    CHECK (
        (delivery_claim_token IS NULL AND delivery_claimed_until IS NULL)
        OR (delivery_claim_token IS NOT NULL AND delivery_claimed_until IS NOT NULL)
    )
);

CREATE INDEX idx_operational_events_open
    ON operational_events (severity, last_occurred_at DESC)
    WHERE status <> 'resolved';

CREATE INDEX idx_operational_events_delivery
    ON operational_events (next_delivery_at, severity)
    WHERE status <> 'resolved' AND delivered_at IS NULL;

ALTER TABLE operational_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE operational_events FORCE ROW LEVEL SECURITY;
CREATE POLICY "operational_events_all_service"
    ON operational_events
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

COMMENT ON TABLE operational_events IS
    'Deduplicated incident lifecycle and optional webhook-delivery outbox for Mazimi operators.';
