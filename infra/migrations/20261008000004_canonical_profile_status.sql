-- Profiles saved before the API normalised enrollment/employment answers hold
-- client vocabulary ("재학", "대학생", "재직"). Hard filters compare canonical
-- codes exactly, so those users were excluded from every program that asks
-- about enrollment or employment. Rewrite the stored answers in place.
--
-- Values nobody can map become NULL (unknown), which the recommender treats as
-- "needs confirmation", never as a contradiction. Every row this migration
-- rewrites is first copied to user_profile_status_backup, so the rewrite can
-- be reversed:
--   UPDATE user_profiles p
--   SET enrollment_status = b.enrollment_status, employment_status = b.employment_status
--   FROM user_profile_status_backup b WHERE b.user_id = p.user_id;
-- The backup is owner-only (RLS forced, service policy) and cascades with the
-- user, so account deletion still erases it.
--
-- Retention: the backup table exists only to reverse this migration. Drop it
-- with a follow-up migration once the deploy has been stable for at least
-- 30 days (DROP TABLE IF EXISTS user_profile_status_backup); it holds the
-- pre-migration answers of real accounts and must not outlive its purpose.
--
-- Order matters: the backup INSERT and the UPDATEs run before RLS is forced on
-- the backup table, so the migration does not depend on the connection role
-- satisfying is_service_role(). The alias lists below are a copy of
-- mazimi_core::canonical_*_status; a core test asserts they stay a subset.
CREATE TABLE IF NOT EXISTS user_profile_status_backup (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    enrollment_status TEXT,
    employment_status TEXT,
    backed_up_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO user_profile_status_backup (user_id, enrollment_status, employment_status)
SELECT user_id, enrollment_status, employment_status
FROM user_profiles
WHERE (enrollment_status IS NOT NULL
       AND enrollment_status NOT IN ('enrolled', 'on_leave', 'graduated', 'research_student'))
   OR (employment_status IS NOT NULL
       AND employment_status NOT IN (
           'employed', 'new_hire', 'unemployed', 'preparing', 'self_employed', 'student', 'other'))
ON CONFLICT (user_id) DO NOTHING;

UPDATE user_profiles
SET enrollment_status = CASE lower(btrim(enrollment_status))
        WHEN 'enrolled' THEN 'enrolled'
        WHEN '재학' THEN 'enrolled'
        WHEN '재학생' THEN 'enrolled'
        WHEN '대학생' THEN 'enrolled'
        WHEN '재학중' THEN 'enrolled'
        WHEN '재학 중' THEN 'enrolled'
        WHEN '졸업예정' THEN 'enrolled'
        WHEN 'on_leave' THEN 'on_leave'
        WHEN 'leave_of_absence' THEN 'on_leave'
        WHEN '휴학' THEN 'on_leave'
        WHEN '휴학생' THEN 'on_leave'
        WHEN '휴학중' THEN 'on_leave'
        WHEN '휴학 중' THEN 'on_leave'
        WHEN 'graduated' THEN 'graduated'
        WHEN '졸업' THEN 'graduated'
        WHEN '졸업생' THEN 'graduated'
        WHEN 'research_student' THEN 'research_student'
        WHEN '수료후연구생' THEN 'research_student'
        WHEN '수료 후 연구생' THEN 'research_student'
    END,
    -- The web onboarding step offered "취업준비생" as an enrollment option.
    -- It is an employment answer; move it when no employment answer exists.
    employment_status = CASE
        WHEN employment_status IS NULL OR btrim(employment_status) = ''
            THEN CASE btrim(enrollment_status) WHEN '취업준비생' THEN 'preparing' ELSE employment_status END
        ELSE employment_status
    END
WHERE enrollment_status IS NOT NULL
  AND enrollment_status NOT IN ('enrolled', 'on_leave', 'graduated', 'research_student');

UPDATE user_profiles
SET employment_status = CASE lower(btrim(employment_status))
        WHEN 'employed' THEN 'employed'
        WHEN '재직' THEN 'employed'
        WHEN '재직자' THEN 'employed'
        WHEN '직장인' THEN 'employed'
        WHEN '재직중' THEN 'employed'
        WHEN '재직 중' THEN 'employed'
        WHEN 'new_hire' THEN 'new_hire'
        WHEN '신입' THEN 'new_hire'
        WHEN '신규입사' THEN 'new_hire'
        WHEN '신규 입사' THEN 'new_hire'
        WHEN 'unemployed' THEN 'unemployed'
        WHEN '미취업' THEN 'unemployed'
        WHEN '무직' THEN 'unemployed'
        WHEN 'preparing' THEN 'preparing'
        WHEN '취업준비생' THEN 'preparing'
        WHEN '취준생' THEN 'preparing'
        WHEN '구직중' THEN 'preparing'
        WHEN '구직 중' THEN 'preparing'
        WHEN '취업준비' THEN 'preparing'
        WHEN 'self_employed' THEN 'self_employed'
        WHEN 'freelancer' THEN 'self_employed'
        WHEN '자영업' THEN 'self_employed'
        WHEN '자영업자' THEN 'self_employed'
        WHEN '프리랜서' THEN 'self_employed'
        WHEN 'student' THEN 'student'
        WHEN '학생' THEN 'student'
        WHEN 'other' THEN 'other'
        WHEN '기타' THEN 'other'
    END
WHERE employment_status IS NOT NULL
  AND employment_status NOT IN (
      'employed', 'new_hire', 'unemployed', 'preparing', 'self_employed', 'student', 'other'
  );

-- Lock the backup down last, once its rows are written.
ALTER TABLE user_profile_status_backup ENABLE ROW LEVEL SECURITY;
ALTER TABLE user_profile_status_backup FORCE ROW LEVEL SECURITY;
REVOKE ALL ON TABLE user_profile_status_backup FROM PUBLIC;
DO $rls$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE user_profile_status_backup FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE user_profile_status_backup FROM authenticated;
  END IF;
END
$rls$;
DROP POLICY IF EXISTS user_profile_status_backup_all_service ON user_profile_status_backup;
CREATE POLICY user_profile_status_backup_all_service ON user_profile_status_backup
  FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());
