-- A-09 운영자 AI 데이터 큐레이터: asynchronous extraction of eligibility
-- facts from public program text, with evidence spans and mandatory admin
-- approval. Model output never reaches public data without a reviewer.
--
-- Additive only. Rollback (do not run automatically): revert the API/worker
-- consumers and the pipeline upsert override clause first, then
--   ALTER TABLE program_documents DROP COLUMN ai_candidate_id,
--     DROP COLUMN evidence, DROP COLUMN source;
--   DROP TABLE program_ai_field_overrides, program_ai_extraction_candidates,
--     program_ai_extraction_cache, program_ai_extraction_runs;
-- Approved values already written to programs/eligibility_rules stay; they
-- are ordinary reviewed data with provenance in eligibility_rules.explain_json.

-- One row per worker run, with the telemetry counters.
CREATE TABLE program_ai_extraction_runs (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    mode                TEXT        NOT NULL CHECK (mode IN ('extract', 'normalize_only')),
    provider            TEXT        NOT NULL CHECK (char_length(provider) BETWEEN 1 AND 50),
    model               TEXT        NOT NULL CHECK (char_length(model) BETWEEN 1 AND 100),
    prompt_version      TEXT        NOT NULL CHECK (char_length(prompt_version) BETWEEN 1 AND 100),
    schema_version      INTEGER     NOT NULL CHECK (schema_version > 0),
    status              TEXT        NOT NULL DEFAULT 'running' CHECK (
                            status IN ('running', 'succeeded', 'partial', 'failed')
                        ),
    selected            INTEGER     NOT NULL DEFAULT 0 CHECK (selected >= 0),
    cache_hits          INTEGER     NOT NULL DEFAULT 0 CHECK (cache_hits >= 0),
    calls               INTEGER     NOT NULL DEFAULT 0 CHECK (calls >= 0),
    failures            INTEGER     NOT NULL DEFAULT 0 CHECK (failures >= 0),
    cooldown_skips      INTEGER     NOT NULL DEFAULT 0 CHECK (cooldown_skips >= 0),
    breaker_skips       INTEGER     NOT NULL DEFAULT 0 CHECK (breaker_skips >= 0),
    budget_skips        INTEGER     NOT NULL DEFAULT 0 CHECK (budget_skips >= 0),
    unchanged_skips     INTEGER     NOT NULL DEFAULT 0 CHECK (unchanged_skips >= 0),
    rejected_by_guard   INTEGER     NOT NULL DEFAULT 0 CHECK (rejected_by_guard >= 0),
    invalid_facts       INTEGER     NOT NULL DEFAULT 0 CHECK (invalid_facts >= 0),
    excluded_by_rules   INTEGER     NOT NULL DEFAULT 0 CHECK (excluded_by_rules >= 0),
    candidates_written  INTEGER     NOT NULL DEFAULT 0 CHECK (candidates_written >= 0),
    superseded          INTEGER     NOT NULL DEFAULT 0 CHECK (superseded >= 0),
    error               TEXT        CHECK (error IS NULL OR char_length(error) <= 1000),
    started_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at         TIMESTAMPTZ
);

CREATE INDEX idx_program_ai_extraction_runs_started
    ON program_ai_extraction_runs (started_at DESC);

