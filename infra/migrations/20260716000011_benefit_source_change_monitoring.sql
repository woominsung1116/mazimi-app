-- Auditable source-change monitoring for published corporate benefits.
--
-- A source page and a public benefit are deliberately modelled many-to-many:
-- one membership page may support several brand benefits, while one benefit
-- may eventually rely on more than one official source. A material change or
-- overdue verification fails closed by hiding only the affected programs.

ALTER TABLE programs
    ADD COLUMN source_health TEXT NOT NULL DEFAULT 'unmonitored',
    ADD COLUMN source_verified_at TIMESTAMPTZ,
    ADD COLUMN source_review_due_at TIMESTAMPTZ,
    ADD COLUMN source_change_detected_at TIMESTAMPTZ;

ALTER TABLE programs
    ADD CONSTRAINT programs_source_health_check CHECK (
        source_health IN (
            'unmonitored', 'current', 'change_detected',
            'unreachable', 'review_due', 'retired'
        )
    ),
    ADD CONSTRAINT programs_source_health_publish_check CHECK (
        is_active = false OR source_health IN ('unmonitored', 'current')
    );

CREATE INDEX idx_programs_source_review_due
    ON programs (source_review_due_at)
    WHERE provider_kind = 'corporate'
      AND source_health = 'current';

ALTER TABLE benefit_source_candidates
    DROP CONSTRAINT IF EXISTS benefit_source_candidates_status_check;

ALTER TABLE benefit_source_candidates
    ADD COLUMN previous_content_hash TEXT,
    ADD COLUMN last_successful_check_at TIMESTAMPTZ,
    ADD COLUMN consecutive_failures INT NOT NULL DEFAULT 0,
    ADD COLUMN recheck_interval_seconds INT NOT NULL DEFAULT 2592000,
    ADD CONSTRAINT benefit_source_candidates_status_check CHECK (
        status IN (
            'queued', 'fetched', 'extracted', 'review', 'approved',
            'change_review', 'unreachable_review', 'verification_due',
            'rejected', 'error'
        )
    ),
    ADD CONSTRAINT benefit_source_candidates_failure_count_check CHECK (
        consecutive_failures BETWEEN 0 AND 100
    ),
    ADD CONSTRAINT benefit_source_candidates_recheck_interval_check CHECK (
        recheck_interval_seconds BETWEEN 3600 AND 7776000
    ),
    ADD CONSTRAINT benefit_source_candidates_previous_hash_check CHECK (
        previous_content_hash IS NULL
        OR previous_content_hash ~ '^[0-9A-Fa-f]{32}$'
    );

CREATE INDEX idx_benefit_candidates_revalidation
    ON benefit_source_candidates (next_check_at, last_checked_at)
    WHERE status = 'approved' AND next_check_at IS NOT NULL;

CREATE TABLE program_source_monitors (
    program_id                UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    candidate_id              UUID        NOT NULL REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    is_primary                BOOLEAN     NOT NULL DEFAULT true,
    verification_state        TEXT        NOT NULL DEFAULT 'current' CHECK (
                                      verification_state IN (
                                          'current', 'change_detected', 'unreachable',
                                          'review_due', 'retired'
                                      )
                                  ),
    verified_content_hash     TEXT,
    verified_at               TIMESTAMPTZ NOT NULL,
    review_due_at             TIMESTAMPTZ NOT NULL,
    recheck_interval_seconds  INT         NOT NULL DEFAULT 2592000 CHECK (
                                      recheck_interval_seconds BETWEEN 3600 AND 7776000
                                  ),
    change_detected_at        TIMESTAMPTZ,
    last_error                TEXT,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (program_id, candidate_id),
    CHECK (
        verified_content_hash IS NULL
        OR verified_content_hash ~ '^[0-9A-Fa-f]{32}$'
    )
);

CREATE INDEX idx_program_source_monitors_candidate
    ON program_source_monitors (candidate_id, verification_state);
