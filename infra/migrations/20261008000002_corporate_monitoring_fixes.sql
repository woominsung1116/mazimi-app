-- Corporate-benefit monitoring fixes (audit items H1, M2, M8).
--
-- One new backup table, one idempotent data repair plus two recorded
-- contract changes that affect how existing rows compare with new ones.
--
-- H1 repair. Until this release, re-discovering a known URL (admin queue,
-- official seed job, licensed search) reset a candidate in a monitoring state
-- (change_review / unreachable_review / verification_due) back to queued or
-- review. The monitor and hold tables kept the truth, so the candidate status
-- is re-derived from them here. Only candidates that have a non-retired
-- monitor and sit in queued/review are touched; a human never puts a
-- monitored candidate into those states. Approved candidates never retain
-- review text, matching the review workflow.
--
-- Every row the repair touches is copied first (status, raw_text,
-- extracted_payload, next_check_at) so the NULLing of review text and the
-- next_check_at fill below are reversible:
--   UPDATE benefit_source_candidates c
--   SET status = b.status, raw_text = b.raw_text, extracted_payload = b.extracted_payload,
--       next_check_at = b.next_check_at, updated_at = now()
--   FROM benefit_source_candidate_review_backup b WHERE b.candidate_id = c.id;
-- The backup is written before RLS is forced on it (no connection-role
-- dependency) and cascades with the candidate.
--
-- Retention: the backup table exists only to reverse this migration. Drop it
-- with a follow-up migration once the deploy has been stable for at least
-- 30 days (DROP TABLE IF EXISTS benefit_source_candidate_review_backup).
CREATE TABLE IF NOT EXISTS benefit_source_candidate_review_backup (
    candidate_id      UUID PRIMARY KEY REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    status            TEXT NOT NULL,
    raw_text          TEXT,
    extracted_payload JSONB,
    next_check_at     TIMESTAMPTZ,
    backed_up_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO benefit_source_candidate_review_backup (candidate_id, status, raw_text, extracted_payload, next_check_at)
SELECT candidate.id, candidate.status, candidate.raw_text, candidate.extracted_payload, candidate.next_check_at
FROM benefit_source_candidates candidate
WHERE candidate.status IN ('queued', 'review')
  AND EXISTS (
      SELECT 1 FROM program_source_monitors monitor
      WHERE monitor.candidate_id = candidate.id
        AND monitor.verification_state <> 'retired'
  )
ON CONFLICT (candidate_id) DO NOTHING;

WITH monitored AS (
    SELECT candidate.id,
           CASE
               WHEN bool_or(monitor.verification_state = 'change_detected') THEN 'change_review'
               WHEN bool_or(monitor.verification_state = 'unreachable') THEN 'unreachable_review'
               WHEN bool_or(monitor.verification_state = 'review_due') THEN 'verification_due'
               ELSE 'approved'
           END AS derived_status
    FROM benefit_source_candidates candidate
    JOIN program_source_monitors monitor ON monitor.candidate_id = candidate.id
    WHERE candidate.status IN ('queued', 'review')
      AND monitor.verification_state <> 'retired'
    GROUP BY candidate.id
)
UPDATE benefit_source_candidates candidate
SET status = monitored.derived_status,
    raw_text = CASE
        WHEN monitored.derived_status = 'approved' THEN NULL
        ELSE candidate.raw_text
    END,
    extracted_payload = CASE
        WHEN monitored.derived_status = 'approved' THEN NULL
        ELSE candidate.extracted_payload
    END,
    next_check_at = COALESCE(candidate.next_check_at, now()),
    updated_at = now()
FROM monitored
WHERE candidate.id = monitored.id;

-- Lock the backup down once its rows are written: service role only.
ALTER TABLE benefit_source_candidate_review_backup ENABLE ROW LEVEL SECURITY;
ALTER TABLE benefit_source_candidate_review_backup FORCE ROW LEVEL SECURITY;
REVOKE ALL ON TABLE benefit_source_candidate_review_backup FROM PUBLIC;
DO $rls$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE benefit_source_candidate_review_backup FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE benefit_source_candidate_review_backup FROM authenticated;
  END IF;
END
$rls$;
DROP POLICY IF EXISTS benefit_source_candidate_review_backup_all_service
    ON benefit_source_candidate_review_backup;
CREATE POLICY benefit_source_candidate_review_backup_all_service
    ON benefit_source_candidate_review_backup
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

-- Content fingerprint v2 (M2). The worker now hashes the page title plus every
-- main/article block (or the in-scope link set for index pages) after
-- normalizing volatile tokens, instead of raw bytes or one block. A verified
-- hash recorded under the old scheme will differ on the first fetch under the
-- new scheme and place the usual reversible change_review hold. Every seeded
-- monitor currently stores a NULL hash on a non-crawlable domain, so no live
-- source is affected today; this note exists for the first domain whose
-- direct collection is approved.

-- Canonical URL v2 (M8). New candidates sort query pairs and drop a trailing
-- slash on non-root paths. Existing canonical_url / programs.source_id values
-- are deliberately NOT rewritten: programs.source_id doubles as the upsert key
-- for approved candidates, so a rewrite needs a coordinated change of both
-- tables and of the fetch URL. Rows whose legacy key differs from the v2 key
-- (10 trailing-slash candidates at the time of writing; every multi-parameter
-- query is already in sorted order) can gain one duplicate review candidate if
-- the same page is discovered again. run_corporate_benefit_maintenance()
-- already groups such pairs into the duplicate queue by stripping the slash.

-- Rollback preconditions: idempotent; re-running is a no-op. The status
-- re-derivation, the review-text NULLing and the next_check_at fill are
-- reversed with the UPDATE in the header, from
-- benefit_source_candidate_review_backup.
