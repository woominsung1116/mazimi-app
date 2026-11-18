-- 기업혜택 browsing chip of each programme on the 기업혜택 tab, stored.
--
-- The rule is Rust only (mazimi_core::corporate_categories::decide): SQL
-- cannot run it, so this migration adds empty columns and the worker's
-- browse-category job (every five minutes, never on the startup path) fills
-- them. Until a row is filled its browse_category is NULL: the list shows it
-- under 전체 only and the chip counts leave it out. The admin
-- corporate-pipeline health reports such rows as `pending`.
--
--   browse_category      chip code (dev, design, ..., other); NULL off the tab
--   browse_category_by   keyword | fallback | none: what decided it
--   browse_category_key  rule fingerprint + md5 of the inputs it was computed
--                        from; a row whose inputs or rules changed no longer
--                        matches and is recomputed (programs has no
--                        updated_at trigger, so updated_at cannot say this)
--   browse_category_at   when it was computed
--
-- No table rewrite and no long lock: the new columns have no default. No
-- trigger reads or writes them, and no existing trigger fires on them (all
-- are UPDATE OF other columns). No new table, so no new RLS policy.
-- The index comes in 20261026000302 (CREATE INDEX CONCURRENTLY).
--
-- Reversible: recreate current_programs as in 20261023000002, then
--   ALTER TABLE programs DROP COLUMN browse_category, DROP COLUMN
--   browse_category_by, DROP COLUMN browse_category_key, DROP COLUMN
--   browse_category_at;
ALTER TABLE programs
    ADD COLUMN browse_category text,
    ADD COLUMN browse_category_by text,
    ADD COLUMN browse_category_key text,
    ADD COLUMN browse_category_at timestamptz;

-- Append the two read columns to the public view (CREATE OR REPLACE VIEW may
-- only add trailing columns; the list stays explicit, see 20261015000001).
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
       p.browse_category, p.browse_category_by
FROM programs p
WHERE p.is_active AND p.program_status NOT IN ('closed','review_required')
  AND (LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at) IS NULL OR LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at)>now())
  AND (p.source_type<>'corporate_feed' OR EXISTS (
    SELECT 1 FROM benefit_feed_items i JOIN benefit_feeds f ON f.id=i.feed_id
    JOIN benefit_source_domains d ON d.id=f.domain_id JOIN brands b ON b.id=f.brand_id
    WHERE i.program_id=p.id AND i.publisher_enabled AND NOT i.publication_suspended AND i.valid_until>now()
      AND f.is_active AND f.authorized_until>now() AND d.is_active AND d.verification_status='verified' AND b.is_active));
