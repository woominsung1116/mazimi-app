-- One-time deployment repair. Stop the worker before running this file.
-- Keep historical applications and program rows; only suspend public listings.
BEGIN;
UPDATE programs
SET program_status='review_required', updated_at=now()
WHERE source_type='manual' AND source_verified_at IS NULL
  AND program_status IN ('active','upcoming')
  AND created_at < '2026-09-23T00:00:00+09'::timestamptz;

-- These zero-row runs predate the fix that propagates provider errors.
UPDATE ingestion_runs
SET status='failed', error_message='Legacy run did not validate upstream errors; retry with corrected adapter'
WHERE source_name IN ('fss_financial','national_welfare','local_welfare')
  AND status='success' AND total_fetched=0
  AND started_at >= '2026-09-23T00:00:00+09'::timestamptz
  AND started_at < '2026-09-23T04:20:00Z'::timestamptz;

UPDATE worker_daily_jobs SET status='failed',lease_until=NULL,lease_token=NULL,
  last_success_at=NULL,last_attempt_at=now()-interval '2 hours',
  details='{"reason":"retry after upstream request and error-handling repair"}'::jsonb
WHERE job_name IN ('daily:youth','daily:financial','daily:national_welfare','daily:local_welfare','daily:government')
  AND last_attempt_at < '2026-09-23T04:20:00Z'::timestamptz;

UPDATE ingestion_runs SET status='failed',finished_at=now(),
  error_message='Interrupted by deployment; scheduled for idempotent retry'
WHERE status='running' AND run_kind='ingestion'
  AND started_at < '2026-09-23T04:20:00Z'::timestamptz;
COMMIT;
