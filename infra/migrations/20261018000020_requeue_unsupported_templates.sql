-- The collector now recognises HWPX (stored `mimetype` entry
-- `application/hwp+zip`). Template candidates it marked `unsupported` before
-- are due again now instead of after their 1-14 day backoff, so an HWPX
-- form (e.g. a current intake round) becomes a candidate on the next run.
-- Schedule columns only: no status, evidence or review column changes, and
-- every row stays. Not reversed on rollback (an earlier probe is harmless;
-- the old binary would just mark the row unsupported again).
UPDATE program_application_resources
   SET next_check_at = now(), consecutive_failures = 0
 WHERE kind = 'template' AND status = 'candidate' AND check_status = 'unsupported'
   AND next_check_at > now();
