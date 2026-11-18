-- Additive, service-only evidence registry. No credentials, user answers or
-- downloaded files. Rollback: revert consumers first, then drop these two tables
-- and programs.application_resource_scan_at (do not run automatically).
ALTER TABLE programs ADD COLUMN application_resource_scan_at TIMESTAMPTZ;
CREATE TABLE program_application_resources (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    program_id UUID NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('notice','application','template')),
    url TEXT NOT NULL CHECK (length(url) BETWEEN 9 AND 2048),
    source_url TEXT NOT NULL CHECK (length(source_url) BETWEEN 9 AND 2048),
    label TEXT NOT NULL CHECK (length(label) BETWEEN 1 AND 200),
    status TEXT NOT NULL DEFAULT 'candidate' CHECK (status IN ('candidate','verified','rejected','unavailable')),
    access TEXT NOT NULL DEFAULT 'unknown' CHECK (access IN ('unknown','direct','login_required')),
    program_fingerprint TEXT NOT NULL,
    discovery_depth INTEGER NOT NULL DEFAULT 0 CHECK (discovery_depth BETWEEN 0 AND 2),
    content_hash TEXT,
    source_hash TEXT,
    file_format TEXT CHECK (file_format IN ('pdf','hwp','hwpx','docx')),
    byte_size BIGINT CHECK (byte_size BETWEEN 1 AND 8388608),
    checked_at TIMESTAMPTZ,
    verified_at TIMESTAMPTZ,
    valid_until TIMESTAMPTZ,
    reviewed_by UUID,
    review_note TEXT,
    check_error TEXT,
    next_check_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revision BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (program_id,kind,url),
    CHECK (status <> 'verified' OR (verified_at IS NOT NULL AND valid_until > verified_at
        AND valid_until <= verified_at + interval '7 days' AND reviewed_by IS NOT NULL
        AND length(review_note) BETWEEN 20 AND 2000 AND access <> 'unknown')),
    CHECK (status <> 'verified' OR kind <> 'template' OR
        (file_format IS NOT NULL AND content_hash IS NOT NULL AND byte_size IS NOT NULL AND checked_at IS NOT NULL))
);
CREATE INDEX application_resources_due ON program_application_resources(next_check_at) WHERE status <> 'rejected';
CREATE INDEX application_resources_program ON program_application_resources(program_id,status);
ALTER TABLE program_application_resources ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_application_resources FORCE ROW LEVEL SECURITY;
CREATE POLICY application_resources_service ON program_application_resources FOR ALL
    USING (is_service_role()) WITH CHECK (is_service_role());

CREATE TABLE application_resource_reviews (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    resource_id UUID NOT NULL REFERENCES program_application_resources(id) ON DELETE CASCADE,
    reviewer_id UUID NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('verified','rejected')),
    revision BIGINT NOT NULL,
    evidence JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER TABLE application_resource_reviews ENABLE ROW LEVEL SECURITY;
ALTER TABLE application_resource_reviews FORCE ROW LEVEL SECURITY;
CREATE POLICY application_resource_reviews_service ON application_resource_reviews FOR ALL
    USING (is_service_role()) WITH CHECK (is_service_role());
