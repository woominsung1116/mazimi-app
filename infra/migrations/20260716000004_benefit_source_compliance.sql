-- Compliance controls for benefit-page discovery and fetching.
--
-- Registering an official domain is intentionally separate from authorising
-- automated fetching. A source may be discovered/verified while crawl_allowed
-- remains false. Automated fetching requires a time-bounded review record,
-- explicit approved paths, an allowed robots policy and a public bot contact.

ALTER TABLE benefit_source_domains
    ALTER COLUMN verification_status SET DEFAULT 'pending',
    ADD COLUMN verification_reference TEXT,
    ADD COLUMN allow_subdomains BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN compliance_status TEXT NOT NULL DEFAULT 'pending',
    ADD COLUMN approval_basis TEXT,
    ADD COLUMN approval_reference TEXT,
    ADD COLUMN terms_url TEXT,
    ADD COLUMN terms_checked_at TIMESTAMPTZ,
    ADD COLUMN robots_url TEXT,
    ADD COLUMN robots_status TEXT NOT NULL DEFAULT 'pending',
    ADD COLUMN robots_checked_at TIMESTAMPTZ,
    ADD COLUMN robots_content_hash TEXT,
    ADD COLUMN allowed_path_prefixes TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    ADD COLUMN min_request_interval_ms INT NOT NULL DEFAULT 10000,
    ADD COLUMN crawler_contact_url TEXT,
    ADD COLUMN compliance_reviewed_by TEXT,
    ADD COLUMN compliance_reviewed_at TIMESTAMPTZ,
    ADD COLUMN approval_expires_at TIMESTAMPTZ;

ALTER TABLE benefit_source_domains
    ADD CONSTRAINT benefit_source_compliance_status_check CHECK (
        compliance_status IN ('pending', 'approved', 'rejected', 'expired')
    ),
    ADD CONSTRAINT benefit_source_approval_basis_check CHECK (
        approval_basis IS NULL OR approval_basis IN (
            'written_permission', 'terms_explicit', 'partner_contract', 'open_license'
        )
    ),
    ADD CONSTRAINT benefit_source_robots_status_check CHECK (
        robots_status IN ('pending', 'allowed', 'blocked', 'unavailable')
    ),
    ADD CONSTRAINT benefit_source_interval_check CHECK (
        min_request_interval_ms BETWEEN 5000 AND 600000
    );

-- A previous application version could set crawl_allowed directly. Revoke any
-- such flag before installing the stronger invariant; an administrator must
-- re-approve the domain with evidence through the dedicated endpoint.
UPDATE benefit_source_domains
SET crawl_allowed = false,
    compliance_status = 'pending',
    updated_at = now()
WHERE crawl_allowed = true;

ALTER TABLE benefit_source_domains
    ADD CONSTRAINT benefit_source_crawl_requires_approval CHECK (
        NOT crawl_allowed OR (
            verification_status = 'verified'
            AND verification_reference IS NOT NULL
            AND char_length(btrim(verification_reference)) > 0
            AND compliance_status = 'approved'
            AND approval_basis IS NOT NULL
            AND approval_reference IS NOT NULL
            AND char_length(btrim(approval_reference)) > 0
            AND terms_checked_at IS NOT NULL
            AND robots_status = 'allowed'
            AND robots_checked_at IS NOT NULL
            AND cardinality(allowed_path_prefixes) > 0
            AND crawler_contact_url IS NOT NULL
            AND compliance_reviewed_by IS NOT NULL
            AND compliance_reviewed_at IS NOT NULL
            AND approval_expires_at IS NOT NULL
        )
    );

ALTER TABLE benefit_source_candidates
    ADD COLUMN etag TEXT,
    ADD COLUMN last_modified TEXT,
    ADD COLUMN http_status INT,
    ADD COLUMN fetched_bytes INT,
    ADD COLUMN next_check_at TIMESTAMPTZ;

CREATE INDEX idx_benefit_candidates_fetchable
    ON benefit_source_candidates (status, next_check_at, discovered_at)
    WHERE status IN ('queued', 'error');

COMMENT ON COLUMN benefit_source_candidates.raw_text IS
    'Service-only, size-limited plain-text review excerpt; never source HTML and never public.';

-- Remove legacy HTML fragments collected by the disabled regional scraper.
UPDATE programs
SET raw_payload = raw_payload - 'raw_html_snippet',
    updated_at = now()
WHERE source_type = 'local_scraper'
  AND raw_payload ? 'raw_html_snippet';

UPDATE source_snapshots
SET raw_payload = raw_payload - 'raw_html_snippet',
    fetched_at = now()
WHERE source_name = 'local_scraper'
  AND raw_payload ? 'raw_html_snippet';
