-- Reviewed eligibility conditions per programme (plan "verified conditions"
-- §2.1). A row records what reviewers concluded from the notice text they
-- read: the rules it states (rule_json, rule_engine grammar), its application
-- window (window_json), whether every condition became a rule (status) and,
-- if not, which checks remain (remaining_checks). The serve-time predicate
-- (mazimi_core::condition_review::assess) marks a recommendation row
-- "조건 확인됨" only from a current `complete` review; any other state lists
-- it as "확인 필요". A complete review may also hide a row whose reviewed rule
-- the profile contradicts (hard_rule) or whose reviewed window has closed
-- (not_open). Missing or stale data never hides a row.
--
-- text_hash is mazimi_core::condition_review::text_hash over the source text
-- and codes the reviewers read (온통청년: the 9 applicant-review text fields
-- plus the age, region, job, school, special-field, major, marriage, income
-- and application-period codes). When the current raw_payload hashes
-- differently the review is stale and ignored until the new text is reviewed.
-- Every evidence quote is checked against that text on write.
--
-- window_confirmed_at: the last time the official 온통청년 detail record
-- still said 상시 (aplyPrdSeCd 0057002) for a rolling window. A rolling
-- window counts as open for 7 days after it.
--
-- Written only by the worker importer, the rolling-window confirmation job
-- and AdminUser routes, all as the service role. Keyed by the programme's
-- source identity, so a review survives re-ingestion of the same notice.
--
-- Additive only. Rollback (do not run automatically): revert the API/worker
-- consumers first, then DROP TABLE program_condition_review_log,
-- program_condition_reviews; DROP FUNCTION
-- program_condition_review_log_immutable().
CREATE TABLE program_condition_reviews (
    source_type      TEXT        NOT NULL CHECK (char_length(source_type) BETWEEN 1 AND 100),
    source_id        TEXT        NOT NULL CHECK (char_length(source_id) BETWEEN 1 AND 200),
    status           TEXT        NOT NULL CHECK (status IN ('complete','incomplete','conflict')),
    -- NULL = the review adds no rule (every condition is structural).
    rule_json        JSONB       CHECK (rule_json IS NULL OR jsonb_typeof(rule_json) = 'object'),
    window_json      JSONB       NOT NULL CHECK (
                         jsonb_typeof(window_json) = 'object'
                         AND window_json->>'kind' IN ('dated','rolling','recurring','event','closed','unknown')),
    remaining_checks TEXT[]      NOT NULL DEFAULT '{}' CHECK (
                         cardinality(remaining_checks) <= 32
                         AND remaining_checks <@ ARRAY['delegated_notice','gender','residence_duration',
                             'household_income_amount','assets','duplicate_benefit','prior_participation',
                             'employer_or_job_detail','business_detail','housing_detail','family_detail',
                             'school_detail','academic_merit','certification_or_training','military_service',
                             'debt_or_credit','isolation','target_group_other','other']::text[]),
    -- The curation coverage map (segment labels, reasons, code rules the
    -- curators saw, agreement), stored as-is for audit; never read at serve.
    coverage_json    JSONB       NOT NULL DEFAULT '{}'::jsonb CHECK (
                         jsonb_typeof(coverage_json) = 'object' AND pg_column_size(coverage_json) <= 1048576),
    -- Verbatim quotes the review rests on: [{"for": "rule:R1"|"window", "quote": "..."}].
    evidence         JSONB       NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(evidence) = 'array'),
    text_hash        TEXT        NOT NULL CHECK (text_hash ~ '^[0-9a-f]{64}$'),
    reviewer         TEXT        NOT NULL CHECK (char_length(reviewer) BETWEEN 1 AND 200),
    window_confirmed_at TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A complete rule set leaves no check open.
    CHECK (status <> 'complete' OR cardinality(remaining_checks) = 0),
    PRIMARY KEY (source_type, source_id)
);

-- Rolling-window confirmation picks complete reviews by window kind.
CREATE INDEX program_condition_reviews_window_kind
    ON program_condition_reviews ((window_json->>'kind'))
    WHERE status = 'complete';

-- Append-only change log: every write, window confirmation and delete, with
-- who made it ('admin:<user id>' from the verified JWT,
-- 'import:<operator>:<file>', or 'job:rolling_window_confirmation').
-- `snapshot` is the row as written (NULL for a delete).
CREATE TABLE program_condition_review_log (
    id              BIGSERIAL   PRIMARY KEY,
    source_type     TEXT        NOT NULL,
    source_id       TEXT        NOT NULL,
    action          TEXT        NOT NULL CHECK (action IN ('upsert','delete','confirm_window')),
    status          TEXT        CHECK (status IS NULL OR status IN ('complete','incomplete','conflict')),
    previous_status TEXT        CHECK (previous_status IS NULL OR previous_status IN ('complete','incomplete','conflict')),
    text_hash       TEXT        CHECK (text_hash IS NULL OR text_hash ~ '^[0-9a-f]{64}$'),
    snapshot        JSONB,
    actor           TEXT        NOT NULL CHECK (char_length(actor) BETWEEN 1 AND 300),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX program_condition_review_log_subject
    ON program_condition_review_log (source_type, source_id, created_at DESC);

-- Append-only in the database, not only by convention: no role may change
-- or remove a log row (TRUNCATE stays an owner-only maintenance action).
CREATE FUNCTION program_condition_review_log_immutable() RETURNS trigger
    LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
BEGIN
    RAISE EXCEPTION 'program_condition_review_log is append-only';
END $$;
CREATE TRIGGER program_condition_review_log_no_mutation
    BEFORE UPDATE OR DELETE ON program_condition_review_log
    FOR EACH ROW EXECUTE FUNCTION program_condition_review_log_immutable();
REVOKE ALL ON FUNCTION program_condition_review_log_immutable() FROM PUBLIC;

DO $access$
DECLARE
  t text;
  client_role text;
BEGIN
  FOREACH t IN ARRAY ARRAY['program_condition_reviews','program_condition_review_log'] LOOP
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
    REVOKE ALL ON SEQUENCE public.program_condition_review_log_id_seq FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON SEQUENCE public.program_condition_review_log_id_seq FROM authenticated;
  END IF;
END $access$;

COMMENT ON TABLE program_condition_reviews IS
    'Reviewed condition rules, window and coverage status per programme source row; applied at serve time only while text_hash matches the current raw_payload.';
COMMENT ON TABLE program_condition_review_log IS
    'Append-only history of program_condition_reviews writes, window confirmations and deletes.';
