-- Holder condition (corporate-doors review C2). An offer a telco, card
-- issuer or pay app pays for is only for people who hold it: "T 멤버십 ·
-- CGV 4,000원 할인" needs an SK텔레콤 line. The requirement is brand data,
-- never page text:
--   1. brands.holder_field / holder_value: the one holder key a brand that
--      issues benefits carries (`telecom_carrier = skt`, `card_issuers ∋ kb`,
--      `pay_apps ∋ payco`). A brand with a key is an issuer.
--   2. current_programs.holder_requirement: the key of the offer's sponsor
--      brand (program_brands.relationship_type = 'sponsor'), the one place
--      the requirement is derived. Every first-party offer has no sponsor
--      with a key, so it is unchanged.
--   3. user_profiles.telecom_carrier / card_issuers / pay_apps: the user's
--      answers. NULL = not answered (the offer stays 확인 필요 with that field
--      as the missing check); an empty array = answered "none" (hidden).
--      user_profiles has FORCE ROW LEVEL SECURITY with own-row policies
--      (20260322000001) and no client-role grants (20260923000012); the
--      policies are table-wide, so they cover the new columns. No client
--      asks these yet.
-- The code lists are mazimi_core::holder's domains; a database test pins
-- these checks to them.
--
-- Rollback (do not run automatically): recreate current_programs as in
-- 20261107000001, then drop the five columns.

ALTER TABLE brands
    ADD COLUMN IF NOT EXISTS holder_field TEXT
        CHECK (holder_field IS NULL OR holder_field IN ('telecom_carrier', 'card_issuers', 'pay_apps')),
    ADD COLUMN IF NOT EXISTS holder_value TEXT
        CHECK (holder_value IS NULL OR holder_value ~ '^[a-z0-9]{2,20}$');

DO $holder_pair$
BEGIN
  ALTER TABLE brands ADD CONSTRAINT brands_holder_key_pair
    CHECK ((holder_field IS NULL) = (holder_value IS NULL));
EXCEPTION WHEN duplicate_object THEN NULL;
END
$holder_pair$;

COMMENT ON COLUMN brands.holder_field IS
    'Profile field a holder of this issuer answers (telecom_carrier | card_issuers | pay_apps); NULL for a brand that issues nothing.';

ALTER TABLE user_profiles
    ADD COLUMN IF NOT EXISTS telecom_carrier TEXT
        CHECK (telecom_carrier IS NULL OR telecom_carrier IN ('skt', 'kt', 'lgu', 'mvno')),
    ADD COLUMN IF NOT EXISTS card_issuers TEXT[]
        CHECK (card_issuers IS NULL OR (card_issuers <@ ARRAY['kb', 'shinhan', 'samsung', 'hyundai', 'lotte', 'woori', 'hana', 'bc', 'nh', 'ibk', 'citi', 'kakaobank', 'tossbank', 'kbank']::text[]
                                        AND cardinality(card_issuers) <= 14)),
    ADD COLUMN IF NOT EXISTS pay_apps TEXT[]
        CHECK (pay_apps IS NULL OR (pay_apps <@ ARRAY['naverpay', 'kakaopay', 'tosspay', 'payco', 'samsungpay', 'applepay', 'ssgpay', 'lpay']::text[]
                                    AND cardinality(pay_apps) <= 8));

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
         ORDER BY hpb.is_primary DESC, hb.id LIMIT 1) AS holder_requirement
FROM programs p
WHERE p.is_active AND p.program_status NOT IN ('closed','review_required')
  AND (LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at) IS NULL OR LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at)>now())
  AND (p.source_type<>'corporate_feed' OR EXISTS (
    SELECT 1 FROM benefit_feed_items i JOIN benefit_feeds f ON f.id=i.feed_id
    JOIN benefit_source_domains d ON d.id=f.domain_id JOIN brands b ON b.id=f.brand_id
    WHERE i.program_id=p.id AND i.publisher_enabled AND NOT i.publication_suspended AND i.valid_until>now()
      AND f.is_active AND f.authorized_until>now() AND d.is_active AND d.verification_status='verified' AND b.is_active));
