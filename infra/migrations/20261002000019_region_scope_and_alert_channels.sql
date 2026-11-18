-- Add an explicit unresolved residence scope without converting missing
-- evidence into nationwide eligibility. No program/application row is removed.
DO $$
DECLARE was_validated boolean;
BEGIN
    SELECT convalidated INTO was_validated FROM pg_constraint
    WHERE conrelid = 'programs'::regclass AND conname = 'programs_regions_quality_check';
    ALTER TABLE programs DROP CONSTRAINT programs_regions_quality_check;
    ALTER TABLE programs ADD CONSTRAINT programs_regions_quality_check CHECK (
        regions IS NULL OR regions = ARRAY['unknown']::TEXT[] OR regions <@ ARRAY[
            'seoul', 'busan', 'daegu', 'incheon', 'gwangju', 'daejeon',
            'ulsan', 'sejong', 'gyeonggi', 'gangwon', 'chungbuk', 'chungnam',
            'jeonbuk', 'jeonnam', 'gyeongbuk', 'gyeongnam', 'jeju'
        ]::TEXT[]
    ) NOT VALID;
    IF was_validated THEN
        ALTER TABLE programs VALIDATE CONSTRAINT programs_regions_quality_check;
    END IF;
END $$;

-- Delivery history must survive while in-app opt-out is respected at the time
-- of sending. Existing deliveries retain their previous visibility.
ALTER TABLE alert_deliveries ADD COLUMN in_app_visible boolean NOT NULL DEFAULT true;
COMMENT ON COLUMN alert_deliveries.in_app_visible IS
    'Notification channel preference captured when this delivery was created.';

-- Rollback preconditions: stop new workers; retain this additive schema for
-- old binaries. To contract the CHECK, first resolve all ['unknown'] scopes
-- from evidence (never replace them with []). The visibility column can stay
-- during binary rollback; dropping it discards channel history and is not an
-- automatic rollback action.
