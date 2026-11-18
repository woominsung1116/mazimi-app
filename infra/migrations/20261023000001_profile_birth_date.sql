-- Date of birth for exact 만 나이 checks (v1 feedback F2.5).
--
-- `birth_year` stays: old clients send only it, and every reader keeps
-- working from it. When `birth_date` is set, `birth_year` is its year (the
-- API derives it; the constraint below keeps the two from disagreeing).
-- Personal data: the minimum the age rules need, never logged.
--
-- user_profiles already has RLS enabled and forced (20260322000001), with
-- the service-role and own-row policies covering every column.
--
-- Reversible: ALTER TABLE user_profiles DROP COLUMN birth_date;
ALTER TABLE user_profiles
    ADD COLUMN IF NOT EXISTS birth_date DATE,
    ADD CONSTRAINT user_profiles_birth_date_floor
        CHECK (birth_date IS NULL OR birth_date >= DATE '1920-01-01'),
    ADD CONSTRAINT user_profiles_birth_date_matches_year
        CHECK (birth_date IS NULL OR birth_year = EXTRACT(YEAR FROM birth_date)::int);

COMMENT ON COLUMN user_profiles.birth_date IS
    'Date of birth (YYYY-MM-DD) when the user gave it; birth_year is its year. NULL = only birth_year known.';
