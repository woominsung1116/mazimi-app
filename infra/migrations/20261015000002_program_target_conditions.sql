-- Who a programme is for, as the sources code it (add-column only, nullable).
-- Written by the worker normalizers; NULL always means "the source does not
-- say", never "no restriction checked".
--   applicant_types         보조금24 사용자구분: individual, household,
--                           small_business, organization.
--   target_groups           personal characteristics the programme is limited
--                           to, any one of which qualifies (보조금24
--                           supportConditions JA03xx, 온통청년 sbizCd), e.g.
--                           disabled, veteran, farmer, pregnant, job_seeker.
--   target_household_types  household types, any one of which qualifies
--                           (JA04xx), e.g. multicultural, single_parent_or_grandparent.
--   target_gender           'male' or 'female' when only one is eligible.
--   income_median_bands     accepted 기준 중위소득 bands when not all of them:
--                           '0-50', '51-75', '76-100', '101-200', '200+'.
-- No new table, so no new RLS policy; programs keeps its existing policies.
--
-- Rollback (do not run automatically): revert the worker first, then
-- ALTER TABLE programs DROP COLUMN applicant_types, DROP COLUMN target_groups,
-- DROP COLUMN target_household_types, DROP COLUMN target_gender,
-- DROP COLUMN income_median_bands.
ALTER TABLE programs
    ADD COLUMN IF NOT EXISTS applicant_types        text[],
    ADD COLUMN IF NOT EXISTS target_groups          text[],
    ADD COLUMN IF NOT EXISTS target_household_types text[],
    ADD COLUMN IF NOT EXISTS target_gender          text,
    ADD COLUMN IF NOT EXISTS income_median_bands    text[];

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conrelid = 'programs'::regclass AND conname = 'programs_target_gender_check'
    ) THEN
        ALTER TABLE programs ADD CONSTRAINT programs_target_gender_check
            CHECK (target_gender IS NULL OR target_gender IN ('male', 'female'));
    END IF;
END
$$;

COMMENT ON COLUMN programs.applicant_types IS
  'Who may apply (individual, household, small_business, organization). NULL = source does not say.';
COMMENT ON COLUMN programs.target_groups IS
  'Personal characteristics the programme is limited to; any one qualifies. NULL = no restriction stated.';
COMMENT ON COLUMN programs.target_household_types IS
  'Household types the programme is limited to; any one qualifies. NULL = no restriction stated.';
COMMENT ON COLUMN programs.target_gender IS
  'male or female when only one is eligible. NULL = either or not stated.';
COMMENT ON COLUMN programs.income_median_bands IS
  'Accepted 기준 중위소득 bands when not all of them. NULL = no band restriction stated.';
