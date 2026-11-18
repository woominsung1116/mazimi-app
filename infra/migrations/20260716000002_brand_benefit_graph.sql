-- Brand-benefit graph foundation.
--
-- A benefit's provider and the brands where it can be redeemed are different
-- concepts (for example, a card issuer can provide a benefit redeemable at a
-- cafe brand). Keep the existing programs table as the benefit entity and add
-- a many-to-many brand catalogue around it.
--
-- Rollback (manual, only after confirming no dependent data):
--   ALTER TABLE ingestion_items DROP COLUMN brand_id;
--   DROP TABLE brand_source_records, program_brands, brands;
--   ALTER TABLE programs DROP COLUMN provider_kind;

ALTER TABLE programs
    ADD COLUMN IF NOT EXISTS provider_kind TEXT;

COMMENT ON COLUMN programs.provider_kind IS
    'Origin of the benefit provider: government, municipality, university, corporate, private_foundation, nonprofit, or public_agency';

CREATE INDEX IF NOT EXISTS idx_programs_provider_kind
    ON programs (provider_kind)
    WHERE provider_kind IS NOT NULL;

-- Backfill only classifications that are explicit in the upstream source.
UPDATE programs
SET provider_kind = 'corporate',
    company_name = COALESCE(company_name, provider_name),
    benefit_category = COALESCE(benefit_category, 'finance')
WHERE source_type = 'fss_financial'
  AND provider_kind IS NULL;

UPDATE programs
SET provider_kind = 'government'
WHERE source_type IN ('gov_benefits', 'national_welfare')
  AND provider_kind IS NULL;

UPDATE programs
SET provider_kind = 'municipality'
WHERE source_type = 'local_welfare'
  AND provider_kind IS NULL;

UPDATE programs
SET provider_kind = CASE
        WHEN COALESCE(raw_payload ->> '운영기관구분', '') LIKE '%민간%' THEN 'private_foundation'
        WHEN COALESCE(raw_payload ->> '운영기관구분', '') LIKE '%대학%' THEN 'university'
        WHEN COALESCE(raw_payload ->> '운영기관구분', '') LIKE '%지자체%' THEN 'municipality'
        WHEN COALESCE(raw_payload ->> '운영기관구분', '') LIKE '%정부%' THEN 'government'
        WHEN COALESCE(raw_payload ->> '운영기관구분', '') LIKE '%공공%' THEN 'public_agency'
        ELSE provider_kind
    END,
    benefit_category = COALESCE(benefit_category, 'scholarship')
WHERE source_type = 'scholarship';

CREATE TABLE brands (
    id                  UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    name                TEXT        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    normalized_name     TEXT        NOT NULL CHECK (char_length(normalized_name) BETWEEN 1 AND 200),
    company_name        TEXT,
    industry_major      TEXT,
    industry_minor      TEXT,
    primary_product     TEXT,
    homepage_url        TEXT,
    logo_url            TEXT,
    source_type         TEXT        NOT NULL,
    source_id           TEXT        NOT NULL,
    discovery_priority  INT         NOT NULL DEFAULT 0 CHECK (discovery_priority BETWEEN 0 AND 100),
    is_active           BOOLEAN     NOT NULL DEFAULT true,
    last_synced_at      TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (source_type, source_id)
);

CREATE INDEX idx_brands_normalized_name
    ON brands (normalized_name text_pattern_ops);
CREATE INDEX idx_brands_industry
    ON brands (industry_major, industry_minor);
CREATE INDEX idx_brands_priority
    ON brands (discovery_priority DESC, name);

CREATE TABLE program_brands (
    program_id          UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    brand_id            UUID        NOT NULL REFERENCES brands(id) ON DELETE CASCADE,
    relationship_type   TEXT        NOT NULL DEFAULT 'redeemable_at'
                                    CHECK (relationship_type IN ('redeemable_at', 'sponsor', 'operator')),
    is_primary          BOOLEAN     NOT NULL DEFAULT false,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (program_id, brand_id, relationship_type)
);

CREATE INDEX idx_program_brands_brand
    ON program_brands (brand_id, program_id);

-- Raw upstream payloads are deliberately separated from the public brands
-- catalogue. This preserves provenance and unknown fields without exposing
-- business identifiers through the public brand API.
CREATE TABLE brand_source_records (
    id              UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    brand_id        UUID        REFERENCES brands(id) ON DELETE SET NULL,
    source_type     TEXT        NOT NULL,
    source_id       TEXT        NOT NULL,
    content_hash    TEXT        NOT NULL,
    raw_payload     JSONB       NOT NULL,
    fetched_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (source_type, source_id)
);

CREATE INDEX idx_brand_source_records_brand
    ON brand_source_records (brand_id);
CREATE INDEX idx_brand_source_records_fetched
    ON brand_source_records (fetched_at DESC);

ALTER TABLE ingestion_items
    ADD COLUMN IF NOT EXISTS brand_id UUID REFERENCES brands(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_ingestion_items_brand
    ON ingestion_items (brand_id)
    WHERE brand_id IS NOT NULL;

-- Public catalogue tables are readable, but only the service role may mutate
-- them. Raw source records remain service-only.
ALTER TABLE brands ENABLE ROW LEVEL SECURITY;
ALTER TABLE brands FORCE ROW LEVEL SECURITY;

CREATE POLICY "brands_all_service" ON brands
    FOR ALL
    USING (is_service_role())
    WITH CHECK (is_service_role());

CREATE POLICY "brands_select_public" ON brands
    FOR SELECT
    USING (is_active = true);

ALTER TABLE program_brands ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_brands FORCE ROW LEVEL SECURITY;

CREATE POLICY "program_brands_all_service" ON program_brands
    FOR ALL
    USING (is_service_role())
    WITH CHECK (is_service_role());

CREATE POLICY "program_brands_select_public" ON program_brands
    FOR SELECT
    USING (
        EXISTS (
            SELECT 1
            FROM programs
            WHERE programs.id = program_brands.program_id
              AND programs.is_active = true
        )
    );

ALTER TABLE brand_source_records ENABLE ROW LEVEL SECURITY;
ALTER TABLE brand_source_records FORCE ROW LEVEL SECURITY;

CREATE POLICY "brand_source_records_all_service" ON brand_source_records
    FOR ALL
    USING (is_service_role())
    WITH CHECK (is_service_role());

-- The requested anchor brand is a discovery target, not a fabricated benefit.
-- Corporate/online/direct-store brands that are absent from the franchise API
-- can be added through the admin brand endpoint using the same source_type.
INSERT INTO brands (
    name, normalized_name, source_type, source_id, discovery_priority
) VALUES (
    '스타벅스', '스타벅스', 'manual', '스타벅스', 100
)
ON CONFLICT (source_type, source_id) DO NOTHING;
