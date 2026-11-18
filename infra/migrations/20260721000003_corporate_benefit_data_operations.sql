-- Corporate-benefit data operations: explicit validity, duplicate review and
-- fail-closed expiry. Duplicate matches are never merged automatically; they
-- are placed in a service-only review queue. Only benefits with an explicit
-- end timestamp are retired automatically.

ALTER TABLE programs
    ADD COLUMN IF NOT EXISTS benefit_valid_until TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS retired_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS retirement_reason TEXT;

ALTER TABLE programs
    ADD CONSTRAINT programs_retirement_reason_check CHECK (
        retirement_reason IS NULL
        OR retirement_reason IN (
            'expired', 'duplicate', 'provider_withdrawn',
            'source_invalid', 'manual'
        )
    );

CREATE INDEX idx_programs_corporate_validity
    ON programs (benefit_valid_until, application_end_at, deadline_at)
    WHERE program_type = 'corporate_benefit' AND is_active = true;

CREATE TABLE program_duplicate_candidates (
    id                    UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    program_id_low        UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    program_id_high       UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    canonical_fingerprint TEXT        NOT NULL CHECK (
                                      canonical_fingerprint ~ '^[0-9a-f]{32}$'
                                  ),
    match_basis           TEXT        NOT NULL CHECK (
                                      match_basis IN ('official_url_title', 'provider_title')
                                  ),
    confidence            NUMERIC(4,3) NOT NULL CHECK (
                                      confidence >= 0 AND confidence <= 1
                                  ),
    status                TEXT        NOT NULL DEFAULT 'open' CHECK (
                                      status IN ('open', 'confirmed', 'dismissed', 'obsolete')
                                  ),
    first_detected_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_detected_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    reviewed_at           TIMESTAMPTZ,
    reviewed_by           UUID        REFERENCES users(id) ON DELETE SET NULL,
    review_note           TEXT        CHECK (
                                      review_note IS NULL OR char_length(review_note) <= 1000
                                  ),
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (program_id_low, program_id_high),
    CHECK (program_id_low < program_id_high),
    CHECK (
        (status IN ('confirmed', 'dismissed') AND reviewed_at IS NOT NULL)
        OR (status IN ('open', 'obsolete') AND reviewed_at IS NULL)
    )
);

CREATE INDEX idx_program_duplicate_candidates_open
    ON program_duplicate_candidates (last_detected_at DESC, confidence DESC)
    WHERE status = 'open';

ALTER TABLE program_duplicate_candidates ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_duplicate_candidates FORCE ROW LEVEL SECURITY;
CREATE POLICY "program_duplicate_candidates_all_service"
    ON program_duplicate_candidates
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

CREATE TABLE program_expiration_events (
    id                      UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    program_id              UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    effective_end_at        TIMESTAMPTZ NOT NULL,
    previous_is_active      BOOLEAN     NOT NULL,
    previous_program_status TEXT        NOT NULL,
    action                  TEXT        NOT NULL DEFAULT 'auto_expired' CHECK (
                                        action IN ('auto_expired', 'restored')
                                    ),
    occurred_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    actor                   TEXT        NOT NULL DEFAULT 'maintenance_worker' CHECK (
                                        char_length(actor) BETWEEN 1 AND 100
                                    ),
    note                    TEXT        CHECK (note IS NULL OR char_length(note) <= 1000),
    UNIQUE (program_id, effective_end_at, action)
);

CREATE INDEX idx_program_expiration_events_recent
    ON program_expiration_events (occurred_at DESC, program_id);

ALTER TABLE program_expiration_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_expiration_events FORCE ROW LEVEL SECURITY;
CREATE POLICY "program_expiration_events_all_service"
    ON program_expiration_events
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

-- Extend the existing admin quality queue with corporate publication rules.
ALTER TABLE program_data_quality_issues
    DROP CONSTRAINT IF EXISTS program_data_quality_issues_issue_code_check;

