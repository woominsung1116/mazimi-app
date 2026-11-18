-- Clear encrypted answers left by the removed connector-submission feature.
--
-- application_submissions.encrypted_answers (20260926000018) held the answers
-- a user agreed to forward to an institution's intake system. The app no
-- longer forwards anything on the user's behalf, so no code reads or writes
-- the column, and the queued answers it still holds are personal data without
-- a processing purpose. maintain_application_submissions() only clears rows
-- that expire or leave the queued state; rows that were queued when the
-- feature was removed keep their payload until this statement runs.
--
-- Idempotent: a second run finds no non-NULL payload and changes nothing. The
-- table and column are checked first so the migration also applies cleanly to
-- a database created after the table is dropped. The column itself is kept;
-- dropping it is a separate, coordinated schema change.
DO $clear$
BEGIN
  IF EXISTS (
      SELECT 1 FROM information_schema.columns
      WHERE table_schema = current_schema()
        AND table_name = 'application_submissions'
        AND column_name = 'encrypted_answers'
  ) THEN
    UPDATE application_submissions
    SET encrypted_answers = NULL, updated_at = now()
    WHERE encrypted_answers IS NOT NULL;
  END IF;
END
$clear$;
