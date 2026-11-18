-- The day a notice counts 만 나이 on, when the notice states it (review
-- R-M1): rule_engine::AgeBasis::for_program collapses the age basis to that
-- day. NULL (not stated) keeps the window basis.
--   'window'      the notice counts on the application day (stated)
--   'year_start'  1 January of the notice's year
--   'year_end'    31 December of the notice's year (연 나이)
--   'YYYY-MM-DD'  that day (e.g. 공고일 2026.3.2. 기준)
-- Written by the worker's normalizer from the source text; nothing else
-- writes it. Additive: one nullable column; current_programs gains it as its
-- last column (CREATE OR REPLACE keeps the grants); programs keeps its RLS.
-- Rollback (do not run automatically): recreate current_programs as in
-- 20261111000102_holder_condition.sql, then
--   ALTER TABLE programs DROP COLUMN age_basis;
ALTER TABLE programs
    ADD COLUMN IF NOT EXISTS age_basis text
        CHECK (age_basis IS NULL OR age_basis IN ('window', 'year_start', 'year_end')
               OR age_basis ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$');

COMMENT ON COLUMN programs.age_basis IS
    'Day the notice counts age on: window | year_start | year_end | YYYY-MM-DD; NULL = not stated.';

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
       p.region_districts, p.provider_category, p.provider_categories, p.field_category,
       p.browse_category, p.browse_category_by, p.eligibility_condition,
       -- The holder requirement (`field:value`) of the primary sponsor that
       -- carries a holder key (NULL for every first-party offer).
       (SELECT hb.holder_field || ':' || hb.holder_value
          FROM program_brands hpb JOIN brands hb ON hb.id = hpb.brand_id
         WHERE hpb.program_id = p.id AND hpb.relationship_type = 'sponsor'
           AND hb.holder_field IS NOT NULL
         ORDER BY hpb.is_primary DESC, hb.id LIMIT 1) AS holder_requirement,
       p.age_basis
FROM programs p
WHERE p.is_active AND p.program_status NOT IN ('closed','review_required')
  AND (LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at) IS NULL OR LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at)>now())
  AND (p.source_type<>'corporate_feed' OR EXISTS (
    SELECT 1 FROM benefit_feed_items i JOIN benefit_feeds f ON f.id=i.feed_id
    JOIN benefit_source_domains d ON d.id=f.domain_id JOIN brands b ON b.id=f.brand_id
    WHERE i.program_id=p.id AND i.publisher_enabled AND NOT i.publication_suspended AND i.valid_until>now()
      AND f.is_active AND f.authorized_until>now() AND d.is_active AND d.verification_status='verified' AND b.is_active));
