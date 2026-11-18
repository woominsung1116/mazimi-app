-- Program facts are consumed directly by recommendation filters, so malformed
-- values must not silently become eligibility decisions. This migration
-- canonicalizes values that are unambiguous, records legacy violations for
-- admin review, and enforces the contract for every new or changed row.

CREATE TABLE program_data_quality_issues (
    id              UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    program_id      UUID        NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    issue_code      TEXT        NOT NULL CHECK (
                        issue_code IN (
                            'invalid_title', 'invalid_age_range',
                            'negative_benefit_amount', 'invalid_date_range',
                            'unknown_region'
                        )
                    ),
    original_value  JSONB       NOT NULL,
    status          TEXT        NOT NULL DEFAULT 'open' CHECK (
                        status IN ('open', 'resolved', 'ignored')
                    ),
    resolution_note TEXT        CHECK (
                        resolution_note IS NULL OR char_length(resolution_note) <= 1000
                    ),
    first_detected_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_detected_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at       TIMESTAMPTZ,
    UNIQUE (program_id, issue_code),
    CHECK (
        (status = 'resolved' AND resolved_at IS NOT NULL)
        OR (status <> 'resolved' AND resolved_at IS NULL)
    )
);

CREATE INDEX idx_program_data_quality_issues_open
    ON program_data_quality_issues (last_detected_at DESC, program_id)
    WHERE status = 'open';

ALTER TABLE program_data_quality_issues ENABLE ROW LEVEL SECURITY;
ALTER TABLE program_data_quality_issues FORCE ROW LEVEL SECURITY;
CREATE POLICY "program_data_quality_issues_all_service"
    ON program_data_quality_issues
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

-- Only rewrite complete, unambiguous region arrays. Unknown labels remain in
-- place, are made visible through the issue table, and cannot be propagated by
-- a future write until reviewed.
WITH mapped AS (
    SELECT
        p.id,
        array_agg(DISTINCT mapping.canonical ORDER BY mapping.canonical)
            FILTER (WHERE mapping.canonical IS NOT NULL) AS canonical_regions,
        bool_and(mapping.canonical IS NOT NULL) AS all_known
    FROM programs p
    CROSS JOIN LATERAL unnest(p.regions) AS item(region)
    CROSS JOIN LATERAL (
        VALUES (
            CASE lower(btrim(item.region))
                WHEN '서울' THEN 'seoul' WHEN '서울시' THEN 'seoul'
                WHEN '서울특별시' THEN 'seoul' WHEN 'seoul' THEN 'seoul'
                WHEN '부산' THEN 'busan' WHEN '부산시' THEN 'busan'
                WHEN '부산광역시' THEN 'busan' WHEN 'busan' THEN 'busan'
                WHEN '대구' THEN 'daegu' WHEN '대구시' THEN 'daegu'
                WHEN '대구광역시' THEN 'daegu' WHEN 'daegu' THEN 'daegu'
                WHEN '인천' THEN 'incheon' WHEN '인천시' THEN 'incheon'
                WHEN '인천광역시' THEN 'incheon' WHEN 'incheon' THEN 'incheon'
                WHEN '광주' THEN 'gwangju' WHEN '광주시' THEN 'gwangju'
                WHEN '광주광역시' THEN 'gwangju' WHEN 'gwangju' THEN 'gwangju'
                WHEN '대전' THEN 'daejeon' WHEN '대전시' THEN 'daejeon'
                WHEN '대전광역시' THEN 'daejeon' WHEN 'daejeon' THEN 'daejeon'
                WHEN '울산' THEN 'ulsan' WHEN '울산시' THEN 'ulsan'
                WHEN '울산광역시' THEN 'ulsan' WHEN 'ulsan' THEN 'ulsan'
                WHEN '세종' THEN 'sejong' WHEN '세종시' THEN 'sejong'
                WHEN '세종특별자치시' THEN 'sejong' WHEN 'sejong' THEN 'sejong'
                WHEN '경기' THEN 'gyeonggi' WHEN '경기도' THEN 'gyeonggi'
                WHEN 'gyeonggi' THEN 'gyeonggi'
                WHEN '강원' THEN 'gangwon' WHEN '강원도' THEN 'gangwon'
                WHEN '강원특별자치도' THEN 'gangwon' WHEN 'gangwon' THEN 'gangwon'
                WHEN '충북' THEN 'chungbuk' WHEN '충청북도' THEN 'chungbuk'
                WHEN 'chungbuk' THEN 'chungbuk'
                WHEN '충남' THEN 'chungnam' WHEN '충청남도' THEN 'chungnam'
                WHEN 'chungnam' THEN 'chungnam'
                WHEN '전북' THEN 'jeonbuk' WHEN '전라북도' THEN 'jeonbuk'
                WHEN '전북특별자치도' THEN 'jeonbuk' WHEN 'jeonbuk' THEN 'jeonbuk'
                WHEN '전남' THEN 'jeonnam' WHEN '전라남도' THEN 'jeonnam'
                WHEN 'jeonnam' THEN 'jeonnam'
                WHEN '경북' THEN 'gyeongbuk' WHEN '경상북도' THEN 'gyeongbuk'
                WHEN 'gyeongbuk' THEN 'gyeongbuk'
                WHEN '경남' THEN 'gyeongnam' WHEN '경상남도' THEN 'gyeongnam'
                WHEN 'gyeongnam' THEN 'gyeongnam'
                WHEN '제주' THEN 'jeju' WHEN '제주도' THEN 'jeju'
                WHEN '제주특별자치도' THEN 'jeju' WHEN 'jeju' THEN 'jeju'
                ELSE NULL
            END
        )
    ) AS mapping(canonical)
    WHERE cardinality(p.regions) > 0
    GROUP BY p.id
)
UPDATE programs p
SET regions = mapped.canonical_regions,
    updated_at = now()