-- Response cache keyed like the source: same program content, provider,
-- model, prompt and schema => no second model call. Failures cool down
-- (default 6h) before a retry. `source_text` is the masked text the model
-- saw and every evidence offset indexes into; it holds public program text
-- only, never user data.
CREATE TABLE program_ai_extraction_cache (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    program_id          UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    source_type         TEXT        NOT NULL,
    source_id           TEXT,
    source_hash         TEXT        NOT NULL CHECK (source_hash ~ '^[0-9a-f]{64}$'),
    provider            TEXT        NOT NULL CHECK (char_length(provider) BETWEEN 1 AND 50),
    model               TEXT        NOT NULL CHECK (char_length(model) BETWEEN 1 AND 100),
    prompt_version      TEXT        NOT NULL CHECK (char_length(prompt_version) BETWEEN 1 AND 100),
    schema_version      INTEGER     NOT NULL CHECK (schema_version > 0),
    request_hash        TEXT        NOT NULL CHECK (request_hash ~ '^[0-9a-f]{64}$'),
    source_text         TEXT        NOT NULL CHECK (char_length(source_text) BETWEEN 1 AND 6000),
    rule_coverage       JSONB       NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(rule_coverage) = 'object'),
    status              TEXT        NOT NULL CHECK (status IN ('succeeded', 'failed')),
    response_json       JSONB,
    error               TEXT        CHECK (error IS NULL OR char_length(error) <= 1000),
    failure_count       INTEGER     NOT NULL DEFAULT 0 CHECK (failure_count BETWEEN 0 AND 1000),
    last_failed_at      TIMESTAMPTZ,
    latency_ms          INTEGER     CHECK (latency_ms IS NULL OR latency_ms >= 0),
    checked_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (program_id, source_hash, provider, model, prompt_version, schema_version),
    CHECK (status <> 'succeeded' OR response_json IS NOT NULL),
    CHECK (status <> 'failed' OR (last_failed_at IS NOT NULL AND failure_count > 0))
);

CREATE INDEX idx_program_ai_extraction_cache_program
    ON program_ai_extraction_cache (program_id, checked_at DESC);

-- One row per extracted fact, so review is per fact (partial approval).
-- Idempotent per (program, source hash, prompt version, model, fact key).
CREATE TABLE program_ai_extraction_candidates (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id              UUID        REFERENCES program_ai_extraction_runs(id) ON DELETE SET NULL,
    cache_id            UUID        NOT NULL REFERENCES program_ai_extraction_cache(id) ON DELETE CASCADE,
    program_id          UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    source_hash         TEXT        NOT NULL CHECK (source_hash ~ '^[0-9a-f]{64}$'),
    provider            TEXT        NOT NULL,
    model               TEXT        NOT NULL,
    prompt_version      TEXT        NOT NULL,
    schema_version      INTEGER     NOT NULL CHECK (schema_version > 0),
    fact_key            TEXT        NOT NULL CHECK (char_length(fact_key) BETWEEN 1 AND 120),
    fact_type           TEXT        NOT NULL CHECK (fact_type IN (
                            'age', 'region', 'enrollment', 'employment', 'income',
                            'document', 'application_window'
                        )),
    value               JSONB       NOT NULL,
    evidence            JSONB,
    confidence          TEXT        CHECK (confidence IS NULL OR confidence IN ('HIGH', 'MEDIUM', 'LOW')),
    status              TEXT        NOT NULL CHECK (status IN (
                            'pending', 'flagged', 'invalid', 'approved', 'rejected', 'superseded'
                        )),
    validation          JSONB       NOT NULL DEFAULT '{"errors":[],"flags":[]}'::jsonb
                            CHECK (jsonb_typeof(validation) = 'object'),
    reviewer_id         UUID        REFERENCES users(id) ON DELETE SET NULL,
    reviewed_at         TIMESTAMPTZ,
    review_note         TEXT        CHECK (review_note IS NULL OR char_length(review_note) BETWEEN 1 AND 1000),
    applied_changes     JSONB,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (program_id, source_hash, prompt_version, model, fact_key),
    -- A reviewable fact always carries evidence; a decision always has a
    -- reviewer, a time and a note.
    CHECK (status NOT IN ('pending', 'flagged', 'approved') OR (evidence IS NOT NULL AND confidence IS NOT NULL)),
    CHECK (status NOT IN ('approved', 'rejected') OR (reviewed_at IS NOT NULL AND review_note IS NOT NULL))
);

CREATE INDEX idx_program_ai_extraction_candidates_queue
    ON program_ai_extraction_candidates (status, created_at)
    WHERE status IN ('pending', 'flagged');
