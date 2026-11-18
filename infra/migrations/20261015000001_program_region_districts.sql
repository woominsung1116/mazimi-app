-- 시·군·구 residence scope for programmes (add-column only, nullable).
--
-- Semantics read by the recommendation filter (crates/core/src/structural.rs):
--   NULL        district scope not collected yet: no district check, no
--               district ranking bonus.
--   '{}'        collected, and the programme covers the whole of every sido
--               in `regions` (sido-wide).
--   '{26410,…}' 5-digit 시군구 codes (행정표준코드 prefix). A user whose
--               city_code is in the same sido but not listed is excluded; a
--               user with no city_code is shown the row as "시·군·구 확인 필요".
--               A sido in `regions` with no listed district is sido-wide.
ALTER TABLE programs ADD COLUMN IF NOT EXISTS region_districts text[];

COMMENT ON COLUMN programs.region_districts IS
  '5-digit 시군구 codes the programme is limited to. NULL = not collected; empty = sido-wide.';

-- The view lists its columns explicitly so later programme columns are not
-- exposed by accident; append the new one at the end (CREATE OR REPLACE
-- VIEW may only add trailing columns).
CREATE OR REPLACE VIEW current_programs WITH (security_invoker=true) AS
SELECT p.id, p.program_type, p.source_type, p.source_id, p.title, p.summary,
       p.provider_name, p.official_url, p.program_status, p.application_start_at,
       p.application_end_at, p.benefit_amount_monthly, p.benefit_amount_semester,
       p.benefit_amount_once, p.region_scope, p.school_scope, p.tags, p.raw_payload,
       p.normalized_payload, p.search_tsv, p.min_age, p.max_age, p.regions,
       p.deadline_at, p.is_active, p.last_synced_at, p.created_at, p.updated_at,
       p.company_name, p.company_logo_url, p.benefit_category, p.application_steps,
       p.application_method, p.submission_documents, p.screening_method,
       p.provider_kind, p.source_health, p.source_verified_at, p.source_review_due_at,
       p.source_change_detected_at, p.normalizer_version, p.normalized_at,
       p.benefit_valid_until, p.retired_at, p.retirement_reason, p.university_id,
       p.region_districts
FROM programs p
WHERE p.is_active AND p.program_status NOT IN ('closed','review_required')
  AND (LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at) IS NULL OR LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at)>now())
  AND (p.source_type<>'corporate_feed' OR EXISTS (
    SELECT 1 FROM benefit_feed_items i JOIN benefit_feeds f ON f.id=i.feed_id
    JOIN benefit_source_domains d ON d.id=f.domain_id JOIN brands b ON b.id=f.brand_id
    WHERE i.program_id=p.id AND i.publisher_enabled AND NOT i.publication_suspended AND i.valid_until>now()
      AND f.is_active AND f.authorized_until>now() AND d.is_active AND d.verification_status='verified' AND b.is_active));
