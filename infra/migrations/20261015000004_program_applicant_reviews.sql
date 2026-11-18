-- Who applies for a programme, decided by people who read the notice. 34% of
-- measured top-20 recommendations were programmes no individual can apply to
-- (employer subsidies, university/consortium grants, institutional mandates);
-- the text rules in crates/core/src/audience.rs stay as they are and this
-- stored verdict is applied on top of them when a list is served:
--   organisation / business  -> hidden as "not individual"
--   indirect                 -> listed lower, with a "참여기관을 통해 참여" note
--   individual / info_only / unclear -> no change
-- A review never lists a row the text rules hide.
--
-- text_hash is mazimi_core::applicant_review::text_hash over the source text
-- the reviewers read. When the current raw_payload hashes differently the
-- review is stale and ignored until the new text is reviewed.
-- evidence is a verbatim quote of that text (checked on every write).
--
-- Written only by the worker importer and AdminUser routes, both as the
-- service role. Keyed by the programme's source identity, so a review
-- survives re-ingestion of the same notice.
--
-- Additive only. Rollback (do not run automatically): revert the API/worker
-- consumers first, then DROP TABLE program_applicant_review_log,
-- program_applicant_reviews; DROP FUNCTION
-- program_applicant_review_log_immutable().
CREATE TABLE program_applicant_reviews (
    source_type TEXT        NOT NULL CHECK (char_length(source_type) BETWEEN 1 AND 100),
    source_id   TEXT        NOT NULL CHECK (char_length(source_id) BETWEEN 1 AND 200),
    class       TEXT        NOT NULL CHECK (class IN ('individual','indirect','organisation','business','info_only','unclear')),
    evidence    TEXT        NOT NULL CHECK (char_length(evidence) BETWEEN 1 AND 2000),
    reviewer    TEXT        NOT NULL CHECK (char_length(reviewer) BETWEEN 1 AND 200),
    text_hash   TEXT        NOT NULL CHECK (text_hash ~ '^[0-9a-f]{64}$'),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (source_type, source_id)
);

-- Append-only change log: every write and delete, with who made it
-- ('admin:<user id>' from the verified JWT, or 'import:<file name>').
CREATE TABLE program_applicant_review_log (
    id          BIGSERIAL   PRIMARY KEY,
    source_type TEXT        NOT NULL,
    source_id   TEXT        NOT NULL,
    action      TEXT        NOT NULL CHECK (action IN ('upsert','delete')),
    class       TEXT        CHECK (class IS NULL OR class IN ('individual','indirect','organisation','business','info_only','unclear')),
    previous_class TEXT     CHECK (previous_class IS NULL OR previous_class IN ('individual','indirect','organisation','business','info_only','unclear')),
    evidence    TEXT,
    reviewer    TEXT,
    text_hash   TEXT        CHECK (text_hash IS NULL OR text_hash ~ '^[0-9a-f]{64}$'),
    actor       TEXT        NOT NULL CHECK (char_length(actor) BETWEEN 1 AND 300),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX program_applicant_review_log_subject
    ON program_applicant_review_log (source_type, source_id, created_at DESC);

-- Append-only in the database, not only by convention: no role may change
-- or remove a log row (TRUNCATE stays an owner-only maintenance action).
CREATE FUNCTION program_applicant_review_log_immutable() RETURNS trigger
    LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
BEGIN
    RAISE EXCEPTION 'program_applicant_review_log is append-only';
END $$;
CREATE TRIGGER program_applicant_review_log_no_mutation
    BEFORE UPDATE OR DELETE ON program_applicant_review_log
    FOR EACH ROW EXECUTE FUNCTION program_applicant_review_log_immutable();
REVOKE ALL ON FUNCTION program_applicant_review_log_immutable() FROM PUBLIC;

DO $access$
DECLARE
  t text;
  client_role text;
BEGIN
  FOREACH t IN ARRAY ARRAY['program_applicant_reviews','program_applicant_review_log'] LOOP
    EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format('CREATE POLICY %I ON public.%I FOR ALL USING (is_service_role()) WITH CHECK (is_service_role())',
                   t || '_service', t);
    EXECUTE format('REVOKE ALL ON TABLE public.%I FROM PUBLIC', t);
    FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
      IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
        EXECUTE format('REVOKE ALL ON TABLE public.%I FROM %I', t, client_role);
      END IF;
    END LOOP;
  END LOOP;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON SEQUENCE public.program_applicant_review_log_id_seq FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON SEQUENCE public.program_applicant_review_log_id_seq FROM authenticated;
  END IF;
END $access$;

COMMENT ON TABLE program_applicant_reviews IS
    'Reviewed applicant class per programme source row; applied at serve time only while text_hash matches the current raw_payload. Hide/demote only.';
COMMENT ON TABLE program_applicant_review_log IS
    'Append-only history of program_applicant_reviews writes and deletes.';
