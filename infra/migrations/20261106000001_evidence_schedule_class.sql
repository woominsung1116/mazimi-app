-- The re-verification class (corporate_auto::reverify) a source's next check
-- was scheduled under. When the classifier later reads the stored outcome
-- in another class, the wait it set is stale and the source is due at once
-- (evidence::reschedule_stale). NULL: scheduled before classes were recorded.
--
-- Rollback: ALTER TABLE corporate_evidence_targets DROP COLUMN IF EXISTS schedule_class;
ALTER TABLE corporate_evidence_targets
    ADD COLUMN IF NOT EXISTS schedule_class TEXT
        CHECK (schedule_class IS NULL OR schedule_class IN ('confirmed', 'refuted', 'transient', 'blocked'));
