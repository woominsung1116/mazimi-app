SET LOCAL lock_timeout = '5s';
-- sqlx runs this file in one transaction (it has no `-- no-transaction`
-- header), so the setting lasts until commit. Replacing the trigger locks
-- `programs`; behind a long ingestion write the migration fails after 5s
-- instead of queueing every reader behind it.

-- View counters (조회수, inqCnt, inqNum) change on every read of a public
-- catalogue without the programme changing. The worker leaves them out of
-- its change hash (`crates/worker/src/sources/mod.rs` `volatile_keys`), and
-- the AI extraction queue trigger now ignores them too: a renormalization
-- or a counter-only rewrite of `raw_payload` enqueues nothing.
--
-- `ingestion_stable_payload` is the same per-source list as
-- `volatile_keys`; the worker test
-- `pipeline::volatile_hash_tests::the_sql_stable_payload_matches_the_rust_volatile_keys`
-- fails when the two drift. No new table, so no RLS change.
--
-- Rollback (do not run automatically): recreate the trigger from
-- 20261012000001_ai_extraction_auto_verify.sql, then
--   DROP FUNCTION ingestion_stable_payload(text, jsonb);

CREATE FUNCTION ingestion_stable_payload(source TEXT, payload JSONB) RETURNS JSONB
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    SET search_path = pg_catalog, public AS $$
    SELECT CASE WHEN jsonb_typeof(payload) = 'object' THEN
        payload - (CASE source
            WHEN 'gov_benefits' THEN ARRAY['조회수']
            WHEN 'youth_center' THEN ARRAY['inqCnt']
            WHEN 'local_welfare' THEN ARRAY['inqNum']
            WHEN 'national_welfare' THEN ARRAY['inqNum']
            ELSE ARRAY[]::text[]
        END)
    ELSE payload END
$$;

DROP TRIGGER programs_enqueue_ai_extraction_update ON programs;

-- Only the columns the evidence source is built from
-- (`job::PROGRAM_TEXT_COLUMNS`); a sync that rewrites identical values, or
-- only a view counter, enqueues nothing.
CREATE TRIGGER programs_enqueue_ai_extraction_update
    AFTER UPDATE OF title, summary, application_method, submission_documents,
        screening_method, raw_payload, is_active ON programs
    FOR EACH ROW
    WHEN (OLD.title IS DISTINCT FROM NEW.title
       OR OLD.summary IS DISTINCT FROM NEW.summary
       OR OLD.application_method IS DISTINCT FROM NEW.application_method
       OR OLD.submission_documents IS DISTINCT FROM NEW.submission_documents
       OR OLD.screening_method IS DISTINCT FROM NEW.screening_method
       OR ingestion_stable_payload(OLD.source_type, OLD.raw_payload)
          IS DISTINCT FROM ingestion_stable_payload(NEW.source_type, NEW.raw_payload)
       OR (NEW.is_active AND NOT OLD.is_active))
    EXECUTE FUNCTION enqueue_ai_extraction();
