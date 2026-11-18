-- Two category axes for the catalogue tabs (v1 feedback F5.1, F5.2).
--
-- Provider: who offers it (정책 / 장학금 / 기업혜택). Field: what it is
-- about (주거 / 생활 / 교육 / 복지 / 금융 / 취업). Both are pure functions
-- of the row's own columns, stored in plain columns of programs (a trigger
-- for new and changed rows, a bounded worker refresh for the backlog and
-- for a rule version bump), so the list, its filters, the detail page and
-- the recommendation list read one answer without computing it per
-- request. Codes only; the Korean labels live in mazimi_core::categories.
-- The indexes come in 20261023000004 (CREATE INDEX CONCURRENTLY).
--
-- No table rewrite and no long lock: the new columns have no default.
--
-- The functions are IMMUTABLE plain SQL, reference only built-ins and each
-- other (schema-qualified), and are SECURITY INVOKER (default).
--
-- Reversible:
--   DROP TRIGGER programs_set_categories ON programs; DROP FUNCTION
--   programs_set_categories(), program_category_version();
--   DROP VIEW current_programs and recreate it without the three columns
--   (20261015000001); ALTER TABLE programs DROP COLUMN provider_category,
--   DROP COLUMN provider_categories, DROP COLUMN field_category,
--   DROP COLUMN category_version; then
--   DROP FUNCTION program_field_category(text, text, text, text, text, text, jsonb),
--   program_provider_categories(text, text, text, text),
--   program_provider_category(text, text, text, text),
--   program_is_scholarship(text, text, text), program_text_has(text, text, text, text).

-- Whether the title names a word of `p_title_pattern` or the summary one of
-- the (stricter) `p_summary_pattern`.
CREATE FUNCTION program_text_has(p_title text, p_summary text, p_title_pattern text, p_summary_pattern text)
RETURNS boolean LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT COALESCE(p_title ~ p_title_pattern, false) OR COALESCE(p_summary ~ p_summary_pattern, false)
$$;

-- A scholarship: the type, a corporate offer filed as one, or a title that
-- names one. "현장학습비", "장학숙" (a dormitory) and "장학사" do not match.
CREATE FUNCTION program_is_scholarship(p_program_type text, p_benefit_category text, p_title text)
RETURNS boolean LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT COALESCE(p_program_type = 'scholarship', false)
        OR COALESCE(p_benefit_category = 'scholarship', false)
        OR COALESCE(p_title ~ '장학(금|생|재단|회|사업|기금|지원|증서|서비스)', false)
        OR COALESCE(p_title ~* 'scholarship', false)
$$;

-- The primary provider tab: scholarship, else corporate (company and campus
-- offers, and company products such as bank deposits), else policy.
CREATE FUNCTION program_provider_category(
    p_program_type text, p_provider_kind text, p_benefit_category text, p_title text
) RETURNS text LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT CASE
        WHEN public.program_is_scholarship(p_program_type, p_benefit_category, p_title)
            THEN 'scholarship'
        WHEN p_program_type IN ('corporate_benefit', 'campus_benefit')
          OR COALESCE(p_provider_kind = 'corporate', false)
            THEN 'corporate'
        ELSE 'policy'
    END
$$;

-- Every provider tab the row is listed under: a corporate scholarship is
-- found under both 장학금 and 기업혜택.
CREATE FUNCTION program_provider_categories(
    p_program_type text, p_provider_kind text, p_benefit_category text, p_title text
) RETURNS text[] LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT CASE
        WHEN public.program_is_scholarship(p_program_type, p_benefit_category, p_title)
         AND (p_program_type IN ('corporate_benefit', 'campus_benefit')
              OR COALESCE(p_provider_kind = 'corporate', false))
            THEN ARRAY['scholarship', 'corporate']
        ELSE ARRAY[public.program_provider_category(
            p_program_type, p_provider_kind, p_benefit_category, p_title)]
    END
$$;