FROM mapped
WHERE p.id = mapped.id
  AND mapped.all_known
  AND p.regions IS DISTINCT FROM mapped.canonical_regions;

INSERT INTO program_data_quality_issues (program_id, issue_code, original_value)
SELECT id, 'invalid_title', jsonb_build_object('title', title)
FROM programs
WHERE char_length(btrim(title)) NOT BETWEEN 1 AND 300
ON CONFLICT (program_id, issue_code) DO UPDATE
SET original_value = EXCLUDED.original_value, last_detected_at = now();

INSERT INTO program_data_quality_issues (program_id, issue_code, original_value)
SELECT id, 'invalid_age_range', jsonb_build_object('min_age', min_age, 'max_age', max_age)
FROM programs
WHERE min_age NOT BETWEEN 0 AND 100
   OR max_age NOT BETWEEN 0 AND 100
   OR min_age > max_age
ON CONFLICT (program_id, issue_code) DO UPDATE
SET original_value = EXCLUDED.original_value, last_detected_at = now();

INSERT INTO program_data_quality_issues (program_id, issue_code, original_value)
SELECT id, 'negative_benefit_amount', jsonb_build_object(
    'monthly', benefit_amount_monthly,
    'semester', benefit_amount_semester,
    'once', benefit_amount_once
)
FROM programs
WHERE benefit_amount_monthly < 0
   OR benefit_amount_semester < 0
   OR benefit_amount_once < 0
ON CONFLICT (program_id, issue_code) DO UPDATE
SET original_value = EXCLUDED.original_value, last_detected_at = now();

INSERT INTO program_data_quality_issues (program_id, issue_code, original_value)
SELECT id, 'invalid_date_range', jsonb_build_object(
    'application_start_at', application_start_at,
    'application_end_at', application_end_at
)
FROM programs
WHERE application_start_at IS NOT NULL
  AND application_end_at IS NOT NULL
  AND application_end_at < application_start_at
ON CONFLICT (program_id, issue_code) DO UPDATE
SET original_value = EXCLUDED.original_value, last_detected_at = now();

INSERT INTO program_data_quality_issues (program_id, issue_code, original_value)
SELECT id, 'unknown_region', to_jsonb(regions)
FROM programs
WHERE regions IS NOT NULL
  AND NOT regions <@ ARRAY[
      'seoul', 'busan', 'daegu', 'incheon', 'gwangju', 'daejeon',
      'ulsan', 'sejong', 'gyeonggi', 'gangwon', 'chungbuk', 'chungnam',
      'jeonbuk', 'jeonnam', 'gyeongbuk', 'gyeongnam', 'jeju'
  ]::TEXT[]
ON CONFLICT (program_id, issue_code) DO UPDATE
SET original_value = EXCLUDED.original_value, last_detected_at = now();

ALTER TABLE programs
    ADD CONSTRAINT programs_title_quality_check
        CHECK (char_length(btrim(title)) BETWEEN 1 AND 300) NOT VALID,
    ADD CONSTRAINT programs_age_quality_check
        CHECK (
            (min_age IS NULL OR min_age BETWEEN 0 AND 100)
            AND (max_age IS NULL OR max_age BETWEEN 0 AND 100)
            AND (min_age IS NULL OR max_age IS NULL OR min_age <= max_age)
        ) NOT VALID,
    ADD CONSTRAINT programs_amount_quality_check
        CHECK (
            (benefit_amount_monthly IS NULL OR benefit_amount_monthly >= 0)
            AND (benefit_amount_semester IS NULL OR benefit_amount_semester >= 0)
            AND (benefit_amount_once IS NULL OR benefit_amount_once >= 0)
        ) NOT VALID,
    ADD CONSTRAINT programs_application_dates_quality_check
        CHECK (
            application_start_at IS NULL OR application_end_at IS NULL
            OR application_start_at <= application_end_at
        ) NOT VALID,
    ADD CONSTRAINT programs_regions_quality_check
        CHECK (
            regions IS NULL OR regions <@ ARRAY[
                'seoul', 'busan', 'daegu', 'incheon', 'gwangju', 'daejeon',
                'ulsan', 'sejong', 'gyeonggi', 'gangwon', 'chungbuk', 'chungnam',
                'jeonbuk', 'jeonnam', 'gyeongbuk', 'gyeongnam', 'jeju'
            ]::TEXT[]
        ) NOT VALID;

-- A clean installation gets fully validated constraints. A legacy installation
-- with recorded issues remains deployable, while PostgreSQL still rejects new
-- violations and the admin queue exposes the rows that need repair.
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM program_data_quality_issues WHERE status = 'open') THEN
        ALTER TABLE programs VALIDATE CONSTRAINT programs_title_quality_check;
        ALTER TABLE programs VALIDATE CONSTRAINT programs_age_quality_check;
        ALTER TABLE programs VALIDATE CONSTRAINT programs_amount_quality_check;
        ALTER TABLE programs VALIDATE CONSTRAINT programs_application_dates_quality_check;
        ALTER TABLE programs VALIDATE CONSTRAINT programs_regions_quality_check;
    END IF;
END
$$;

COMMENT ON TABLE program_data_quality_issues IS
    'Service-only audit queue for legacy program facts that violate recommendation data contracts.';