CREATE INDEX idx_program_ai_extraction_candidates_program
    ON program_ai_extraction_candidates (program_id, created_at DESC);

-- Approved values for columns the ingestion upsert rewrites on every sync.
-- An override applies only while the program's raw payload is byte-identical
-- to the one the reviewer approved against (`raw_payload_md5`); a changed
-- source falls back to the normalizer's value until a new review.
CREATE TABLE program_ai_field_overrides (
    program_id          UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    field               TEXT        NOT NULL CHECK (field IN (
                            'min_age', 'max_age', 'application_start_at', 'application_end_at'
                        )),
    int_value           INTEGER,
    timestamp_value     TIMESTAMPTZ,
    raw_payload_md5     TEXT        CHECK (raw_payload_md5 IS NULL OR raw_payload_md5 ~ '^[0-9a-f]{32}$'),
    candidate_id        UUID        NOT NULL REFERENCES program_ai_extraction_candidates(id) ON DELETE CASCADE,
    approved_by         UUID        REFERENCES users(id) ON DELETE SET NULL,
    approved_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (program_id, field),
    CHECK (
        (field IN ('min_age', 'max_age') AND int_value BETWEEN 0 AND 100 AND timestamp_value IS NULL)
        OR (field IN ('application_start_at', 'application_end_at') AND timestamp_value IS NOT NULL AND int_value IS NULL)
    )
);

-- Provenance of approved document rows. Existing rows keep NULL source.
ALTER TABLE program_documents
    ADD COLUMN IF NOT EXISTS source TEXT CHECK (source IS NULL OR source IN ('ai_extraction_approved')),
    ADD COLUMN IF NOT EXISTS evidence JSONB CHECK (evidence IS NULL OR jsonb_typeof(evidence) = 'object'),
    ADD COLUMN IF NOT EXISTS ai_candidate_id UUID REFERENCES program_ai_extraction_candidates(id) ON DELETE SET NULL;

-- Service-only access, like every other worker/admin table.
ALTER TABLE program_ai_extraction_runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_ai_extraction_runs FORCE ROW LEVEL SECURITY;
CREATE POLICY program_ai_extraction_runs_service ON program_ai_extraction_runs
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

ALTER TABLE program_ai_extraction_cache ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_ai_extraction_cache FORCE ROW LEVEL SECURITY;
CREATE POLICY program_ai_extraction_cache_service ON program_ai_extraction_cache
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

ALTER TABLE program_ai_extraction_candidates ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_ai_extraction_candidates FORCE ROW LEVEL SECURITY;
CREATE POLICY program_ai_extraction_candidates_service ON program_ai_extraction_candidates
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

ALTER TABLE program_ai_field_overrides ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_ai_field_overrides FORCE ROW LEVEL SECURITY;
CREATE POLICY program_ai_field_overrides_service ON program_ai_field_overrides
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

-- Client roles never see these tables (see 20260923000012).
DO $access$
DECLARE
  table_name text;
  client_role text;
BEGIN
  FOREACH table_name IN ARRAY ARRAY[
    'program_ai_extraction_runs', 'program_ai_extraction_cache',
    'program_ai_extraction_candidates', 'program_ai_field_overrides'
  ] LOOP
    EXECUTE format('REVOKE ALL ON TABLE public.%I FROM PUBLIC', table_name);
    FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
      IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
        EXECUTE format('REVOKE ALL ON TABLE public.%I FROM %I', table_name, client_role);
      END IF;
    END LOOP;
  END LOOP;
END $access$;

COMMENT ON TABLE program_ai_extraction_candidates IS
    'Per-fact AI extraction candidates with exact evidence spans. Never a publication authority: only an admin approval applies a fact.';
COMMENT ON TABLE program_ai_field_overrides IS
    'Admin-approved AI values for ingestion-managed program columns; valid only while md5(raw_payload::text) matches.';
