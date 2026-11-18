-- Maintenance self-healing (2026-10-17): replay quarantine counts, per-event
-- email delivery state, and automatic domain approval renewal bookkeeping.
--
-- Additive only. Rollback (manual, after the previous worker image is back):
--   ALTER TABLE ingestion_runs DROP COLUMN total_quarantined;
--   ALTER TABLE operational_events DROP COLUMN email_delivered_at,
--     DROP COLUMN email_delivery_attempt_count, DROP COLUMN email_next_delivery_at,
--     DROP COLUMN email_claimed_until, DROP COLUMN email_last_delivery_error;
--   ALTER TABLE benefit_source_domains DROP COLUMN approval_period_days,
--     DROP COLUMN approval_auto_renewed_at, DROP COLUMN approval_auto_renewals;
-- The previous binaries never read these columns, so leaving them in place
-- after an image rollback is safe.

-- A stored record that is rejected deterministically and never produced a
-- programme row (for example youth_center 20260422005400212868) is counted
-- here instead of as a rejection or failure, so real failures stay visible.
ALTER TABLE ingestion_runs
    ADD COLUMN IF NOT EXISTS total_quarantined INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ingestion_runs DROP CONSTRAINT IF EXISTS ingestion_runs_total_quarantined_check;
ALTER TABLE ingestion_runs ADD CONSTRAINT ingestion_runs_total_quarantined_check
    CHECK (total_quarantined >= 0);

-- Email is a second delivery channel with its own retry state; webhook
-- delivery keeps using delivered_at / next_delivery_at.
ALTER TABLE operational_events
    ADD COLUMN IF NOT EXISTS email_delivered_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS email_delivery_attempt_count INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS email_next_delivery_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN IF NOT EXISTS email_claimed_until TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS email_last_delivery_error TEXT;
ALTER TABLE operational_events DROP CONSTRAINT IF EXISTS operational_events_email_attempts_check;
ALTER TABLE operational_events ADD CONSTRAINT operational_events_email_attempts_check
    CHECK (email_delivery_attempt_count BETWEEN 0 AND 20);
ALTER TABLE operational_events DROP CONSTRAINT IF EXISTS operational_events_email_error_check;
ALTER TABLE operational_events ADD CONSTRAINT operational_events_email_error_check
    CHECK (email_last_delivery_error IS NULL OR char_length(email_last_delivery_error) <= 200);

-- An approval renewed by the worker keeps the human-reviewed period: the
-- first renewal derives it from compliance_reviewed_at, later renewals reuse
-- approval_period_days. A new admin approval (compliance_reviewed_at after
-- approval_auto_renewed_at) derives the period afresh.
ALTER TABLE benefit_source_domains
    ADD COLUMN IF NOT EXISTS approval_period_days INTEGER,
    ADD COLUMN IF NOT EXISTS approval_auto_renewed_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS approval_auto_renewals INTEGER NOT NULL DEFAULT 0;
ALTER TABLE benefit_source_domains DROP CONSTRAINT IF EXISTS benefit_source_approval_period_check;
ALTER TABLE benefit_source_domains ADD CONSTRAINT benefit_source_approval_period_check
    CHECK (approval_period_days IS NULL OR approval_period_days BETWEEN 1 AND 366);
