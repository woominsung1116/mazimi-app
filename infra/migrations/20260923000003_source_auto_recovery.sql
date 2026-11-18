ALTER TABLE benefit_source_candidates ADD COLUMN relocation_attempts INT NOT NULL DEFAULT 0 CHECK (relocation_attempts BETWEEN 0 AND 5);
-- Existing suspended sources can recover when access remains authorized and
-- the previously verified content returns. This grants no new collection rights.
UPDATE benefit_source_candidates SET next_check_at=now()
WHERE status IN ('verification_due','unreachable_review','change_review') AND next_check_at IS NULL;
