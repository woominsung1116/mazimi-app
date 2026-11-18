-- Source lifecycle: a program that disappears from a complete official fetch is
-- counted, then closed after repeated confirmed absence. Nothing is deleted.
-- Automatic closures (expired, source_removed) are reversible by the source;
-- operator holds (manual, NULL reason with review_required) are not.
ALTER TABLE programs
    ADD COLUMN IF NOT EXISTS source_missing_runs INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS source_missing_since TIMESTAMPTZ;

ALTER TABLE programs DROP CONSTRAINT IF EXISTS programs_source_missing_runs_check;
ALTER TABLE programs ADD CONSTRAINT programs_source_missing_runs_check
    CHECK (source_missing_runs >= 0);

ALTER TABLE programs DROP CONSTRAINT IF EXISTS programs_retirement_reason_check;
ALTER TABLE programs ADD CONSTRAINT programs_retirement_reason_check CHECK (
    retirement_reason IS NULL
    OR retirement_reason IN (
        'expired', 'duplicate', 'provider_withdrawn',
        'source_invalid', 'manual', 'source_removed'
    )
);

COMMENT ON COLUMN programs.source_missing_runs IS
    'Consecutive complete ingestion runs of programs.source_type that did not list this source_id. Reset to 0 when seen.';
COMMENT ON COLUMN programs.source_missing_since IS
    'First complete run that did not list this source_id; NULL while present upstream.';

-- Rows closed by the previous expire_all_programs() carried no reason. Label
-- only rows whose own dates prove the expiry so a later source listing with a
-- new window can restore them; operator holds never match this predicate.
-- Restricted to the sources run_ingestion reconciles: a manually curated row
-- an operator closed without a reason has no source listing to restore it
-- and must not become "expired" (which the upsert path would reopen).
UPDATE programs
SET retirement_reason = 'expired', retired_at = COALESCE(retired_at, updated_at)
WHERE NOT is_active AND program_status = 'closed' AND retirement_reason IS NULL
  AND source_type IN (
      'youth_center', 'gov_benefits', 'scholarship',
      'fss_financial', 'national_welfare', 'local_welfare'
  )
  AND LEAST(benefit_valid_until, deadline_at, application_end_at) <= now();

-- Rejections (deterministic upstream defects) are counted separately from
-- transient failures so one bad record no longer fails and re-fetches a source.
ALTER TABLE ingestion_runs
    ADD COLUMN IF NOT EXISTS total_rejected INTEGER NOT NULL DEFAULT 0;
ALTER TABLE ingestion_runs DROP CONSTRAINT IF EXISTS ingestion_runs_total_rejected_check;
ALTER TABLE ingestion_runs ADD CONSTRAINT ingestion_runs_total_rejected_check
    CHECK (total_rejected >= 0);

CREATE OR REPLACE FUNCTION expire_all_programs() RETURNS BIGINT LANGUAGE plpgsql AS $$
DECLARE expired BIGINT;
BEGIN
    UPDATE programs SET is_active=false, program_status='closed',
        retired_at=now(), retirement_reason='expired', updated_at=now()
    WHERE is_active AND LEAST(benefit_valid_until,deadline_at,application_end_at)<=now();
    GET DIAGNOSTICS expired=ROW_COUNT;
    RETURN expired;
END;
$$;
REVOKE EXECUTE ON FUNCTION expire_all_programs() FROM PUBLIC;

-- Rollback preconditions: stop new workers first. The two counters and the
-- widened CHECK are additive and safe for old binaries. Closing rows stay
-- closed (is_active=false, retirement_reason='source_removed'); reopening them
-- is a data decision, not an automatic rollback action.