CREATE INDEX idx_program_source_monitors_due
    ON program_source_monitors (review_due_at, candidate_id)
    WHERE verification_state = 'current';

CREATE TABLE benefit_source_checks (
    id                      UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    candidate_id            UUID        NOT NULL REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    check_kind              TEXT        NOT NULL CHECK (
                                      check_kind IN (
                                          'initial', 'not_modified', 'unchanged', 'changed',
                                          'fetch_error', 'blocked', 'redirected', 'not_found',
                                          'review_overdue', 'manual_confirmation', 'retired'
                                      )
                                  ),
    previous_content_hash   TEXT,
    content_hash            TEXT,
    http_status             INT CHECK (http_status IS NULL OR http_status BETWEEN 100 AND 599),
    fetched_bytes           INT CHECK (fetched_bytes IS NULL OR fetched_bytes >= 0),
    error_summary           TEXT,
    review_status           TEXT        NOT NULL DEFAULT 'not_required' CHECK (
                                      review_status IN (
                                          'not_required', 'pending', 'confirmed', 'retired'
                                      )
                                  ),
    detected_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    reviewed_at             TIMESTAMPTZ,
    reviewed_by             UUID        REFERENCES users(id) ON DELETE SET NULL,
    review_note             TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        previous_content_hash IS NULL
        OR previous_content_hash ~ '^[0-9A-Fa-f]{32}$'
    ),
    CHECK (content_hash IS NULL OR content_hash ~ '^[0-9A-Fa-f]{32}$')
);

CREATE INDEX idx_benefit_source_checks_candidate
    ON benefit_source_checks (candidate_id, detected_at DESC);
CREATE INDEX idx_benefit_source_checks_pending
    ON benefit_source_checks (detected_at)
    WHERE review_status = 'pending';

CREATE TABLE program_source_holds (
    id                      UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    program_id              UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    candidate_id            UUID        NOT NULL REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    source_check_id         UUID        REFERENCES benefit_source_checks(id) ON DELETE SET NULL,
    hold_reason             TEXT        NOT NULL CHECK (
                                      hold_reason IN (
                                          'content_changed', 'source_unreachable',
                                          'verification_overdue', 'access_revoked',
                                          'redirected', 'not_found'
                                      )
                                  ),
    previous_is_active      BOOLEAN     NOT NULL,
    previous_program_status TEXT        NOT NULL,
    detected_content_hash   TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at             TIMESTAMPTZ,
    resolved_by             UUID        REFERENCES users(id) ON DELETE SET NULL,
    resolution              TEXT CHECK (
                                      resolution IS NULL OR resolution IN (
                                          'confirmed_unchanged', 'program_updated',
                                          'retired', 'dismissed'
                                      )
                                  ),
    resolution_note         TEXT,
    CHECK (
        detected_content_hash IS NULL
        OR detected_content_hash ~ '^[0-9A-Fa-f]{32}$'
    )
);

CREATE UNIQUE INDEX uq_program_source_holds_open
    ON program_source_holds (program_id, candidate_id)
    WHERE resolved_at IS NULL;
CREATE INDEX idx_program_source_holds_review
    ON program_source_holds (created_at, program_id)
    WHERE resolved_at IS NULL;

ALTER TABLE program_source_monitors ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_source_monitors FORCE ROW LEVEL SECURITY;
CREATE POLICY "program_source_monitors_all_service" ON program_source_monitors
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

ALTER TABLE benefit_source_checks ENABLE ROW LEVEL SECURITY;
ALTER TABLE benefit_source_checks FORCE ROW LEVEL SECURITY;
CREATE POLICY "benefit_source_checks_all_service" ON benefit_source_checks
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

ALTER TABLE program_source_holds ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_source_holds FORCE ROW LEVEL SECURITY;
CREATE POLICY "program_source_holds_all_service" ON program_source_holds
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

