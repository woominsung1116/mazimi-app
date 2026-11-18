-- Fresh-extraction request on a corporate evidence target.
--
-- 20261018000012 is already applied in production and must not change;
-- schema added after it lives here.
--
-- Rollback (do not run automatically; flags off first):
--   ALTER TABLE corporate_evidence_targets DROP CONSTRAINT IF EXISTS corporate_evidence_targets_reextract_reason_check;
--   ALTER TABLE corporate_evidence_targets DROP COLUMN reextract_requested_at, DROP COLUMN reextract_reason;

-- A source whose fact can no longer be shown as the same offer (identity
-- changed, quote gone) is flagged for fresh extraction, which validates
-- from scratch instead of re-deriving from the old fact.
ALTER TABLE corporate_evidence_targets
    ADD COLUMN IF NOT EXISTS reextract_requested_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS reextract_reason TEXT;
ALTER TABLE corporate_evidence_targets DROP CONSTRAINT IF EXISTS corporate_evidence_targets_reextract_reason_check;
ALTER TABLE corporate_evidence_targets ADD CONSTRAINT corporate_evidence_targets_reextract_reason_check
    CHECK (reextract_reason IS NULL OR char_length(reextract_reason) <= 60);
