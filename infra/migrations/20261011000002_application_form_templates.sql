-- 서류 작성 도우미: admin-defined input boxes for a verified official PDF
-- template. Positions only (page fractions, type, label, profile mapping);
-- never user answers or filled documents, which stay on the user's device.
-- A definition is bound to the template bytes it was drawn on: it is served
-- only while content_hash equals the resource's current content_hash, so a
-- changed official form silently stops using stale boxes.
--
-- Additive only. Rollback (do not run automatically): revert the API/web
-- consumers first, then DROP TABLE application_form_templates.
CREATE TABLE application_form_templates (
    resource_id  UUID        PRIMARY KEY REFERENCES program_application_resources(id) ON DELETE CASCADE,
    content_hash TEXT        NOT NULL CHECK (content_hash ~ '^[0-9a-f]{64}$'),
    page_count   INTEGER     NOT NULL CHECK (page_count BETWEEN 1 AND 50),
    source       TEXT        NOT NULL CHECK (source IN ('admin', 'acroform')),
    fields       JSONB       NOT NULL CHECK (
                     jsonb_typeof(fields) = 'array'
                     AND jsonb_array_length(fields) BETWEEN 1 AND 200
                 ),
    revision     BIGINT      NOT NULL DEFAULT 1 CHECK (revision > 0),
    updated_by   UUID        NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Service-only access, like every other worker/admin table.
ALTER TABLE application_form_templates ENABLE ROW LEVEL SECURITY;
ALTER TABLE application_form_templates FORCE ROW LEVEL SECURITY;
CREATE POLICY application_form_templates_service ON application_form_templates
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

-- Client roles never see this table (see 20260923000012).
DO $access$
DECLARE
  client_role text;
BEGIN
  REVOKE ALL ON TABLE public.application_form_templates FROM PUBLIC;
  FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
      EXECUTE format('REVOKE ALL ON TABLE public.application_form_templates FROM %I', client_role);
    END IF;
  END LOOP;
END $access$;

COMMENT ON TABLE application_form_templates IS
    'Input box positions for a verified PDF template, valid only while content_hash matches the resource. No user data.';
