-- Daily database size readings, so the storage job can compute a growth rate
-- and project the days left before the plan limit. `worker_daily_jobs.details`
-- is overwritten every run, so it cannot hold a history.
--
-- One row per storage run (a few per day at most), sizes only: no user data.
-- The storage job keeps 400 days and deletes older rows itself.
-- RLS forced, service-role policy only, client roles revoked (same pattern as
-- the other internal operational tables).
-- Additive only. Rollback (do not run automatically): DROP TABLE storage_size_history.

CREATE TABLE storage_size_history (
    id          BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    measured_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    db_bytes    BIGINT      NOT NULL CHECK (db_bytes >= 0),
    -- {"table_name": total_relation_bytes} for the largest tables.
    top_tables  JSONB       NOT NULL DEFAULT '{}'::jsonb
);
CREATE INDEX storage_size_history_measured_at ON storage_size_history (measured_at);

ALTER TABLE storage_size_history ENABLE ROW LEVEL SECURITY;
ALTER TABLE storage_size_history FORCE ROW LEVEL SECURITY;
CREATE POLICY storage_size_history_service ON storage_size_history FOR ALL
    USING (is_service_role()) WITH CHECK (is_service_role());

DO $access$
DECLARE
  client_role text;
BEGIN
  REVOKE ALL ON TABLE public.storage_size_history FROM PUBLIC;
  FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
      EXECUTE format('REVOKE ALL ON TABLE public.storage_size_history FROM %I', client_role);
      EXECUTE format('REVOKE ALL ON SEQUENCE public.storage_size_history_id_seq FROM %I', client_role);
    END IF;
  END LOOP;
END $access$;

COMMENT ON TABLE storage_size_history IS
  'Storage job size readings (database bytes, largest tables) for growth and days-to-limit; kept 400 days.';
