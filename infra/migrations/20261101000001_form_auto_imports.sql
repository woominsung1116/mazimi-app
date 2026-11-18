-- Audit of automatic form-template imports (scripts/forms/auto, api::form_auto_import).
-- A template reviewed with zero findings is published without a human step; every
-- import, refusal and rollback is one append-only row naming the deciding reviewer, the
-- verdict file sha256 and the template sha256 it was bound to.
-- One state machine (api::form_auto_import):
--   publish   a 'publishing' row is written BEFORE anything is published, carrying
--             post_check_pending while fewer than POST_CHECKS post-checks came back ok; the
--             outcome ('import' | 'up_to_date' | 'refused') follows with attempt = that row.
--             A crash between the two leaves the attempt pending: a post-check is never lost.
--   pending   a flagged 'publishing' row with no 'up_to_date'/'refused' outcome and no
--             'post_check' answer. While MAX_PENDING are pending, new imports are refused.
--   post_check  one row per attempt (ok | rollback), written after the rollback succeeded.
--   rollback  unpublish, un-verify a resource the importer verified, then this row, which also
--             refuses these bytes from then on; each step re-checks the actual state.
CREATE TABLE form_auto_imports (
    id              BIGSERIAL   PRIMARY KEY,
    action          TEXT        NOT NULL CHECK (action IN ('publishing', 'import', 'up_to_date', 'refused', 'rollback', 'post_check')),
    -- Values, not foreign keys: an audit row describes what was imported even after the
    -- programme or resource is deleted (the daily pipeline deletes programmes; a cascading SET
    -- NULL would be an UPDATE, which the append-only trigger refuses, failing the delete).
    resource_id     UUID,
    program_id      UUID,
    slug            TEXT        NOT NULL CHECK (char_length(slug) BETWEEN 1 AND 64),
    reviewer        TEXT        NOT NULL CHECK (reviewer IN ('claude-reviewer', 'judge', 'claude-post-check', '-')),
    verdict_sha256  TEXT        CHECK (verdict_sha256 ~ '^[0-9a-f]{64}$'),
    template_sha256 TEXT        CHECK (template_sha256 ~ '^[0-9a-f]{64}$'),
    -- The official file the boxes were bound to; a rollback row refuses these bytes from then on.
    official_sha256 TEXT        CHECK (official_sha256 ~ '^[0-9a-f]{64}$'),
    run             TEXT        NOT NULL CHECK (char_length(run) BETWEEN 1 AND 80),
    revision        BIGINT,
    actor           UUID        NOT NULL,
    detail          TEXT        NOT NULL DEFAULT '' CHECK (char_length(detail) <= 500),
    post_check_pending BOOLEAN  NOT NULL DEFAULT false,
    -- The 'publishing' row an outcome row belongs to.
    attempt         BIGINT      REFERENCES form_auto_imports(id),
    -- The 'publishing' row a post_check row answers.
    checks_import   BIGINT      REFERENCES form_auto_imports(id),
    post_check      TEXT        CHECK (post_check IN ('ok', 'rollback')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (action <> 'import' OR (resource_id IS NOT NULL AND verdict_sha256 IS NOT NULL AND template_sha256 IS NOT NULL AND revision IS NOT NULL)),
    CHECK (action <> 'publishing' OR (resource_id IS NOT NULL AND verdict_sha256 IS NOT NULL AND template_sha256 IS NOT NULL)),
    CHECK (action NOT IN ('import', 'up_to_date') OR attempt IS NOT NULL),
    CHECK (NOT post_check_pending OR action = 'publishing'),
    CHECK ((action = 'post_check') = (checks_import IS NOT NULL AND post_check IS NOT NULL))
);
CREATE INDEX form_auto_imports_resource ON form_auto_imports (resource_id, created_at DESC);
-- One answer per import.
CREATE INDEX form_auto_imports_attempt ON form_auto_imports (attempt) WHERE attempt IS NOT NULL;
CREATE UNIQUE INDEX form_auto_imports_one_post_check ON form_auto_imports (checks_import) WHERE action = 'post_check';
CREATE INDEX form_auto_imports_rollbacks ON form_auto_imports (resource_id, official_sha256) WHERE action = 'rollback';

CREATE FUNCTION form_auto_imports_immutable() RETURNS trigger
    LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
BEGIN
    RAISE EXCEPTION 'form_auto_imports is append-only';
END $$;
CREATE TRIGGER form_auto_imports_no_mutation
    BEFORE UPDATE OR DELETE ON form_auto_imports
    FOR EACH ROW EXECUTE FUNCTION form_auto_imports_immutable();
CREATE TRIGGER form_auto_imports_no_truncate
    BEFORE TRUNCATE ON form_auto_imports
    FOR EACH STATEMENT EXECUTE FUNCTION form_auto_imports_immutable();
REVOKE ALL ON FUNCTION form_auto_imports_immutable() FROM PUBLIC;

DO $access$
DECLARE
  client_role text;
BEGIN
  ALTER TABLE public.form_auto_imports ENABLE ROW LEVEL SECURITY;
  ALTER TABLE public.form_auto_imports FORCE ROW LEVEL SECURITY;
  CREATE POLICY form_auto_imports_service ON public.form_auto_imports
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());
  REVOKE ALL ON TABLE public.form_auto_imports FROM PUBLIC;
  FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
      EXECUTE format('REVOKE ALL ON TABLE public.form_auto_imports FROM %I', client_role);
      EXECUTE format('REVOKE ALL ON SEQUENCE public.form_auto_imports_id_seq FROM %I', client_role);
    END IF;
  END LOOP;
END $access$;

COMMENT ON TABLE form_auto_imports IS
    'Append-only audit of automatic form-template imports: deciding reviewer, verdict and template sha256, rollbacks.';