-- The field tab. A source's category is a hint, not an answer: 보조금24
-- files 결혼축하금 under 주거·자립 and 아이돌봄 under 보육·교육, so a
-- category maps to its field only when the title (or, for housing, the
-- summary) names it. Rows no rule claims stay in 복지 / 생활 rather than
-- being guessed. First match wins:
--   1. a financial product is finance; a scholarship is education;
--   2. housing / education / employment / finance words guard each source
--      category (보조금24 서비스분야, 온통청년 분류, 복지로 관심주제);
--   3. the ingestion's program_type housing / employment under the same
--      guards;
--   4. the rest by source category: 보건·의료, 임신·출산, 보호·돌봄,
--      생활안정, 보육, 복지로 rows → 복지; 문화·환경, 행정·안전, 참여·기반,
--      farm operations → 생활;
--   5. a company / campus offer's benefit_category; welfare stays welfare,
--      everything else is 생활.
CREATE FUNCTION program_field_category(
    p_program_type text, p_source_type text, p_provider_kind text,
    p_benefit_category text, p_title text, p_summary text, p_raw jsonb
) RETURNS text LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT CASE
        WHEN p_program_type = 'financial_product' THEN 'finance'
        -- A loan, interest subsidy or credit programme is finance wherever it
        -- is filed (학자금대출 이자 included); lending books is not.
        WHEN COALESCE(p_title ~ '(대출|대부|이자|융자|이차보전|신용회복|채무|상환|신용보증|회사채|담보대출|동산담보|청약통장|금융지원|보증연계)', false)
         AND NOT COALESCE(p_title ~ '(도서|서점|책|전집|도서대출|나눔|기부|후원|성금)', false)
            THEN 'finance'
        -- A dormitory or public rental run by anyone (a 장학회 included) is housing.
        WHEN COALESCE(p_title ~ '(기숙사|학생생활관|연합생활관|장학관|학사관|셰어하우스)', false) THEN 'housing'
        WHEN public.program_is_scholarship(p_program_type, p_benefit_category, p_title)
            THEN 'education'
        -- 보조금24
        WHEN p_source_type = 'gov_benefits' AND p_raw->>'서비스분야' = '주거·자립'
         AND NOT COALESCE(p_title ~ '(태양광|에너지|신재생|그린홈|도시가스|소방|화재|기반시설|급수|시스템|구축|가격|알리미|창업|사업자|점포|사업장|상가)', false)
         AND public.program_text_has(p_title, p_summary, '(주택|주거비|주거급여|주거지원|주거안정|주거지|주거공간|월세|전세|임대주택|임대료|전세임대|매입임대|임차|보증금|이사|집수리|리모델링|기숙사|셰어하우스|원룸)', '(월세|전세|임대주택|임차|보증금|주거비|집수리|기숙사)')
            THEN 'housing'
        -- 고용·창업 is employment unless the title is plainly not about work.
        WHEN p_source_type = 'gov_benefits' AND p_raw->>'서비스분야' = '고용·창업'
         AND (COALESCE(p_title ~ '(일자리|취업|구직|창업|고용|근로|직업|인턴|면접|채용|일경험|근속|재직|자격증|응시료|구인|일하는|노동)', false)
              OR (NOT COALESCE(p_summary ~ '(기업|사업주|소상공인|소공인|중소|법인|업체|사업자)', false)
                  AND NOT COALESCE(p_title ~ '(문화센터|건립|조성|축제|관광|박람회|전시회|해외진출|수출|인증|위로금|마을|기업|소상공인|중소|회사채|담보|보증|상용화|기술개발|제조|콘텐츠|산업|시장|광산|굴진|취재|제작)', false)))
            THEN 'employment'
        WHEN p_source_type = 'gov_benefits' AND p_raw->>'서비스분야' = '농림축산어업'
         AND COALESCE(p_title ~ '(창업|정착|청년농|일자리|취업|후계)', false)
            THEN 'employment'
        WHEN p_source_type = 'gov_benefits' AND p_raw->>'서비스분야' = '보육·교육'
         AND NOT COALESCE(p_title ~ '(생활안정|생계|격려금|위문|체육|수영|일자리|취업)', false)
         AND (COALESCE(p_title ~ '(교육비|수업료|학비|급식비)', false)
              OR (NOT COALESCE(p_title ~ '(보육|돌봄|육아|양육|어린이집|아이|영유아|유아|출산|임신|아동수당)', false)
                  AND COALESCE(p_title ~ '(교육|학습|훈련|학자금|강좌|강의|수강|연수|교재|학원|진학|진로|입학|학생|대학|교복|자격증|공부|영어|독서실|멘토링)', false)))
            THEN 'education'
        -- A loan, interest or savings programme filed elsewhere is finance.
        WHEN p_source_type IN ('gov_benefits', 'local_welfare', 'national_welfare')
         AND COALESCE(p_title ~ '(대출|이자|적금|저축|채무|신용회복|자산형성|금융상품|서민금융진흥원|융자|경영자금|육성자금|운전자금|정책자금|이차보전|특례보증|목돈|적립|채움공제|채움통장|재무|계좌|키움통장|디딤돌통장|디딤돌 통장|희망통장|청년통장|이룸통장|통장개설)', false)
         AND NOT COALESCE(p_title ~ '(도서|서점|책|전집|도서대출|나눔|기부|후원|성금)', false)
            THEN 'finance'
        WHEN p_source_type = 'gov_benefits' AND p_raw->>'서비스분야' IN ('문화·환경', '행정·안전')
         AND COALESCE(p_title ~ '(노인|어르신|장애|아동|수급|취약|저소득|한부모|보호|돌봄|실종|위기|예방|접종|건강|의료|검진|진료|긴급|폭력|피해자|기초|왕진|위로금|항쟁|희생자|환자|산후|산모|출산|임신|사회서비스|독거|경로당)', false)
            THEN 'welfare'
        -- A title that names housing or a job, filed under a general category.
        WHEN p_source_type IN ('gov_benefits', 'local_welfare', 'national_welfare')
         AND COALESCE(p_title ~ '(월세|전세|임대주택|임대료|주거비|임차|주택|주거지|주거공간|주거시설|주거환경|보금자리|숙소|임시거처|이사비|이사 비용|이사지원|중개보수|집수리)', false)
         AND NOT COALESCE(p_title ~ '(태양광|에너지|신재생|그린홈|도시가스|소방|화재|기반시설|급수|시스템|구축|가격|알리미|창업|사업자|점포|사업장|상가)', false)
            THEN 'housing'
        WHEN p_source_type IN ('gov_benefits', 'local_welfare', 'national_welfare')
         AND COALESCE(p_title ~ '(구직|취업|면접|일자리|채용|인턴|창업|직업훈련|훈련수당|일경험|자격증|응시료)', false)
            THEN 'employment'
        WHEN p_source_type IN ('gov_benefits', 'local_welfare', 'national_welfare')
         AND COALESCE(p_title ~ '(등록금|학비|수업료|교육비|학습|강좌|아카데미|영어|독서실|교복|입학준비|학생교류|직업교육|평생교육)', false)
            THEN 'education'
        WHEN p_source_type = 'gov_benefits'
         AND p_raw->>'서비스분야' IN ('문화·환경', '행정·안전', '농림축산어업', '고용·창업')
            THEN 'living'
        -- 생활안정 is welfare when it names a group it supports, else everyday life.
        WHEN p_source_type = 'gov_benefits' AND p_raw->>'서비스분야' IN ('생활안정', '주거·자립')
         AND NOT COALESCE(p_title ~ '(노인|어르신|장수|장애|아동|청소년|수급|기초|취약|저소득|차상위|한부모|보훈|유공자|참전|다문화|북한이탈|결혼이민|이주|임산부|출산|산모|난임|육아|양육|건강|의료|왕진|돌봄|위기|긴급|생계|자립|보호|피해자|행려|노숙|위로금|수당)', false)
            THEN 'living'
        WHEN p_source_type = 'gov_benefits' AND p_raw->>'서비스분야' IS NOT NULL THEN 'welfare'
        -- 온통청년: a job title is employment whatever its category.
        WHEN p_source_type = 'youth_center'
         AND COALESCE(p_title ~ '(면접|구직|취업|일자리|채용|인턴|창업)', false)
            THEN 'employment'
        WHEN p_source_type = 'youth_center' AND split_part(p_raw->>'lclsfNm', ',', 1) LIKE '일자리%'
         AND NOT (COALESCE(p_title ~ '(문화센터|건립|조성|축제|관광|박람회|전시회|해외진출|수출|인증|위로금|마을|기업|소상공인|중소|회사채|담보|보증|상용화|기술개발|제조|콘텐츠|산업|시장|광산|굴진|취재|제작)', false)
                  AND NOT COALESCE(p_title ~ '(일자리|취업|구직|창업|고용|근로|직업|인턴|채용|일경험)', false))
            THEN 'employment'
        WHEN p_source_type = 'youth_center' AND split_part(p_raw->>'lclsfNm', ',', 1) LIKE '주거%'
         AND public.program_text_has(p_title, p_summary, '(주택|주거비|주거급여|주거지원|주거안정|주거지|주거공간|월세|전세|임대주택|임대료|전세임대|매입임대|임차|보증금|이사|집수리|리모델링|기숙사|셰어하우스|원룸)', '(월세|전세|임대주택|임차|보증금|주거비|집수리|기숙사)')
            THEN 'housing'
        WHEN p_source_type = 'youth_center' AND split_part(p_raw->>'lclsfNm', ',', 1) LIKE '교육%'
         AND COALESCE(p_title ~ '(일자리|취업|창업|스타트업|구직|채용)', false)
            THEN 'employment'
        WHEN p_source_type = 'youth_center' AND split_part(p_raw->>'lclsfNm', ',', 1) LIKE '교육%'
         AND COALESCE(p_title ~ '(교육|학습|훈련|강좌|강의|수강|아카데미|양성|연수|멘토링|캠프|교실|학교|학생|대학|장학|학자금|진로|자격|인재)', false)
            THEN 'education'
        WHEN p_source_type = 'youth_center' AND split_part(p_raw->>'lclsfNm', ',', 1) LIKE '금융%'
         AND split_part(p_raw->>'mclsfNm', ',', 1) = '취약계층 및 금융지원'
         AND COALESCE(p_title ~ '(대출|적금|금융|이자|신용|저축|자산|통장|보증|부채|채무)', false)
            THEN 'finance'
        WHEN p_source_type = 'youth_center' AND split_part(p_raw->>'lclsfNm', ',', 1) LIKE '금융%'
         AND split_part(p_raw->>'mclsfNm', ',', 1) IN ('문화활동 및 생활지원', '예술인지원')
            THEN 'living'
        WHEN p_source_type = 'youth_center' AND split_part(p_raw->>'lclsfNm', ',', 1) LIKE '금융%'
            THEN 'welfare'
        WHEN p_source_type = 'youth_center' AND split_part(p_raw->>'lclsfNm', ',', 1) LIKE '참여%'
            THEN 'living'
        -- 복지로
        WHEN p_source_type IN ('local_welfare', 'national_welfare')
         AND COALESCE(p_raw->>'intrsThemaNmArray', p_raw->>'intrsThemaArray') LIKE '%주거%'
         AND public.program_text_has(p_title, p_summary, '(주택|주거비|주거급여|주거지원|주거안정|주거지|주거공간|월세|전세|임대주택|임대료|전세임대|매입임대|임차|보증금|이사|집수리|리모델링|기숙사|셰어하우스|원룸)', '(월세|전세|임대주택|임차|보증금|주거비|집수리|기숙사)')
            THEN 'housing'
        WHEN p_source_type IN ('local_welfare', 'national_welfare')
         AND COALESCE(p_raw->>'intrsThemaNmArray', p_raw->>'intrsThemaArray') LIKE '%일자리%'
         AND COALESCE(p_title ~ '(일자리|취업|구직|창업|고용|근로|직업|인턴|면접|자격증|응시료)', false)
            THEN 'employment'
        WHEN p_source_type IN ('local_welfare', 'national_welfare')
         AND COALESCE(p_raw->>'intrsThemaNmArray', p_raw->>'intrsThemaArray') ~ '교육'
         AND COALESCE(p_title ~ '(교육|학습|훈련|학자금|강좌|강의|수강|연수|교재|학원|진학|입학|학생|대학|교복|자격증|공부|영어|독서실|멘토링)', false)
            THEN 'education'
        -- The ingestion's keyword type, under the same guards.
        WHEN p_program_type = 'housing'
         AND public.program_text_has(p_title, p_summary, '(주택|주거비|주거급여|주거지원|주거안정|주거지|주거공간|월세|전세|임대주택|임대료|전세임대|매입임대|임차|보증금|이사|집수리|리모델링|기숙사|셰어하우스|원룸)', '(월세|전세|임대주택|임차|보증금|주거비|집수리|기숙사)')
            THEN 'housing'
        WHEN p_program_type = 'employment'
         AND COALESCE(p_title ~ '(일자리|취업|구직|창업|고용|근로|직업|인턴|면접|채용|일경험|근속|재직)', false)
            THEN 'employment'
        WHEN p_source_type IN ('local_welfare', 'national_welfare') THEN 'welfare'
        WHEN p_program_type IN ('corporate_benefit', 'campus_benefit')
          OR COALESCE(p_provider_kind = 'corporate', false) THEN CASE p_benefit_category
            WHEN 'education' THEN 'education'
            WHEN 'finance' THEN 'finance'
            ELSE 'living'
        END
        WHEN p_program_type IN ('welfare', 'housing') THEN 'welfare'
        ELSE 'living'
    END