-- Turn each distinct verified official URL into a monitoring candidate. URL
-- fragments are browser-local identifiers and are removed so shared provider
-- pages correctly become one source with several linked programs.
WITH official_program AS (
    SELECT
        program.id AS program_id,
        regexp_replace(btrim(program.official_url), '#.*$', '') AS canonical_url,
        lower(substring(btrim(program.official_url) FROM '^https://([^/?#]+)')) AS source_domain,
        CASE
            WHEN program.benefit_category IN ('software', 'student_discount')
                THEN 7776000
            WHEN program.benefit_category IN (
                'membership', 'subscription', 'telecom', 'transport', 'discount'
            )
                THEN 2592000
            ELSE 5184000
        END AS interval_seconds,
        COALESCE(program.last_synced_at, program.updated_at, now()) AS verified_at
    FROM programs AS program
    WHERE program.source_type = 'corporate_official'
      AND program.is_active = true
      AND program.official_url ~ '^https://'
), matched AS (
    SELECT DISTINCT ON (official_program.canonical_url)
        official_program.canonical_url,
        official_program.source_domain,
        source.id AS domain_id,
        min(official_program.interval_seconds) OVER (
            PARTITION BY official_program.canonical_url
        ) AS interval_seconds,
        min(official_program.verified_at) OVER (
            PARTITION BY official_program.canonical_url
        ) AS verified_at
    FROM official_program
    JOIN benefit_source_domains AS source
      ON source.verification_status = 'verified'
     AND source.is_active = true
     AND (
         source.domain = official_program.source_domain
         OR (
             source.allow_subdomains = true
             AND official_program.source_domain LIKE '%.' || source.domain
         )
     )
    ORDER BY official_program.canonical_url, char_length(source.domain) DESC
)
INSERT INTO benefit_source_candidates (
    domain_id, url, canonical_url, discovered_via, status,
    candidate_kind, discovery_depth, next_check_at,
    recheck_interval_seconds, discovered_at, last_discovered_at,
    created_at, updated_at
)
SELECT
    matched.domain_id,
    matched.canonical_url,
    matched.canonical_url,
    'manual',
    'approved',
    'detail',
    0,
    matched.verified_at + make_interval(secs => matched.interval_seconds),
    matched.interval_seconds,
    now(),
    now(),
    now(),
    now()
FROM matched
ON CONFLICT (canonical_url) DO UPDATE SET
    status = CASE
        WHEN benefit_source_candidates.status = 'rejected'
        THEN benefit_source_candidates.status
        ELSE 'approved'
    END,
    recheck_interval_seconds = LEAST(
        benefit_source_candidates.recheck_interval_seconds,
        EXCLUDED.recheck_interval_seconds
    ),
    next_check_at = CASE
        WHEN benefit_source_candidates.next_check_at IS NULL
        THEN EXCLUDED.next_check_at
        ELSE LEAST(benefit_source_candidates.next_check_at, EXCLUDED.next_check_at)
    END,
    last_discovered_at = now(),
    raw_text = NULL,
    extracted_payload = NULL,
    updated_at = now();

WITH official_program AS (
    SELECT
        program.id AS program_id,
        regexp_replace(btrim(program.official_url), '#.*$', '') AS canonical_url,
        CASE
            WHEN program.benefit_category IN ('software', 'student_discount')
                THEN 7776000
            WHEN program.benefit_category IN (
                'membership', 'subscription', 'telecom', 'transport', 'discount'
            )
                THEN 2592000
            ELSE 5184000
        END AS interval_seconds,
        COALESCE(program.last_synced_at, program.updated_at, now()) AS verified_at
    FROM programs AS program
    WHERE program.source_type = 'corporate_official'
      AND program.is_active = true
      AND program.official_url ~ '^https://'
)
INSERT INTO program_source_monitors (
    program_id, candidate_id, is_primary, verification_state,
    verified_content_hash, verified_at, review_due_at,
    recheck_interval_seconds
)
SELECT
    official_program.program_id,
    candidate.id,
    true,
    'current',
    candidate.content_hash,
    official_program.verified_at,
    official_program.verified_at + make_interval(secs => official_program.interval_seconds),
    official_program.interval_seconds
