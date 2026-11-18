-- Reviewed overrides of source-code rules (mazimi_core::condition_review::overrides).
-- A review may widen (`widen` + `replacement`) or drop (`drop`) a code rule
-- that hides a programme (age band, regions, districts, enrollment statuses,
-- compiled jobCd/schoolCd/sbizCd/plcyMajorCd conditions) where the notice text
-- admits more people. Each entry is validated on write (target exists, the
-- replacement provably contains it, verbatim evidence) and applied at serve
-- only while the review's text_hash is current. NULL = no override.
--
-- The table already has RLS enabled and forced with a service-role-only
-- policy (20261015100001); a new column inherits it.
--
-- Additive only. Rollback (do not run automatically): revert the API/worker
-- consumers first, then
--   DROP INDEX program_condition_reviews_code_overrides;
--   ALTER TABLE program_condition_reviews DROP COLUMN code_overrides;
ALTER TABLE program_condition_reviews
    ADD COLUMN code_overrides JSONB CHECK (
        code_overrides IS NULL
        OR (jsonb_typeof(code_overrides) = 'array'
            AND jsonb_array_length(code_overrides) BETWEEN 1 AND 8));

-- The list reads every current override before its structural pass; only
-- reviews with overrides are indexed.
CREATE INDEX program_condition_reviews_code_overrides
    ON program_condition_reviews (source_type, source_id)
    WHERE code_overrides IS NOT NULL;

COMMENT ON COLUMN program_condition_reviews.code_overrides IS
    'Reviewed widen/drop overrides of source-code rules: [{target, action, replacement?, check_kind, seg_id?, evidence}]. Applied only while text_hash is current.';
