-- 서류 작성 도우미 for official HWP/HWPX forms. The app can only overlay
-- boxes on a PDF, so an admin uploads a PDF converted from the verified
-- official file and Mazimi serves it itself. Two hashes bind it:
--   source_sha256  the official HWP/HWPX bytes the worker verified
--                  (program_application_resources.content_hash)
--   content_hash   the converted PDF bytes stored here, which the app checks
--                  after download and the boxes were drawn on.
-- A changed official file (new content_hash) stops both the PDF and the
-- boxes from being served, exactly like a stale PDF template today.
-- Blank official forms only: never a filled document or any user value.
--
-- Additive only. Rollback (do not run automatically): revert the API/app
-- consumers first, then DROP TABLE application_form_derived_pdfs and
-- ALTER TABLE application_form_templates DROP COLUMN source_sha256,
-- DROP COLUMN converter.
CREATE TABLE application_form_derived_pdfs (
    resource_id   UUID        PRIMARY KEY REFERENCES program_application_resources(id) ON DELETE CASCADE,
    source_sha256 TEXT        NOT NULL CHECK (source_sha256 ~ '^[0-9a-f]{64}$'),
    converter     TEXT        NOT NULL CHECK (converter ~ '^[ -~]{1,200}$'),
    content_hash  TEXT        NOT NULL CHECK (content_hash ~ '^[0-9a-f]{64}$' AND content_hash <> source_sha256),
    byte_size     INTEGER     NOT NULL CHECK (byte_size BETWEEN 1 AND 8388608),
    pdf           BYTEA       NOT NULL CHECK (octet_length(pdf) = byte_size),
    created_by    UUID        NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE application_form_derived_pdfs ENABLE ROW LEVEL SECURITY;
ALTER TABLE application_form_derived_pdfs FORCE ROW LEVEL SECURITY;
CREATE POLICY application_form_derived_pdfs_service ON application_form_derived_pdfs
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

DO $access$
DECLARE
  client_role text;
BEGIN
  REVOKE ALL ON TABLE public.application_form_derived_pdfs FROM PUBLIC;
  FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
      EXECUTE format('REVOKE ALL ON TABLE public.application_form_derived_pdfs FROM %I', client_role);
    END IF;
  END LOOP;
END $access$;

COMMENT ON TABLE application_form_derived_pdfs IS
    'Blank PDF converted from a verified official HWP/HWPX, served only while source_sha256 matches the resource. No user data.';

-- Boxes drawn on a derived PDF record which official file they belong to.
ALTER TABLE application_form_templates
    ADD COLUMN source_sha256 TEXT CHECK (source_sha256 ~ '^[0-9a-f]{64}$'),
    ADD COLUMN converter     TEXT CHECK (converter ~ '^[ -~]{1,200}$'),
    ADD CONSTRAINT application_form_templates_derived_pair
        CHECK ((source_sha256 IS NULL) = (converter IS NULL));
