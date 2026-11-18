-- Audit H1: only the coverage check (`condition_review::coverage::check`)
-- decides that a condition review is `complete`. Each write stores the
-- version of the check it passed; the list serves a `complete` row without
-- one (written before the check existed) as incomplete until it is
-- re-imported through the check. No row is changed here.
--
-- Reversible: ALTER TABLE program_condition_reviews DROP COLUMN coverage_version;
ALTER TABLE program_condition_reviews ADD COLUMN coverage_version smallint;
