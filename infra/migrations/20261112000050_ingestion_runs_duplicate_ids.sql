-- Records of one fetch that shared a source id with a later record and were
-- dropped (audit M1). A source must give every upstream row its own id; the
-- worker logs a non-zero count and stores it here.
-- Additive: one column with a default; ingestion_runs keeps its RLS.
-- Rollback (do not run automatically):
--   ALTER TABLE ingestion_runs DROP COLUMN total_duplicate_ids;
ALTER TABLE ingestion_runs
    ADD COLUMN IF NOT EXISTS total_duplicate_ids integer NOT NULL DEFAULT 0;

COMMENT ON COLUMN ingestion_runs.total_duplicate_ids IS
    'Records of this fetch dropped because a later record had the same source id.';
