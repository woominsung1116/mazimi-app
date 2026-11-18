-- Refresh tokens become server-tracked so a session can end before the 30-day
-- JWT expiry: logout revokes, /auth/refresh rotates (one use per jti), and a
-- replayed jti revokes every session of that account. Rows cascade with the
-- account. Only the API role reads or writes this table.
CREATE TABLE IF NOT EXISTS refresh_tokens (
    jti         UUID        PRIMARY KEY,
    user_id     UUID        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at  TIMESTAMPTZ NOT NULL,
    revoked_at  TIMESTAMPTZ,
    replaced_by UUID,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS refresh_tokens_user_id_idx ON refresh_tokens (user_id);
CREATE INDEX IF NOT EXISTS refresh_tokens_expires_at_idx ON refresh_tokens (expires_at);

ALTER TABLE refresh_tokens ENABLE ROW LEVEL SECURITY;
ALTER TABLE refresh_tokens FORCE ROW LEVEL SECURITY;
DROP POLICY IF EXISTS refresh_tokens_all_service ON refresh_tokens;
CREATE POLICY refresh_tokens_all_service ON refresh_tokens
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

-- Normalize the remaining tables to the FORCE + is_service_role() contract
-- used everywhere else (20260322000001, 20260923000012). The API and worker
-- connect as the table owner, so FORCE is only safe once a service policy
-- exists; user_documents lost its (over-broad) service policy in
-- 20260923000012 and gets a conventional one here. The three ingestion
-- tables never had RLS. Every statement is idempotent.
DO $rls$
DECLARE
  table_name text;
BEGIN
  FOREACH table_name IN ARRAY ARRAY[
    'subscription_plans','user_subscriptions','user_documents',
    'ingestion_runs','ingestion_items','source_snapshots','refresh_tokens'
  ] LOOP
    EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', table_name);
    EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY', table_name);
    EXECUTE format('REVOKE ALL ON TABLE public.%I FROM PUBLIC', table_name);
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='anon') THEN
      EXECUTE format('REVOKE ALL ON TABLE public.%I FROM anon', table_name);
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname='authenticated') THEN
      EXECUTE format('REVOKE ALL ON TABLE public.%I FROM authenticated', table_name);
    END IF;
  END LOOP;
END $rls$;

DROP POLICY IF EXISTS user_documents_all_service ON user_documents;
CREATE POLICY user_documents_all_service ON user_documents
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

DROP POLICY IF EXISTS ingestion_runs_all_service ON ingestion_runs;
CREATE POLICY ingestion_runs_all_service ON ingestion_runs
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

DROP POLICY IF EXISTS ingestion_items_all_service ON ingestion_items;
CREATE POLICY ingestion_items_all_service ON ingestion_items
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

DROP POLICY IF EXISTS source_snapshots_all_service ON source_snapshots;
CREATE POLICY source_snapshots_all_service ON source_snapshots
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

-- Rollback: additive only. Old API binaries ignore refresh_tokens (their
-- refresh tokens carry no jti and the new binary rejects them, so a rollback
-- re-enables stateless refresh for tokens the old binary issues). Dropping the
-- table or the FORCE flags discards revocation history and is not automatic.
