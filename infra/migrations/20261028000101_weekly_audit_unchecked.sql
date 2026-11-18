-- Weekly audit: a programme whose page the extractor does not read by
-- design (a membership, not a student or youth offer) is recorded with
-- reason class 'unchecked', counted apart from pages that could not be
-- read. Additive: the constraint only gains a value.
--
-- Rollback (no row uses the value before this migration):
--   ALTER TABLE corporate_weekly_audit_verdicts
--     DROP CONSTRAINT corporate_weekly_audit_verdicts_reason_class_check,
--     ADD CONSTRAINT corporate_weekly_audit_verdicts_reason_class_check
--       CHECK (reason_class IS NULL OR reason_class IN ('policy', 'transient', 'no_offer', 'no_source'));

ALTER TABLE corporate_weekly_audit_verdicts
    DROP CONSTRAINT IF EXISTS corporate_weekly_audit_verdicts_reason_class_check,
    ADD CONSTRAINT corporate_weekly_audit_verdicts_reason_class_check
        CHECK (reason_class IS NULL OR reason_class IN ('policy', 'transient', 'no_offer', 'no_source', 'unchecked'));
