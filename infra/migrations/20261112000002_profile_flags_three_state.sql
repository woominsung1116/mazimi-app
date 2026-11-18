-- Group flags three-state at rest (review-core-arch M1), as an expand step
-- that the running API and a plain image rollback both survive (review
-- B-M2).
--
-- The four flags (has_disability, is_multicultural_family,
-- is_low_income_household, veteran_family) were saved as `false` when
-- unanswered, so an answered "no" could not be told apart. No client has
-- ever asked them, so every stored `false` is unanswered.
--
-- Expand: four nullable answer columns, NULL = not answered, false =
-- answered "no", true = answered "yes". A stored `true` is copied; a stored
-- `false` stays NULL. The old columns keep their type, default and values:
-- the API that runs while this migrates, and an older image after a
-- rollback, read and write them exactly as before. The new API reads only
-- the answer columns and writes both (the old column as
-- COALESCE(answer, false)), so the two never disagree on a "yes".
--
-- Contract (a later release, once no image older than this one can be
-- deployed): drop the four old columns and rename the answer columns.
--
-- user_profiles keeps its own-row RLS policies (20260322000001); no new
-- table, no row deleted, no existing value changed.
--
-- Reversible: ALTER TABLE user_profiles DROP COLUMN has_disability_answer,
--   DROP COLUMN is_multicultural_family_answer,
--   DROP COLUMN is_low_income_household_answer, DROP COLUMN veteran_family_answer;
-- (an image older than this release ignores the answer columns, so it needs
-- no reverse step to run).
ALTER TABLE user_profiles
    ADD COLUMN has_disability_answer boolean,
    ADD COLUMN is_multicultural_family_answer boolean,
    ADD COLUMN is_low_income_household_answer boolean,
    ADD COLUMN veteran_family_answer boolean;

UPDATE user_profiles
SET has_disability_answer = CASE WHEN has_disability THEN true END,
    is_multicultural_family_answer = CASE WHEN is_multicultural_family THEN true END,
    is_low_income_household_answer = CASE WHEN is_low_income_household THEN true END,
    veteran_family_answer = CASE WHEN veteran_family THEN true END
WHERE has_disability OR is_multicultural_family OR is_low_income_household OR veteran_family;
