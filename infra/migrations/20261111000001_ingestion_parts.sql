-- Per-part state of a source read in parts (finlife: one part per product
-- kind × sector, named by the id prefix of its records). Written by the
-- worker after every ingestion run (crates/worker/src/pipeline.rs
-- `persist_parts`): the last outcome, since when a part has been unsettled
-- and for how many runs in a row, and the counts that otherwise exist only
-- in log lines. Alerts and the daily digest read it. Additive; service role
-- only (RLS forced). Nothing here is shown to users.
--
-- Rollback: DROP TABLE IF EXISTS ingestion_parts;

CREATE TABLE IF NOT EXISTS ingestion_parts (
    source_name                TEXT        NOT NULL CHECK (char_length(source_name) BETWEEN 1 AND 100),
    id_prefix                  TEXT        NOT NULL CHECK (char_length(id_prefix) BETWEEN 1 AND 200),
    state                      TEXT        NOT NULL CHECK (state IN ('settled', 'empty', 'unsettled')),
    reason_code                TEXT        CHECK (reason_code IS NULL OR char_length(reason_code) <= 40),
    reason_detail              TEXT        CHECK (reason_detail IS NULL OR char_length(reason_detail) <= 300),
    unsettled_since            TIMESTAMPTZ,
    consecutive_unsettled_runs INT         NOT NULL DEFAULT 0 CHECK (consecutive_unsettled_runs >= 0),
    last_settled_at            TIMESTAMPTZ,
    last_seen                  INT         NOT NULL DEFAULT 0 CHECK (last_seen >= 0),
    last_kept                  INT         NOT NULL DEFAULT 0 CHECK (last_kept >= 0),
    last_set_aside             INT         NOT NULL DEFAULT 0 CHECK (last_set_aside >= 0),
    shown                      INT         NOT NULL DEFAULT 0 CHECK (shown >= 0),
    last_run_id                UUID,
    updated_at                 TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (source_name, id_prefix),
    CHECK ((state = 'unsettled') = (reason_code IS NOT NULL))
);

ALTER TABLE ingestion_parts ENABLE ROW LEVEL SECURITY;
ALTER TABLE ingestion_parts FORCE ROW LEVEL SECURITY;
REVOKE ALL ON TABLE ingestion_parts FROM PUBLIC;
DO $rls$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE ingestion_parts FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE ingestion_parts FROM authenticated;
  END IF;
END
$rls$;
DROP POLICY IF EXISTS ingestion_parts_all_service ON ingestion_parts;
CREATE POLICY ingestion_parts_all_service ON ingestion_parts
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

COMMENT ON TABLE ingestion_parts IS
    'Last outcome of each part of a source read in parts (id prefix), with since when and how often in a row it has been unsettled. Internal.';