FROM official_program
JOIN benefit_source_candidates AS candidate
  ON candidate.canonical_url = official_program.canonical_url
 AND candidate.status <> 'rejected'
ON CONFLICT (program_id, candidate_id) DO NOTHING;

-- Preserve monitoring for benefits approved through the admin review flow
-- before this migration existed.
INSERT INTO program_source_monitors (
    program_id, candidate_id, is_primary, verification_state,
    verified_content_hash, verified_at, review_due_at,
    recheck_interval_seconds
)
SELECT
    program.id,
    candidate.id,
    true,
    'current',
    candidate.content_hash,
    COALESCE(candidate.last_checked_at, program.last_synced_at, program.updated_at, now()),
    COALESCE(candidate.last_checked_at, program.last_synced_at, program.updated_at, now())
        + make_interval(secs => candidate.recheck_interval_seconds),
    candidate.recheck_interval_seconds
FROM benefit_source_candidates AS candidate
JOIN programs AS program ON program.id = candidate.program_id
WHERE candidate.status = 'approved'
  AND program.source_type = 'corporate_official'
ON CONFLICT (program_id, candidate_id) DO NOTHING;

UPDATE programs AS program
SET source_health = monitor.verification_state,
    source_verified_at = monitor.verified_at,
    source_review_due_at = monitor.review_due_at,
    updated_at = now()
FROM (
    SELECT
        program_id,
        CASE
            WHEN bool_and(verification_state = 'current') THEN 'current'
            ELSE 'review_due'
        END AS verification_state,
        max(verified_at) AS verified_at,
        min(review_due_at) AS review_due_at
    FROM program_source_monitors
    GROUP BY program_id
) AS monitor
WHERE program.id = monitor.program_id;

UPDATE benefit_source_candidates AS candidate
SET recheck_interval_seconds = monitor.interval_seconds,
    next_check_at = CASE
        WHEN candidate.next_check_at IS NULL THEN monitor.review_due_at
        ELSE LEAST(candidate.next_check_at, monitor.review_due_at)
    END,
    updated_at = now()
FROM (
    SELECT
        candidate_id,
        min(recheck_interval_seconds) AS interval_seconds,
        min(review_due_at) AS review_due_at
    FROM program_source_monitors
    WHERE verification_state = 'current'
    GROUP BY candidate_id
) AS monitor
WHERE candidate.id = monitor.candidate_id;

-- An official corporate benefit without a verified monitor must never remain
-- public. This also makes future direct/admin inserts use the reviewed source
-- workflow instead of bypassing it with source_health='unmonitored'.
UPDATE programs AS program
SET is_active = false,
    program_status = 'review_required',
    source_health = 'review_due',
    source_change_detected_at = now(),
    updated_at = now()
WHERE program.source_type = 'corporate_official'
  AND program.is_active = true
  AND NOT EXISTS (
      SELECT 1
      FROM program_source_monitors AS monitor
      WHERE monitor.program_id = program.id
        AND monitor.verification_state = 'current'
  );

ALTER TABLE programs
    ADD CONSTRAINT programs_official_source_current_publish_check CHECK (
        is_active = false
        OR source_type <> 'corporate_official'
        OR source_health = 'current'
    );

COMMENT ON TABLE program_source_monitors IS
    'Many-to-many verified-source state for public programs; drives revalidation and fail-closed publication holds.';
COMMENT ON TABLE benefit_source_checks IS
    'Hash/status-only source check audit. Source HTML is never stored here.';
COMMENT ON TABLE program_source_holds IS
    'Reversible publication state captured when source evidence changes, becomes unreachable, or expires.';
