-- Weekly full re-extraction audit of corporate offers (measurement only).
--
-- Once a week every corporate_official programme with an official page is
-- read again with the fresh extractor (corporate_auto::reextract) and
-- compared with its stored values. The verdict is recorded here and
-- summarised in corporate_pipeline_daily and the ops digest. Nothing in this
-- migration or the job hides, publishes or changes a programme, a monitor,
-- a hold, a fact or a proposal, in any verification mode.
--
-- corporate_reextract_state is keyed by source and drives the fresh-extraction
-- retry clock, and corporate_reextract_proposals holds offers awaiting a
-- publishing step; an audit verdict (per programme, possibly with no offer)
-- fits neither, so it gets its own table. Additive only; the job is off
-- until CORPORATE_WEEKLY_AUDIT_ENABLED is set.
--
-- Rollback (do not run automatically; flag off first):
--   DROP TABLE IF EXISTS corporate_weekly_audit_verdicts;

CREATE TABLE IF NOT EXISTS corporate_weekly_audit_verdicts (
    id             UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    program_id     UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    -- The source page read; NULL when the programme has no source monitor.
    candidate_id   UUID        REFERENCES benefit_source_candidates(id) ON DELETE SET NULL,
    audited_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- CORPORATE_AUTO_VERIFY_MODE at the time; recorded only, it never
    -- changes what the audit does.
    mode           TEXT        NOT NULL CHECK (mode IN ('shadow', 'hide_only', 'live')),
    -- audit_fetch: the audit's own polite fetch; evidence_page: the page the
    -- evidence job had just read (no second request); no_request: refused
    -- before any request.
    via            TEXT        NOT NULL CHECK (via IN ('audit_fetch', 'evidence_page', 'no_request')),
    verdict        TEXT        NOT NULL CHECK (verdict IN ('agrees', 'disagrees', 'unreadable')),
    -- The stored fields the page contradicts (verdict 'disagrees' only).
    fields         TEXT[]      NOT NULL DEFAULT '{}'
                               CHECK (fields <@ ARRAY['title', 'audience', 'age', 'benefit', 'dates', 'url']::text[]),
    -- Why nothing could be compared (verdict 'unreadable' only).
    reason         TEXT        CHECK (reason IS NULL OR char_length(reason) BETWEEN 1 AND 60),
    reason_class   TEXT        CHECK (reason_class IS NULL OR reason_class IN ('policy', 'transient', 'no_offer', 'no_source')),
    program_active BOOLEAN     NOT NULL,
    final_url      TEXT        CHECK (final_url IS NULL OR char_length(final_url) <= 2000),
    page_hash      TEXT        CHECK (page_hash IS NULL OR page_hash ~ '^[0-9a-f]{8,64}$'),
    -- Per-field comparison and the gates; values and short reasons only,
    -- never page text.
    details        JSONB       NOT NULL DEFAULT '{}'::jsonb
                               CHECK (jsonb_typeof(details) = 'object' AND pg_column_size(details) <= 8192),
    CHECK ((verdict = 'disagrees') = (cardinality(fields) > 0)),
    CHECK ((verdict = 'unreadable') = (reason IS NOT NULL)),
    CHECK ((reason IS NULL) = (reason_class IS NULL))
);
CREATE INDEX IF NOT EXISTS idx_corporate_weekly_audit_program
    ON corporate_weekly_audit_verdicts (program_id, audited_at DESC);
CREATE INDEX IF NOT EXISTS idx_corporate_weekly_audit_time
    ON corporate_weekly_audit_verdicts (audited_at DESC);

DO $rls$
BEGIN
  ALTER TABLE corporate_weekly_audit_verdicts ENABLE ROW LEVEL SECURITY;
  ALTER TABLE corporate_weekly_audit_verdicts FORCE ROW LEVEL SECURITY;
  REVOKE ALL ON TABLE corporate_weekly_audit_verdicts FROM PUBLIC;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE corporate_weekly_audit_verdicts FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE corporate_weekly_audit_verdicts FROM authenticated;
  END IF;
  DROP POLICY IF EXISTS corporate_weekly_audit_verdicts_all_service ON corporate_weekly_audit_verdicts;
  CREATE POLICY corporate_weekly_audit_verdicts_all_service ON corporate_weekly_audit_verdicts
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());
END
$rls$;

COMMENT ON TABLE corporate_weekly_audit_verdicts IS
    'Weekly re-extraction audit of corporate offers: agrees / disagrees (fields) / unreadable (reason). Measurement only; never hides or publishes. Internal.';
COMMENT ON COLUMN corporate_weekly_audit_verdicts.program_active IS
    'Visibility at observation time. An evidence_page verdict is written beside the same evidence pass''s decision and may read the state before or after it; aggregates and alerts read programs.is_active instead.';
