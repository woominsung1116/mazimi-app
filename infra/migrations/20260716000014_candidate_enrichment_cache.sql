-- Optional LLM-assisted extraction cache. Outputs are admin-only drafts and
-- can never publish a program without the existing human review endpoint.

CREATE TABLE benefit_candidate_enrichments (
    id                  UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    candidate_id        UUID        NOT NULL REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    content_hash        TEXT        NOT NULL CHECK (content_hash ~ '^[0-9A-Fa-f]{32}$'),
    provider            TEXT        NOT NULL CHECK (char_length(provider) BETWEEN 1 AND 50),
    model               TEXT        NOT NULL CHECK (char_length(model) BETWEEN 1 AND 100),
    prompt_version      TEXT        NOT NULL CHECK (char_length(prompt_version) BETWEEN 1 AND 100),
    schema_version      INTEGER     NOT NULL CHECK (schema_version > 0),
    status              TEXT        NOT NULL CHECK (
                            status IN ('processing', 'succeeded', 'failed', 'rejected')
                        ),
    response_payload    JSONB,
    validation_errors   JSONB       NOT NULL DEFAULT '[]'::jsonb CHECK (
                            jsonb_typeof(validation_errors) = 'array'
                        ),
    input_token_count   INTEGER     CHECK (input_token_count IS NULL OR input_token_count >= 0),
    output_token_count  INTEGER     CHECK (output_token_count IS NULL OR output_token_count >= 0),
    attempt_count       INTEGER     NOT NULL DEFAULT 1 CHECK (attempt_count BETWEEN 1 AND 20),
    last_error          TEXT CHECK (last_error IS NULL OR char_length(last_error) <= 1000),
    retry_after         TIMESTAMPTZ,
    completed_at        TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (
        candidate_id, content_hash, provider, model,
        prompt_version, schema_version
    ),
    CHECK (
        (status = 'succeeded' AND response_payload IS NOT NULL AND completed_at IS NOT NULL)
        OR status <> 'succeeded'
    )
);

CREATE INDEX idx_benefit_candidate_enrichments_retry
    ON benefit_candidate_enrichments (retry_after, updated_at)
    WHERE status = 'failed';

CREATE INDEX idx_benefit_candidate_enrichments_candidate
    ON benefit_candidate_enrichments (candidate_id, created_at DESC);

ALTER TABLE benefit_candidate_enrichments ENABLE ROW LEVEL SECURITY;
ALTER TABLE benefit_candidate_enrichments FORCE ROW LEVEL SECURITY;
CREATE POLICY "benefit_candidate_enrichments_all_service"
    ON benefit_candidate_enrichments
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

COMMENT ON TABLE benefit_candidate_enrichments IS
    'Versioned, service-only cache for optional structured extraction. Never a publication authority.';