ALTER TABLE program_data_quality_issues
    ADD CONSTRAINT program_data_quality_issues_issue_code_check CHECK (
        issue_code IN (
            'invalid_title', 'invalid_age_range', 'negative_benefit_amount',
            'invalid_date_range', 'unknown_region', 'missing_official_url',
            'missing_brand_link', 'missing_source_monitor'
        )
    );

CREATE OR REPLACE FUNCTION run_corporate_benefit_maintenance(
    p_as_of TIMESTAMPTZ DEFAULT now()
)
RETURNS TABLE (
    duplicate_candidates_upserted BIGINT,
    quality_issues_observed BIGINT,
    programs_expired BIGINT
)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    duplicate_count BIGINT := 0;
    quality_count BIGINT := 0;
    expired_count BIGINT := 0;
    affected BIGINT := 0;
BEGIN
    IF p_as_of IS NULL THEN
        RAISE EXCEPTION 'p_as_of must not be null';
    END IF;

    -- Serialize maintenance runs while allowing the ingestion and API paths
    -- to continue normally.
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'mazimi:corporate-benefit-maintenance', 0
    ));

    WITH normalized AS (
        SELECT
            program.id,
            regexp_replace(
                lower(btrim(COALESCE(program.company_name, program.provider_name, ''))),
                '[[:space:][:punct:]]+', '', 'g'
            ) AS provider_key,
            regexp_replace(
                lower(btrim(program.title)), '[[:space:][:punct:]]+', '', 'g'
            ) AS title_key,
            lower(regexp_replace(
                regexp_replace(btrim(COALESCE(program.official_url, '')), '#.*$', ''),
                '/+$', ''
            )) AS url_key
        FROM programs AS program
        WHERE program.program_type = 'corporate_benefit'
          AND program.provider_kind = 'corporate'
          AND program.is_active = true
    ), matches AS (
        SELECT
            LEAST(left_program.id, right_program.id) AS program_id_low,
            GREATEST(left_program.id, right_program.id) AS program_id_high,
            CASE
                WHEN left_program.url_key <> ''
                 AND left_program.url_key = right_program.url_key
                    THEN 'official_url_title'
                ELSE 'provider_title'
            END AS match_basis,
            CASE
                WHEN left_program.url_key <> ''
                 AND left_program.url_key = right_program.url_key
                    THEN 1.000::NUMERIC(4,3)
                ELSE 0.950::NUMERIC(4,3)
            END AS confidence
        FROM normalized AS left_program
        JOIN normalized AS right_program ON left_program.id < right_program.id
        WHERE left_program.title_key = right_program.title_key
          AND char_length(left_program.title_key) >= 4
          AND (
              (
                  left_program.provider_key <> ''
                  AND left_program.provider_key = right_program.provider_key
              )
              OR (
                  left_program.url_key <> ''
                  AND left_program.url_key = right_program.url_key
              )
          )
    )
    INSERT INTO program_duplicate_candidates (
        program_id_low, program_id_high, canonical_fingerprint,
        match_basis, confidence, first_detected_at, last_detected_at
    )
    SELECT
        match.program_id_low,
        match.program_id_high,
        md5(
            match.program_id_low::TEXT || ':' ||
            match.program_id_high::TEXT || ':' || match.match_basis
        ),
        match.match_basis,
        match.confidence,
        p_as_of,
        p_as_of
    FROM matches AS match
    ON CONFLICT (program_id_low, program_id_high) DO UPDATE SET
        canonical_fingerprint = EXCLUDED.canonical_fingerprint,
        match_basis = EXCLUDED.match_basis,
        confidence = EXCLUDED.confidence,
        status = CASE
            WHEN program_duplicate_candidates.status = 'obsolete' THEN 'open'
            ELSE program_duplicate_candidates.status
        END,
        last_detected_at = p_as_of,
        reviewed_at = CASE
            WHEN program_duplicate_candidates.status = 'obsolete' THEN NULL
            ELSE program_duplicate_candidates.reviewed_at
        END,
        updated_at = p_as_of;
    GET DIAGNOSTICS duplicate_count = ROW_COUNT;

    -- Open matches that disappeared are retained as audit history, but no
    -- longer remain in the operator's active review queue.
    WITH normalized AS (
        SELECT
            program.id,
            regexp_replace(
                lower(btrim(COALESCE(program.company_name, program.provider_name, ''))),
                '[[:space:][:punct:]]+', '', 'g'
            ) AS provider_key,
            regexp_replace(
                lower(btrim(program.title)), '[[:space:][:punct:]]+', '', 'g'
            ) AS title_key,
            lower(regexp_replace(
                regexp_replace(btrim(COALESCE(program.official_url, '')), '#.*$', ''),
                '/+$', ''
            )) AS url_key
        FROM programs AS program
        WHERE program.program_type = 'corporate_benefit'
          AND program.provider_kind = 'corporate'
          AND program.is_active = true
    )
    UPDATE program_duplicate_candidates AS candidate
    SET status = 'obsolete', last_detected_at = p_as_of, updated_at = p_as_of
    WHERE candidate.status = 'open'
      AND NOT EXISTS (
          SELECT 1
          FROM normalized AS left_program
          JOIN normalized AS right_program ON left_program.id < right_program.id
          WHERE left_program.id = candidate.program_id_low
            AND right_program.id = candidate.program_id_high
            AND left_program.title_key = right_program.title_key
            AND char_length(left_program.title_key) >= 4
            AND (
                (left_program.provider_key <> '' AND left_program.provider_key = right_program.provider_key)
                OR (left_program.url_key <> '' AND left_program.url_key = right_program.url_key)
            )
      );

    INSERT INTO program_data_quality_issues (
        program_id, issue_code, original_value,
        status, first_detected_at, last_detected_at
    )
    SELECT
        program.id,
        'missing_official_url',
        jsonb_build_object('official_url', program.official_url),
        'open', p_as_of, p_as_of
    FROM programs AS program
    WHERE program.program_type = 'corporate_benefit'
      AND program.provider_kind = 'corporate'
      AND program.is_active = true
      AND (
          program.official_url IS NULL
          OR btrim(program.official_url) !~ '^https://[^[:space:]]+$'
      )
    ON CONFLICT (program_id, issue_code) DO UPDATE SET
        original_value = EXCLUDED.original_value,
        status = CASE
            WHEN program_data_quality_issues.status = 'ignored' THEN 'ignored'
            ELSE 'open'
        END,
        last_detected_at = p_as_of,
        resolved_at = NULL,
        resolution_note = CASE
            WHEN program_data_quality_issues.status = 'ignored'
                THEN program_data_quality_issues.resolution_note
            ELSE NULL
        END;
    GET DIAGNOSTICS affected = ROW_COUNT;
    quality_count := quality_count + affected;

    INSERT INTO program_data_quality_issues (
        program_id, issue_code, original_value,
        status, first_detected_at, last_detected_at
    )
    SELECT
        program.id,
        'missing_brand_link',
        jsonb_build_object('source_id', program.source_id),
        'open', p_as_of, p_as_of
    FROM programs AS program
    WHERE program.source_type = 'corporate_official'
      AND program.is_active = true
      AND NOT EXISTS (
          SELECT 1 FROM program_brands AS link
          WHERE link.program_id = program.id
      )
    ON CONFLICT (program_id, issue_code) DO UPDATE SET
        original_value = EXCLUDED.original_value,
        status = CASE
            WHEN program_data_quality_issues.status = 'ignored' THEN 'ignored'
            ELSE 'open'
        END,
        last_detected_at = p_as_of,
        resolved_at = NULL,
        resolution_note = CASE
            WHEN program_data_quality_issues.status = 'ignored'
                THEN program_data_quality_issues.resolution_note
            ELSE NULL
        END;
    GET DIAGNOSTICS affected = ROW_COUNT;
    quality_count := quality_count + affected;

    INSERT INTO program_data_quality_issues (
        program_id, issue_code, original_value,
        status, first_detected_at, last_detected_at
    )
    SELECT
        program.id,
        'missing_source_monitor',
        jsonb_build_object('source_health', program.source_health),
        'open', p_as_of, p_as_of
    FROM programs AS program
    WHERE program.source_type = 'corporate_official'
      AND program.is_active = true
      AND NOT EXISTS (
          SELECT 1 FROM program_source_monitors AS monitor
          WHERE monitor.program_id = program.id
            AND monitor.verification_state = 'current'
      )
    ON CONFLICT (program_id, issue_code) DO UPDATE SET
        original_value = EXCLUDED.original_value,
        status = CASE
            WHEN program_data_quality_issues.status = 'ignored' THEN 'ignored'
            ELSE 'open'
        END,
        last_detected_at = p_as_of,
        resolved_at = NULL,
        resolution_note = CASE
            WHEN program_data_quality_issues.status = 'ignored'
                THEN program_data_quality_issues.resolution_note
            ELSE NULL
        END;
    GET DIAGNOSTICS affected = ROW_COUNT;
    quality_count := quality_count + affected;

    -- System-owned issues resolve automatically once the underlying program
    -- is corrected. Ignored findings remain an explicit operator decision.
    UPDATE program_data_quality_issues AS issue
    SET status = 'resolved', resolved_at = p_as_of,
        resolution_note = 'Automatically resolved by corporate benefit maintenance.'
    FROM programs AS program
    WHERE program.id = issue.program_id
      AND issue.status = 'open'
      AND (
          (
              issue.issue_code = 'missing_official_url'
              AND (
                  program.is_active = false
                  OR (
                      program.official_url IS NOT NULL
                      AND btrim(program.official_url) ~ '^https://[^[:space:]]+$'
                  )
              )
          )
          OR (
              issue.issue_code = 'missing_brand_link'
              AND (
                  program.is_active = false
                  OR EXISTS (
                      SELECT 1 FROM program_brands AS link
                      WHERE link.program_id = program.id
                  )
              )
          )
          OR (
              issue.issue_code = 'missing_source_monitor'
              AND (
                  program.is_active = false
                  OR EXISTS (
                      SELECT 1 FROM program_source_monitors AS monitor
                      WHERE monitor.program_id = program.id
                        AND monitor.verification_state = 'current'
                  )
              )
          )
      );

    INSERT INTO program_expiration_events (
        program_id, effective_end_at, previous_is_active,
        previous_program_status, action, occurred_at
    )
    SELECT
        program.id,
        COALESCE(
            program.benefit_valid_until,
            program.application_end_at,
            program.deadline_at
        ),
        program.is_active,
        program.program_status,
        'auto_expired',
        p_as_of
    FROM programs AS program
    WHERE program.program_type = 'corporate_benefit'
      AND program.provider_kind = 'corporate'
      AND program.is_active = true
      AND COALESCE(
          program.benefit_valid_until,
          program.application_end_at,
          program.deadline_at
      ) < p_as_of
    ON CONFLICT (program_id, effective_end_at, action) DO NOTHING;

    UPDATE programs AS program
    SET is_active = false,
        program_status = 'closed',
        retired_at = p_as_of,
        retirement_reason = 'expired',
        updated_at = p_as_of
    WHERE program.program_type = 'corporate_benefit'
      AND program.provider_kind = 'corporate'
      AND program.is_active = true
      AND COALESCE(
          program.benefit_valid_until,
          program.application_end_at,
          program.deadline_at
      ) < p_as_of;
    GET DIAGNOSTICS expired_count = ROW_COUNT;

    RETURN QUERY SELECT duplicate_count, quality_count, expired_count;
END;
$$;

REVOKE ALL ON FUNCTION run_corporate_benefit_maintenance(TIMESTAMPTZ) FROM PUBLIC;

COMMENT ON TABLE program_duplicate_candidates IS
    'Service-only, non-destructive review queue for high-confidence corporate benefit duplicates.';
COMMENT ON TABLE program_expiration_events IS
    'Immutable audit history for explicit corporate-benefit expiry and restoration actions.';
COMMENT ON FUNCTION run_corporate_benefit_maintenance(TIMESTAMPTZ) IS
    'Idempotent duplicate, publication-quality and explicit-expiry sweep; intended for the internal worker.';
