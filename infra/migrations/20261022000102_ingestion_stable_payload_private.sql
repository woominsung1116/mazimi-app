-- `ingestion_stable_payload` (20261021000001) was created with the default
-- EXECUTE for PUBLIC. Its callers are the AI queue trigger on `programs`,
-- the worker and the API, all connecting as the database owner. Checked on
-- production (read-only, 2026-10-22): INSERT/UPDATE on `programs` is held
-- only by the owner and `service_role`, which has its own EXECUTE grant;
-- `anon` and `authenticated` cannot write `programs` and no view or policy
-- calls the function. So PUBLIC loses EXECUTE; `service_role` keeps it
-- where the role exists (the trigger runs as the writing role).
--
-- Rollback (do not run automatically):
--   GRANT EXECUTE ON FUNCTION ingestion_stable_payload(text, jsonb) TO PUBLIC;

REVOKE ALL ON FUNCTION ingestion_stable_payload(text, jsonb) FROM PUBLIC;

DO $grant$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'service_role') THEN
        GRANT EXECUTE ON FUNCTION ingestion_stable_payload(text, jsonb) TO service_role;
    END IF;
END
$grant$;
