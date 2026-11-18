-- Issuer + redeem model for corporate offers (.reports/corp-kr-routes.md
-- R1/R2, 1단계). A youth offer a telco or card company pays for and a
-- brand honours ("SKT 청년 혜택: CGV 영화 50% 할인") belongs to its issuer:
--   - the issuer is the programme's `sponsor` brand (program_brands,
--     is_primary), the brand where it is used is `redeemable_at`;
--   - `programs.eligibility_condition` states, word for word from the
--     issuer's page, what the user must hold ("만 34세 이하 베스트/ 라이트
--     요금제 가입 고객님께 제공됩니다."). It is required whenever the
--     redeem brand differs from the issuer, so "CGV 50%" is never shown
--     without the SKT plan it rests on;
--   - the title never starts with the redeem brand, so no "CGV 대학생 할인"
--     label implies CGV vouches for it.
-- The two rules are deferred constraint triggers: a migration or a publish
-- may write the programme and its brands in any order within one
-- transaction.
--
-- Rollback:
--   DROP TRIGGER IF EXISTS program_brands_issuer_redeem ON program_brands;
--   DROP TRIGGER IF EXISTS programs_issuer_redeem ON programs;
--   DROP FUNCTION IF EXISTS check_issuer_redeem();
--   recreate current_programs as in 20261026000301 (without the column),
--   then ALTER TABLE programs DROP COLUMN IF EXISTS eligibility_condition;

ALTER TABLE programs
    ADD COLUMN IF NOT EXISTS eligibility_condition TEXT
        CHECK (eligibility_condition IS NULL OR char_length(eligibility_condition) BETWEEN 4 AND 300);

COMMENT ON COLUMN programs.eligibility_condition IS
    'What the user must hold for an issuer-paid offer, verbatim from the issuer page (required when a redeemable_at brand differs from the sponsor).';

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
       p.browse_category, p.browse_category_by, p.eligibility_condition
FROM programs p
WHERE p.is_active AND p.program_status NOT IN ('closed','review_required')
  AND (LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at) IS NULL OR LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at)>now())
  AND (p.source_type<>'corporate_feed' OR EXISTS (
    SELECT 1 FROM benefit_feed_items i JOIN benefit_feeds f ON f.id=i.feed_id
    JOIN benefit_source_domains d ON d.id=f.domain_id JOIN brands b ON b.id=f.brand_id
    WHERE i.program_id=p.id AND i.publisher_enabled AND NOT i.publication_suspended AND i.valid_until>now()
      AND f.is_active AND f.authorized_until>now() AND d.is_active AND d.verification_status='verified' AND b.is_active));

CREATE OR REPLACE FUNCTION check_issuer_redeem() RETURNS trigger
LANGUAGE plpgsql SET search_path = public AS $fn$
DECLARE
    -- program_brands rows carry program_id; programs rows are the programme.
    pid UUID := COALESCE((to_jsonb(NEW)->>'program_id')::uuid, (to_jsonb(NEW)->>'id')::uuid);
    p RECORD;
BEGIN
    SELECT title, eligibility_condition INTO p FROM programs WHERE id = pid;
    IF NOT FOUND THEN
        RETURN NULL;
    END IF;
    IF EXISTS (
        SELECT 1 FROM program_brands s
        JOIN program_brands r ON r.program_id = s.program_id
         AND r.relationship_type = 'redeemable_at' AND r.brand_id <> s.brand_id
        WHERE s.program_id = pid AND s.relationship_type = 'sponsor'
    ) THEN
        IF p.eligibility_condition IS NULL OR btrim(p.eligibility_condition) = '' THEN
            RAISE EXCEPTION 'issuer offer % needs its eligibility_condition', pid
                USING ERRCODE = 'check_violation';
        END IF;
        IF EXISTS (
            SELECT 1 FROM program_brands r JOIN brands b ON b.id = r.brand_id
            WHERE r.program_id = pid AND r.relationship_type = 'redeemable_at'
              AND NOT EXISTS (SELECT 1 FROM program_brands s
                              WHERE s.program_id = pid AND s.relationship_type = 'sponsor'
                                AND s.brand_id = r.brand_id)
              AND left(lower(btrim(p.title)), length(lower(btrim(b.name)))) = lower(btrim(b.name))
        ) THEN
            RAISE EXCEPTION 'issuer offer % is titled by its redeem brand', pid
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NULL;
END
$fn$;

DROP TRIGGER IF EXISTS program_brands_issuer_redeem ON program_brands;
CREATE CONSTRAINT TRIGGER program_brands_issuer_redeem
    AFTER INSERT OR UPDATE ON program_brands
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION check_issuer_redeem();

DROP TRIGGER IF EXISTS programs_issuer_redeem ON programs;
CREATE CONSTRAINT TRIGGER programs_issuer_redeem
    AFTER UPDATE OF title, eligibility_condition ON programs
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION check_issuer_redeem();