$$;

-- The rule set's version. Bump it in the migration that changes a rule:
-- the worker's bounded refresh then recomputes every row whose stored
-- category_version differs.
CREATE FUNCTION program_category_version() RETURNS integer
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$ SELECT 1 $$;

-- Plain nullable columns: adding them is a catalog-only change (no table
-- rewrite). NULL = not computed yet; the API then lists the row only under
-- 전체. The worker job (mazimi_worker::categories) fills existing rows in
-- bounded batches after the heartbeat; the trigger below keeps every new or
-- changed row current.
ALTER TABLE programs
    ADD COLUMN IF NOT EXISTS provider_category text,
    ADD COLUMN IF NOT EXISTS provider_categories text[],
    ADD COLUMN IF NOT EXISTS field_category text,
    ADD COLUMN IF NOT EXISTS category_version integer;

CREATE FUNCTION programs_set_categories() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.provider_category := public.program_provider_category(
        NEW.program_type, NEW.provider_kind, NEW.benefit_category, NEW.title);
    NEW.provider_categories := public.program_provider_categories(
        NEW.program_type, NEW.provider_kind, NEW.benefit_category, NEW.title);
    NEW.field_category := public.program_field_category(
        NEW.program_type, NEW.source_type, NEW.provider_kind, NEW.benefit_category,
        NEW.title, NEW.summary, NEW.raw_payload);
    NEW.category_version := public.program_category_version();
    RETURN NEW;
END
$$;
ALTER FUNCTION programs_set_categories() SET search_path = pg_catalog, public;

CREATE TRIGGER programs_set_categories
    BEFORE INSERT OR UPDATE OF program_type, source_type, provider_kind,
        benefit_category, title, summary, raw_payload
    ON programs FOR EACH ROW EXECUTE FUNCTION programs_set_categories();

-- Append the three columns to the public view (CREATE OR REPLACE VIEW may
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
       p.region_districts, p.provider_category, p.provider_categories, p.field_category
FROM programs p
WHERE p.is_active AND p.program_status NOT IN ('closed','review_required')
  AND (LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at) IS NULL OR LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at)>now())
  AND (p.source_type<>'corporate_feed' OR EXISTS (
    SELECT 1 FROM benefit_feed_items i JOIN benefit_feeds f ON f.id=i.feed_id
    JOIN benefit_source_domains d ON d.id=f.domain_id JOIN brands b ON b.id=f.brand_id
    WHERE i.program_id=p.id AND i.publisher_enabled AND NOT i.publication_suspended AND i.valid_until>now()
      AND f.is_active AND f.authorized_until>now() AND d.is_active AND d.verification_status='verified' AND b.is_active));
