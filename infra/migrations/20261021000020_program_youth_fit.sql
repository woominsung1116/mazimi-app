-- Whether a published programme is for a young individual (youth_fit).
--
-- About a quarter of the visible 보조금24 catalogue is for farms, firms,
-- institutions or people over 40, and about a tenth is for children only.
-- mazimi_core::youth_fit classifies each row from its stored columns and
-- raw payload; the worker stores the verdict here. Demoted rows
-- (verdict <> 'fit') stay searchable and reachable by link, but leave the
-- default catalogue list, the default sort and the recommendation
-- candidates; business_only rows stay for self-employed users,
-- disability_only rows for users with the disability flag, child_only rows
-- in the recommendations of users aged 25+. Nothing here hides or changes a
-- programme or its raw payload. Works without any human review: verdicts
-- are recomputed on every input or classifier change.
--
-- verdict/target/rule/evidence are recomputed by the worker whenever
-- classifier_version or input_hash (SHA-256 of the classifier's inputs)
-- differs from the current one. reviewed_* is a human override the worker
-- never writes; readers use COALESCE(reviewed_verdict, verdict).
--
-- Additive only. Rollback (do not run automatically): revert the API and
-- worker readers first, then DROP TABLE program_youth_fit_runs, program_youth_fit.
CREATE TABLE IF NOT EXISTS program_youth_fit (
    program_id         UUID        PRIMARY KEY REFERENCES programs(id) ON DELETE CASCADE,
    verdict            TEXT        NOT NULL
                                   CHECK (verdict IN ('fit', 'off_target', 'business_only', 'child_only', 'disability_only', 'veteran_only')),
    -- Who a demoted row is for instead; NULL exactly when verdict = 'fit'.
    target             TEXT        CHECK (target IS NULL OR target IN
                                   ('organisation', 'business', 'farm_fishery', 'elder', 'child', 'disability', 'veteran', 'other')),
    -- Rule code (K0-K7, none, S1-S5, D1, D2, V1) and the value it read.
    rule               TEXT        NOT NULL CHECK (rule ~ '^[A-Za-z0-9_]{1,20}$'),
    evidence           TEXT        CHECK (evidence IS NULL OR char_length(evidence) BETWEEN 1 AND 300),
    classifier_version TEXT        NOT NULL CHECK (classifier_version ~ '^[a-z0-9_]{1,40}$'),
    input_hash         TEXT        NOT NULL CHECK (input_hash ~ '^[0-9a-f]{64}$'),
    computed_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    reviewed_verdict   TEXT        CHECK (reviewed_verdict IS NULL OR reviewed_verdict IN
                                   ('fit', 'off_target', 'business_only', 'child_only', 'disability_only', 'veteran_only')),
    reviewed_target    TEXT        CHECK (reviewed_target IS NULL OR reviewed_target IN
                                   ('organisation', 'business', 'farm_fishery', 'elder', 'child', 'disability', 'veteran', 'other')),
    reviewed_by        TEXT        CHECK (reviewed_by IS NULL OR char_length(reviewed_by) BETWEEN 1 AND 200),
    reviewed_note      TEXT        CHECK (reviewed_note IS NULL OR char_length(reviewed_note) BETWEEN 1 AND 500),
    reviewed_at        TIMESTAMPTZ,
    CHECK ((verdict = 'fit') = (target IS NULL)),
    CHECK ((reviewed_verdict IS NULL) = (reviewed_by IS NULL)
           AND (reviewed_verdict IS NULL) = (reviewed_at IS NULL)),
    CHECK (reviewed_verdict IS NULL OR (reviewed_verdict = 'fit') = (reviewed_target IS NULL))
);
-- Readers look a row up by program_id (the primary key); no other index.

-- One row per worker refresh: verdict counts over the published catalogue,
-- so a jump in the demoted share between runs can raise an operational
-- event. Counts only, no programme text.
CREATE TABLE IF NOT EXISTS program_youth_fit_runs (
    id                 BIGSERIAL   PRIMARY KEY,
    ran_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    classifier_version TEXT        NOT NULL CHECK (classifier_version ~ '^[a-z0-9_]{1,40}$'),
    examined           INTEGER     NOT NULL CHECK (examined >= 0),
    written            INTEGER     NOT NULL CHECK (written >= 0),
    verdicts           JSONB       NOT NULL CHECK (jsonb_typeof(verdicts) = 'object'),
    demoted_share      NUMERIC(5,2) NOT NULL CHECK (demoted_share BETWEEN 0 AND 100)
);
CREATE INDEX IF NOT EXISTS idx_program_youth_fit_runs_time ON program_youth_fit_runs (ran_at DESC);

DO $rls$
DECLARE
  t text;
  client_role text;
BEGIN
  FOREACH t IN ARRAY ARRAY['program_youth_fit', 'program_youth_fit_runs'] LOOP
    EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', t);
    EXECUTE format('ALTER TABLE public.%I FORCE ROW LEVEL SECURITY', t);
    EXECUTE format('REVOKE ALL ON TABLE public.%I FROM PUBLIC', t);
    FOREACH client_role IN ARRAY ARRAY['anon', 'authenticated'] LOOP
      IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
        EXECUTE format('REVOKE ALL ON TABLE public.%I FROM %I', t, client_role);
      END IF;
    END LOOP;
    EXECUTE format('DROP POLICY IF EXISTS %I ON public.%I', t || '_all_service', t);
    EXECUTE format('CREATE POLICY %I ON public.%I FOR ALL USING (is_service_role()) WITH CHECK (is_service_role())',
                   t || '_all_service', t);
  END LOOP;
  FOREACH client_role IN ARRAY ARRAY['anon', 'authenticated'] LOOP
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
      EXECUTE format('REVOKE ALL ON SEQUENCE public.program_youth_fit_runs_id_seq FROM %I', client_role);
    END IF;
  END LOOP;
END
$rls$;

COMMENT ON TABLE program_youth_fit IS
    'youth_fit verdict per programme (mazimi_core::youth_fit): demoted rows leave the default list and recommendations but stay searchable. Derived; recomputed on classifier_version or input_hash change. reviewed_* is a human override.';
