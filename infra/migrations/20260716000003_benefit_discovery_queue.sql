-- Targeted benefit-page discovery queue.
--
-- Search APIs, sitemaps, partner feeds and admin/user reports only enqueue
-- candidate URLs. A separate worker may fetch a candidate after its domain is
-- explicitly trusted and crawl_allowed=true. Nothing in this queue is public
-- or promoted to programs without review.
--
-- Rollback (manual):
--   DROP TABLE benefit_candidate_brands, benefit_source_candidates,
--              benefit_source_domains;

CREATE TABLE benefit_source_domains (
    id                      UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    brand_id                UUID        REFERENCES brands(id) ON DELETE SET NULL,
    organization_name       TEXT        NOT NULL CHECK (char_length(organization_name) BETWEEN 1 AND 200),
    domain                  TEXT        NOT NULL UNIQUE CHECK (char_length(domain) BETWEEN 1 AND 253),
    source_role             TEXT        NOT NULL CHECK (
                                source_role IN ('brand_official', 'benefit_provider', 'partner')
                            ),
    crawl_allowed           BOOLEAN     NOT NULL DEFAULT false,
    verification_status     TEXT        NOT NULL DEFAULT 'verified' CHECK (
                                verification_status IN ('pending', 'verified', 'rejected')
                            ),
    verified_at             TIMESTAMPTZ,
    last_discovered_at      TIMESTAMPTZ,
    is_active               BOOLEAN     NOT NULL DEFAULT true,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_benefit_source_domains_brand
    ON benefit_source_domains (brand_id)
    WHERE brand_id IS NOT NULL;
CREATE INDEX idx_benefit_source_domains_crawl
    ON benefit_source_domains (crawl_allowed, is_active)
    WHERE verification_status = 'verified';

CREATE TABLE benefit_source_candidates (
    id                      UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    domain_id               UUID        NOT NULL REFERENCES benefit_source_domains(id) ON DELETE RESTRICT,
    url                     TEXT        NOT NULL,
    canonical_url           TEXT        NOT NULL UNIQUE,
    discovered_via          TEXT        NOT NULL CHECK (
                                discovered_via IN (
                                    'search_api', 'sitemap', 'official_feed',
                                    'manual', 'user_submission', 'partner_api'
                                )
                            ),
    discovery_query         TEXT,
    page_title              TEXT,
    raw_text                TEXT,
    content_hash            TEXT,
    extracted_payload       JSONB,
    extraction_confidence   REAL CHECK (
                                extraction_confidence IS NULL
                                OR extraction_confidence BETWEEN 0 AND 1
                            ),
    status                  TEXT        NOT NULL DEFAULT 'queued' CHECK (
                                status IN (
                                    'queued', 'fetched', 'extracted', 'review',
                                    'approved', 'rejected', 'error'
                                )
                            ),
    program_id              UUID        REFERENCES programs(id) ON DELETE SET NULL,
    attempt_count           INT         NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_error              TEXT,
    discovered_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_discovered_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_checked_at         TIMESTAMPTZ,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_benefit_candidates_queue
    ON benefit_source_candidates (status, discovered_at);
CREATE INDEX idx_benefit_candidates_domain
    ON benefit_source_candidates (domain_id, status);
CREATE INDEX idx_benefit_candidates_program
    ON benefit_source_candidates (program_id)
    WHERE program_id IS NOT NULL;

CREATE TABLE benefit_candidate_brands (
    candidate_id    UUID        NOT NULL REFERENCES benefit_source_candidates(id) ON DELETE CASCADE,
    brand_id        UUID        NOT NULL REFERENCES brands(id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (candidate_id, brand_id)
);

CREATE INDEX idx_benefit_candidate_brands_brand
    ON benefit_candidate_brands (brand_id, candidate_id);

-- Discovery configuration, fetched text and extraction output are operational
-- data. Only the API/worker service role may read or mutate them.
ALTER TABLE benefit_source_domains ENABLE ROW LEVEL SECURITY;
ALTER TABLE benefit_source_domains FORCE ROW LEVEL SECURITY;
CREATE POLICY "benefit_source_domains_all_service" ON benefit_source_domains
    FOR ALL
    USING (is_service_role())
    WITH CHECK (is_service_role());

ALTER TABLE benefit_source_candidates ENABLE ROW LEVEL SECURITY;
ALTER TABLE benefit_source_candidates FORCE ROW LEVEL SECURITY;
CREATE POLICY "benefit_source_candidates_all_service" ON benefit_source_candidates
    FOR ALL
    USING (is_service_role())
    WITH CHECK (is_service_role());

ALTER TABLE benefit_candidate_brands ENABLE ROW LEVEL SECURITY;
ALTER TABLE benefit_candidate_brands FORCE ROW LEVEL SECURITY;
CREATE POLICY "benefit_candidate_brands_all_service" ON benefit_candidate_brands
    FOR ALL
    USING (is_service_role())
    WITH CHECK (is_service_role());
