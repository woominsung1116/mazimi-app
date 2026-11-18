-- Verification mode `hide_only`: hides for allowlisted reasons apply,
-- nothing is ever made visible. Its decisions are logged like the others.
--
-- 20261018000001 (applied in production) created the mode check inline;
-- it is replaced here, never edited there.
--
-- Rollback (do not run automatically; set CORPORATE_AUTO_VERIFY_MODE back
-- to shadow first, and delete or relabel 'hide_only' rows):
--   ALTER TABLE corporate_evidence_checks DROP CONSTRAINT IF EXISTS corporate_evidence_checks_mode_check;
--   ALTER TABLE corporate_evidence_checks ADD CONSTRAINT corporate_evidence_checks_mode_check
--     CHECK (mode IN ('shadow', 'live'));

ALTER TABLE corporate_evidence_checks DROP CONSTRAINT IF EXISTS corporate_evidence_checks_mode_check;
ALTER TABLE corporate_evidence_checks ADD CONSTRAINT corporate_evidence_checks_mode_check
    CHECK (mode IN ('shadow', 'hide_only', 'live'));
