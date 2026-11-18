-- Second manually reviewed corporate-benefit expansion.
--
-- Review date: 2026-07-21
-- Evidence: current first-party pages only. This migration publishes no
-- search result, inferred promotion, or crawler output. Registering a source
-- still does not grant automated access: every domain remains crawl-disabled
-- unless a separate, evidence-backed compliance approval already exists.

-- ---------------------------------------------------------------------------
-- 1. Extend the discovery graph for newly verified providers.
-- ---------------------------------------------------------------------------

WITH seed(target_key, display_name, category, priority) AS (
    VALUES
      ('shinsegae_point', '신세계포인트', '멤버십·포인트', 86),
      ('tableau', 'Tableau', '전자·소프트웨어', 86)
)
INSERT INTO brand_discovery_targets (
    target_key, display_name, coverage_category, discovery_priority
)
SELECT target_key, display_name, category, priority
FROM seed
ON CONFLICT (target_key) DO UPDATE SET
    display_name = EXCLUDED.display_name,
    coverage_category = EXCLUDED.coverage_category,
    discovery_priority = GREATEST(
        brand_discovery_targets.discovery_priority,
        EXCLUDED.discovery_priority
    ),
    updated_at = now();

INSERT INTO brands (
    name, normalized_name, source_type, source_id, discovery_priority
)
SELECT
    target.display_name,
    lower(regexp_replace(target.display_name, '[^0-9A-Za-z가-힣]', '', 'g')),
    'coverage_seed',
    target.target_key,
    target.discovery_priority
FROM brand_discovery_targets AS target
WHERE target.target_key IN ('shinsegae_point', 'tableau')
  AND NOT EXISTS (
      SELECT 1
      FROM brands AS brand
      WHERE brand.normalized_name = lower(
                regexp_replace(target.display_name, '[^0-9A-Za-z가-힣]', '', 'g')
            )
        AND brand.is_active = true
  )
ON CONFLICT (source_type, source_id) DO UPDATE SET
    discovery_priority = GREATEST(
        brands.discovery_priority,
        EXCLUDED.discovery_priority
    ),
    updated_at = now();

WITH ranked_matches AS (
    SELECT
        target.id AS target_id,
        brand.id AS brand_id,
        row_number() OVER (
            PARTITION BY target.id
            ORDER BY brand.discovery_priority DESC, brand.created_at ASC
        ) AS match_rank
    FROM brand_discovery_targets AS target
    JOIN brands AS brand
      ON brand.normalized_name = lower(
             regexp_replace(target.display_name, '[^0-9A-Za-z가-힣]', '', 'g')
         )
     AND brand.is_active = true
    WHERE target.target_key IN ('shinsegae_point', 'tableau')
      AND target.brand_id IS NULL
)
UPDATE brand_discovery_targets AS target
SET brand_id = matched.brand_id,
    updated_at = now()
FROM ranked_matches AS matched
WHERE target.id = matched.target_id
  AND matched.match_rank = 1;

-- Official-root attribution is search metadata only. Direct collection stays
-- outside this migration and requires the compliance workflow.
WITH official(target_key, official_domain, homepage_url) AS (
    VALUES
      ('lottecinema', 'www.lottecinema.co.kr', 'https://www.lottecinema.co.kr/'),
      ('gs25', 'gs25.gsretail.com', 'https://gs25.gsretail.com/'),
      ('daiso', 'www.daiso.co.kr', 'https://www.daiso.co.kr/'),
      ('coffee_bean', 'www.coffeebeankorea.com', 'https://www.coffeebeankorea.com/'),
      ('paul_bassett', 'www.baristapaulbassett.co.kr', 'https://www.baristapaulbassett.co.kr/'),
      ('hpoint', 'www.h-point.co.kr', 'https://www.h-point.co.kr/'),
      ('shinsegae_point', 'bluefit.shinsegae.com', 'https://bluefit.shinsegae.com/service/membership/shinsegae-point.do'),
      ('yes24', 'www.yes24.com', 'https://www.yes24.com/'),
      ('aladin', 'www.aladin.co.kr', 'https://www.aladin.co.kr/'),
      ('okcashbag', 'www.okcashbag.com', 'https://www.okcashbag.com/'),
      ('musinsa', 'www.musinsa.com', 'https://www.musinsa.com/'),
      ('aws', 'aws.amazon.com', 'https://aws.amazon.com/education/awseducate/'),
      ('tableau', 'www.tableau.com', 'https://www.tableau.com/academic/students'),
      ('lpoint', 'm.lpoint.com', 'https://m.lpoint.com/'),
      ('lotteria', 'www.lotteeatz.com', 'https://www.lotteeatz.com/')
)
UPDATE brand_discovery_targets AS target
SET official_domain = official.official_domain,
    official_homepage_url = official.homepage_url,
    domain_status = 'verified',
    policy_status = 'search_only',
    preferred_collection_modes = ARRAY['licensed_search', 'manual_review']::TEXT[],
    policy_notes = COALESCE(
        target.policy_notes,
        'Official domain verified; direct collection remains disabled pending separate compliance approval.'
    ),
    next_search_at = COALESCE(target.next_search_at, now()),
    last_error = NULL,
    updated_at = now()
FROM official
WHERE target.target_key = official.target_key
  AND target.policy_status <> 'blocked'
  AND target.domain_status IN ('unresolved', 'candidate', 'verified')
  AND (
      target.official_domain IS NULL
      OR target.official_domain = official.official_domain
  );

UPDATE brands AS brand
SET homepage_url = COALESCE(brand.homepage_url, target.official_homepage_url),
    discovery_priority = GREATEST(brand.discovery_priority, target.discovery_priority),
    updated_at = now()
FROM brand_discovery_targets AS target
WHERE target.brand_id = brand.id
  AND target.domain_status = 'verified'
  AND target.official_homepage_url IS NOT NULL;

-- ---------------------------------------------------------------------------
-- 2. Register exact first-party evidence domains. Verification is provenance,
--    never crawl permission.
-- ---------------------------------------------------------------------------

WITH source(
    domain, organization_name, verification_reference, source_role,
    primary_target_key
) AS (
    VALUES
      ('www.lottecinema.co.kr', '롯데시네마', 'https://www.lottecinema.co.kr/NLCHS/Membership/l_point', 'brand_official', 'lottecinema'),
      ('gs25.gsretail.com', 'GS25', 'https://gs25.gsretail.com/gscvs/ko/membership-services/gs-retail', 'brand_official', 'gs25'),
      ('www.daiso.co.kr', '다이소', 'https://www.daiso.co.kr/cs/faq?cid=21', 'brand_official', 'daiso'),
      ('www.coffeebeankorea.com', '커피빈코리아', 'https://www.coffeebeankorea.com/card/about.asp', 'brand_official', 'coffee_bean'),
      ('www.baristapaulbassett.co.kr', '폴 바셋', 'https://www.baristapaulbassett.co.kr/society/Introduction.pb', 'brand_official', 'paul_bassett'),
      ('www.h-point.co.kr', 'H.Point', 'https://www.h-point.co.kr/cu/hpoint/hpoint.nhd', 'benefit_provider', 'hpoint'),
      ('bluefit.shinsegae.com', '신세계포인트', 'https://bluefit.shinsegae.com/service/membership/shinsegae-point.do', 'benefit_provider', 'shinsegae_point'),
      ('www.yes24.com', '예스24', 'https://www.yes24.com/notice/FirstClass/infoYESPoint.aspx', 'brand_official', 'yes24'),
      ('www.aladin.co.kr', '알라딘', 'https://www.aladin.co.kr/cs_center/wcs_guide_detail.aspx?pn=mybank_01', 'brand_official', 'aladin'),
      ('www.okcashbag.com', 'OK캐쉬백', 'https://www.okcashbag.com/clientservice/faqDetail.do', 'benefit_provider', 'okcashbag'),
      ('www.musinsa.com', '무신사', 'https://www.musinsa.com/onboarding/firstbuy', 'brand_official', 'musinsa'),
      ('aws.amazon.com', 'AWS Educate', 'https://aws.amazon.com/education/awseducate/', 'brand_official', 'aws'),
      ('www.tableau.com', 'Tableau Academic Programs', 'https://www.tableau.com/academic/students', 'brand_official', 'tableau'),
      ('m.lpoint.com', 'L.POINT', 'https://m.lpoint.com/app/card/LWCA100200.do?cdId=11001&desId=11001002', 'benefit_provider', 'lpoint'),
      ('www.lotteeatz.com', 'LOTTE EATZ', 'https://www.lotteeatz.com/', 'benefit_provider', 'lotteria')
), selected AS (
    SELECT source.*, target.brand_id
    FROM source
    JOIN brand_discovery_targets AS target
      ON target.target_key = source.primary_target_key
     AND target.brand_id IS NOT NULL
)
INSERT INTO benefit_source_domains (
    brand_id, organization_name, domain, source_role,
    crawl_allowed, verification_status, verification_reference, verified_at,
    compliance_status, collection_mode, policy_notes, is_active
)
SELECT
    selected.brand_id,
    selected.organization_name,
    selected.domain,
    selected.source_role,
    false,
    'verified',
    selected.verification_reference,
    now(),
    'pending',
    'manual_review',
    'Official source manually verified on 2026-07-21; automated collection remains separately gated.',
    true
FROM selected
ON CONFLICT (domain) DO UPDATE SET
    brand_id = COALESCE(benefit_source_domains.brand_id, EXCLUDED.brand_id),
    verification_status = CASE
        WHEN benefit_source_domains.verification_status = 'pending'
         AND benefit_source_domains.crawl_allowed = false
        THEN 'verified'
        ELSE benefit_source_domains.verification_status
    END,
    verification_reference = CASE
        WHEN benefit_source_domains.verification_status = 'pending'
         AND benefit_source_domains.crawl_allowed = false
        THEN EXCLUDED.verification_reference
        ELSE benefit_source_domains.verification_reference
    END,
    verified_at = CASE
        WHEN benefit_source_domains.verification_status = 'pending'
         AND benefit_source_domains.crawl_allowed = false
        THEN now()
        ELSE benefit_source_domains.verified_at
    END,
    last_discovered_at = now(),
    updated_at = now()
WHERE benefit_source_domains.verification_status <> 'rejected';

WITH mapping(domain, target_key) AS (
    VALUES
      ('www.lottecinema.co.kr', 'lottecinema'),
      ('gs25.gsretail.com', 'gs25'),
      ('www.daiso.co.kr', 'daiso'),
      ('www.coffeebeankorea.com', 'coffee_bean'),
      ('www.baristapaulbassett.co.kr', 'paul_bassett'),
      ('www.h-point.co.kr', 'hpoint'),
      ('www.h-point.co.kr', 'hyundai_department'),
      ('www.h-point.co.kr', 'kyobo'),
      ('bluefit.shinsegae.com', 'shinsegae_point'),
      ('bluefit.shinsegae.com', 'shinsegae_department'),
      ('bluefit.shinsegae.com', 'ssg'),
      ('www.yes24.com', 'yes24'),
      ('www.aladin.co.kr', 'aladin'),
      ('www.okcashbag.com', 'okcashbag'),
      ('www.musinsa.com', 'musinsa'),
      ('aws.amazon.com', 'aws'),
      ('www.tableau.com', 'tableau'),
      ('m.lpoint.com', 'lpoint'),
      ('m.lpoint.com', 'lottecinema'),
      ('m.lpoint.com', 'seven_eleven'),
      ('m.lpoint.com', 'lotteria'),
      ('m.lpoint.com', 'angelinus'),
      ('m.lpoint.com', 'krispy_kreme'),
      ('m.lpoint.com', 'lotteworld'),
      ('www.lotteeatz.com', 'lotteria'),
      ('www.lotteeatz.com', 'angelinus'),
      ('www.lotteeatz.com', 'krispy_kreme')
)
INSERT INTO benefit_source_domain_brands (domain_id, brand_id)
SELECT DISTINCT source.id, target.brand_id
FROM mapping
JOIN benefit_source_domains AS source
  ON source.domain = mapping.domain
JOIN brand_discovery_targets AS target
  ON target.target_key = mapping.target_key
 AND target.brand_id IS NOT NULL
WHERE source.verification_status = 'verified'
  AND source.is_active = true
ON CONFLICT (domain_id, brand_id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- 3. Publish facts manually checked against the official pages above.
-- ---------------------------------------------------------------------------

WITH benefit(
    source_id, official_url, title, summary, provider_name,
    min_age, max_age, benefit_category, tags, recheck_interval_seconds
) AS (
    VALUES
      (
        'https://www.lottecinema.co.kr/NLCHS/Membership/l_point',
        'https://www.lottecinema.co.kr/NLCHS/Membership/l_point',
        '롯데시네마 L.POINT 적립·사용',
        '롯데시네마 회원은 대상 영화 관람 결제에서 L.POINT를 적립하고 보유 포인트를 예매 등에 사용할 수 있습니다. 카드 등록 전 적립률은 0.1%이며 회원 등급과 결제·관람권 종류에 따라 적립률과 제외 조건이 달라질 수 있으므로 예매 전 공식 안내를 확인해야 합니다.',
        '롯데시네마·L.POINT', NULL, NULL, 'membership',
        '["롯데시네마","영화","L.POINT","포인트 적립","포인트 사용"]'::JSONB,
        2592000
      ),
      (
        'https://gs25.gsretail.com/gscvs/ko/membership-services/gs-retail',
        'https://gs25.gsretail.com/gscvs/ko/membership-services/gs-retail',
        'GS25 GS ALL 멤버십 포인트',
        'GS ALL 멤버십 회원은 GS25를 포함한 GS리테일 브랜드 이용금액의 0.1%를 포인트로 적립하고, 온·오프라인 참여 매장에서 10P를 10원으로 사용할 수 있습니다. 등급 달성 시 GS25·GS THE FRESH·GS SHOP의 등급 혜택과 쿠폰도 함께 이용할 수 있으며 일부 품목은 제외됩니다.',
        'GS25·GS리테일', NULL, NULL, 'membership',
        '["GS25","GS ALL","편의점","0.1% 적립","10P 사용","등급 쿠폰"]'::JSONB,
        2592000
      ),
      (
        'https://www.daiso.co.kr/cs/faq?cid=21#membership-points',
        'https://www.daiso.co.kr/cs/faq?cid=21',
        '다이소 멤버십 포인트 적립·사용',
        '다이소 멤버십 회원은 매장과 다이소몰에서 쿠폰·배송비·포인트 사용액을 제외한 실결제금액의 0.1%를 적립할 수 있습니다. 500원 이하 구매는 적립되지 않으며 적립 포인트는 매장과 다이소몰에서 50P 단위로 사용할 수 있습니다. 일부 매장은 포인트 사용이 제한될 수 있습니다.',
        '다이소', NULL, NULL, 'membership',
        '["다이소","생활용품","멤버십","0.1% 적립","50P 단위 사용"]'::JSONB,
        2592000
      ),
      (
        'https://www.coffeebeankorea.com/card/about.asp',
        'https://www.coffeebeankorea.com/card/about.asp',
        '커피빈 멤버스클럽 콩·등급 혜택',
        '등록한 커피빈 카드로 음료 또는 베이커리를 구매하면 품목당 콩 1개가 적립되고 12개월 실적에 따라 등급이 정해집니다. 카드 최초 등록과 3만원 이상 충전 시 무료음료 쿠폰이 제공되며, Gold·VVIP 등급은 무료 Extra, 무료음료권, 생일 무료음료 등 혜택을 받을 수 있습니다. 일부 입점 매장은 제외됩니다.',
        '커피빈코리아', NULL, NULL, 'membership',
        '["커피빈","멤버스클럽","콩 적립","무료 음료","생일 혜택","등급 혜택"]'::JSONB,
        2592000
      ),
      (
        'https://www.baristapaulbassett.co.kr/society/Introduction.pb',
        'https://www.baristapaulbassett.co.kr/society/Introduction.pb',
        '폴 바셋 Society 크라운·무료음료 혜택',
        '폴 바셋 Society 회원은 3천원 이상 결제 시 크라운 1개를 적립하고 1만원 이상부터 5천원 단위로 추가 적립합니다. 크라운 12개를 모으면 다음 날 무료음료 쿠폰이 발급되며, 등록한 폴 바셋 카드 첫 구매와 생일에도 무료음료 쿠폰을 받을 수 있습니다. 크라운 유효기간은 1년입니다.',
        '폴 바셋', NULL, NULL, 'membership',
        '["폴바셋","Society","크라운","12+1","무료 음료","생일 혜택"]'::JSONB,
        2592000
      ),
      (
        'https://www.h-point.co.kr/cu/hpoint/hpoint.nhd#integrated-membership',
        'https://www.h-point.co.kr/cu/hpoint/hpoint.nhd',
        'H.Point 통합 멤버십 적립·사용',
        'H.Point 회원은 현대백화점그룹 제휴 브랜드에서 결제금액의 약 0.1~0.5%를 제휴처별 기준에 따라 적립하고 보유 포인트를 현금처럼 사용할 수 있습니다. 현대백화점은 기본 0.1%, 조건 충족 시 0.5% 적립이 가능하며 현대아울렛·더현대닷컴·교보문고 등은 각기 다른 적립률과 최소 사용 기준이 적용됩니다.',
        'H.Point·현대백화점그룹', NULL, NULL, 'membership',
        '["H.Point","현대백화점","현대아울렛","교보문고","통합 멤버십","포인트"]'::JSONB,
        2592000
      ),
      (
        'https://bluefit.shinsegae.com/service/membership/shinsegae-point.do',
        'https://bluefit.shinsegae.com/service/membership/shinsegae-point.do',
        '신세계포인트 통합 적립·사용',
        '신세계포인트 회원은 신세계백화점, 이마트, SSG.COM 등 신세계포인트 가맹점에서 구매 실적에 따라 포인트를 적립하고 사용할 수 있습니다. 제휴처·상품·결제수단별 적립률과 사용 기준, 제외 매장이 다를 수 있으므로 결제 전 해당 가맹점과 공식 멤버십 안내를 확인해야 합니다.',
        '신세계포인트', NULL, NULL, 'membership',
        '["신세계포인트","신세계백화점","이마트","SSG.COM","통합 멤버십"]'::JSONB,
        2592000
      ),
      (
        'https://www.yes24.com/notice/FirstClass/infoYESPoint.aspx',
        'https://www.yes24.com/notice/FirstClass/infoYESPoint.aspx',
        '예스24 YES포인트·YES머니 혜택',
        '예스24 회원은 상품 구매, 리뷰와 이벤트 참여 등으로 YES포인트를 적립할 수 있습니다. 5천 포인트 이상 모으면 YES머니로 전환해 현금처럼 사용할 수 있고, YES마니아 회원은 대상 상품에서 등급별 1~3% 추가 적립을 받을 수 있습니다. YES포인트는 공식 안내상 유효기간이 없습니다.',
        '예스24', NULL, NULL, 'membership',
        '["예스24","YES포인트","YES머니","도서","등급 추가 적립","유효기간 없음"]'::JSONB,
        2592000
      ),
      (
        'https://www.aladin.co.kr/cs_center/wcs_guide_detail.aspx?pn=mybank_01',
        'https://www.aladin.co.kr/cs_center/wcs_guide_detail.aspx?pn=mybank_01',
        '알라딘 멤버십 마일리지 혜택',
        '알라딘 회원은 상품별 마일리지와 최근 3개월 구매액에 따른 1~3% 멤버십 마일리지를 적립할 수 있습니다. 대상 주문이 5만원 이상이면 조건에 따라 2천점이 추가되며, 5천점 이상부터 5천점 단위로 적립금으로 전환해 결제에 사용할 수 있습니다. 마일리지 유효기간은 발급일부터 1년입니다.',
        '알라딘', NULL, NULL, 'membership',
        '["알라딘","도서","마일리지","1~3% 추가 적립","적립금 전환"]'::JSONB,
        2592000
      ),
      (
        'https://www.okcashbag.com/clientservice/faqDetail.do#membership-service',
        'https://www.okcashbag.com/clientservice/faqDetail.do',
        'OK캐쉬백 포인트 적립·사용 서비스',
        'OK캐쉬백 회원은 온·오프라인 제휴 가맹점과 제휴카드에서 포인트를 적립하고 사용할 수 있으며 앱 인증 후 온라인 사용, 포인트 조회, 전환·합산과 현금상환 등 회원 서비스를 이용할 수 있습니다. 가맹점별 적립률과 사용 기준이 다르고 일부 적립 포인트에는 서비스 이용 수수료가 반영될 수 있습니다.',
        'OK캐쉬백', NULL, NULL, 'membership',
        '["OK캐쉬백","포인트","제휴 가맹점","포인트 합산","현금상환"]'::JSONB,
        2592000
      ),
      (
        'https://www.musinsa.com/onboarding/firstbuy',
        'https://www.musinsa.com/onboarding/firstbuy',
        '무신사 신규회원 첫 구매 혜택',
        '무신사 신규 회원은 가입과 동시에 20% 할인 쿠폰과 5천원 적립금을 받을 수 있습니다. 쿠폰은 30일, 적립금은 가입 후 24시간 동안 사용할 수 있고 전 상품 무료배송 혜택도 제공됩니다. 신규 가입 혜택은 계정당 1회이며 일부 상품은 쿠폰 적용에서 제외될 수 있습니다.',
        '무신사', NULL, NULL, 'discount',
        '["무신사","신규회원","20% 쿠폰","5천원 적립금","무료배송","첫 구매"]'::JSONB,
        2592000
      ),
      (
        'https://aws.amazon.com/education/awseducate/',
        'https://aws.amazon.com/education/awseducate/',
        'AWS Educate 무료 클라우드 학습',
        'AWS Educate는 교육 단계나 기술 경험과 관계없이 무료로 이용할 수 있는 클라우드·AI 학습 프로그램입니다. 만 13세 이상 학습자는 이메일만으로 가입해 신용카드 없이 실습 랩과 자기주도형 교육 콘텐츠를 이용하고 디지털 배지를 받을 수 있으며, 만 18세 이상은 채용·인턴십 게시판도 이용할 수 있습니다.',
        'Amazon Web Services', 13, NULL, 'software',
        '["AWS","클라우드","AI","무료 학습","실습 랩","신용카드 불필요"]'::JSONB,
        7776000
      ),
      (
        'https://www.tableau.com/academic/students',
        'https://www.tableau.com/academic/students',
        'Tableau for Students 무료 학습 도구',
        '학생과 학습자는 Tableau for Students 안내를 통해 Tableau Desktop Public Edition을 무료로 내려받고 교육 영상과 커뮤니티 자료를 이용할 수 있습니다. 갱신은 필요하지 않지만 결과물은 Tableau Public에 공개되며 상업적 사용과 Tableau Cloud 게시에는 사용할 수 없습니다.',
        'Tableau', NULL, NULL, 'software',
        '["학생","Tableau","데이터 시각화","무료","Public Edition","비상업용"]'::JSONB,
        7776000
      ),
      (
        'https://m.lpoint.com/app/card/LWCA100200.do?cdId=11001&desId=11001002',
        'https://m.lpoint.com/app/card/LWCA100200.do?cdId=11001&desId=11001002',
        'L.POINT 통합 멤버십 적립·사용',
        'L.POINT 정회원은 무료 모바일 카드를 발급받아 롯데백화점·세븐일레븐·롯데시네마·롯데월드·롯데리아·엔제리너스·크리스피크림도넛 등 참여 제휴사에서 포인트 적립과 사용 혜택을 받을 수 있습니다. 적립률과 사용 단위, 제외 상품은 제휴사와 회원 등급에 따라 달라집니다.',
        'L.POINT', NULL, NULL, 'membership',
        '["L.POINT","롯데","통합 멤버십","포인트 적립","포인트 사용"]'::JSONB,
        2592000
      ),
      (
        'https://www.lotteeatz.com/#membership-lotteria',
        'https://www.lotteeatz.com/',
        '롯데리아 LOTTE EATZ 등급·쿠폰 혜택',
        'LOTTE EATZ 롯데리아 회원은 최근 6개월 구매액에 따라 Regular·Gold·VIP·VVIP 등급을 받고 월별 할인쿠폰과 승급·생일 혜택을 이용할 수 있습니다. Regular 회원도 10장의 월 쿠폰과 5천원 이상 첫 구매 시 소프트콘 쿠폰을 받을 수 있으며 상위 등급에는 배달 주문 마일과 추가 쿠폰이 제공됩니다.',
        '롯데리아·LOTTE EATZ', NULL, NULL, 'membership',
        '["롯데리아","LOTTE EATZ","등급 혜택","할인 쿠폰","첫 구매 쿠폰"]'::JSONB,
        2592000
      ),
      (
        'https://www.lotteeatz.com/#membership-angelinus',
        'https://www.lotteeatz.com/',
        '엔제리너스 LOTTE EATZ 스탬프·등급 혜택',
        'LOTTE EATZ 엔제리너스 회원은 최근 12개월 구매액에 따라 등급별 승급·생일 쿠폰을 받을 수 있습니다. 첫 구매 시 아메리카노S 쿠폰이 제공되고, 최종 결제금액 기준으로 적립한 엔젤 스탬프 10개마다 아메리카노S 무료 쿠폰을 받을 수 있습니다. 스탬프 적립 제외 조건은 공식 안내를 확인해야 합니다.',
        '엔제리너스·LOTTE EATZ', NULL, NULL, 'membership',
        '["엔제리너스","LOTTE EATZ","스탬프 10개","무료 커피","생일 쿠폰"]'::JSONB,
        2592000
      ),
      (
        'https://www.lotteeatz.com/#membership-krispy-kreme',
        'https://www.lotteeatz.com/',
        '크리스피크림 LOTTE EATZ 스탬프·등급 혜택',
        'LOTTE EATZ 크리스피크림 회원은 최근 12개월 구매액에 따라 등급별 승급·생일 쿠폰을 받을 수 있습니다. 첫 구매 시 오리지널 글레이즈드와 아메리카노 싱글세트 쿠폰이 제공되고, 5천원 결제마다 스탬프를 적립해 10개 달성 시 오리지널 글레이즈드 하프더즌 쿠폰을 받을 수 있습니다.',
        '크리스피크림도넛·LOTTE EATZ', NULL, NULL, 'membership',
        '["크리스피크림","LOTTE EATZ","스탬프 10개","하프더즌","첫 구매 쿠폰"]'::JSONB,
        2592000
      )
)
INSERT INTO programs (
    program_type, source_type, source_id, title, summary,
    provider_name, official_url, program_status,
    tags, raw_payload, search_tsv, min_age, max_age, regions,
    is_active, last_synced_at, company_name, benefit_category, provider_kind,
    source_health, source_verified_at, source_review_due_at
)
SELECT
    'corporate_benefit',
    'corporate_official',
    benefit.source_id,
    benefit.title,
    benefit.summary,
    benefit.provider_name,
    benefit.official_url,
    'always_open',
    benefit.tags,
    jsonb_build_object(
        'curation', jsonb_build_object(
            'batch', '2026-07-21-expansion-2',
            'status', 'verified',
            'reviewed_at', '2026-07-21',
            'review_method', 'manual_official_page',
            'official_url', benefit.official_url
        )
    ),
    to_tsvector(
        'simple',
        concat_ws(' ', benefit.title, benefit.summary, benefit.provider_name)
    ),
    benefit.min_age::INTEGER,
    benefit.max_age::INTEGER,
    ARRAY[]::TEXT[],
    true,
    now(),
    benefit.provider_name,
    benefit.benefit_category,
    'corporate',
    'current',
    now(),
    now() + make_interval(secs => benefit.recheck_interval_seconds)
FROM benefit
ON CONFLICT (source_type, source_id) DO NOTHING;

WITH link(source_id, target_key, is_primary) AS (
    VALUES
      ('https://www.lottecinema.co.kr/NLCHS/Membership/l_point', 'lottecinema', true),
      ('https://gs25.gsretail.com/gscvs/ko/membership-services/gs-retail', 'gs25', true),
      ('https://www.daiso.co.kr/cs/faq?cid=21#membership-points', 'daiso', true),
      ('https://www.coffeebeankorea.com/card/about.asp', 'coffee_bean', true),
      ('https://www.baristapaulbassett.co.kr/society/Introduction.pb', 'paul_bassett', true),
      ('https://www.h-point.co.kr/cu/hpoint/hpoint.nhd#integrated-membership', 'hpoint', true),
      ('https://www.h-point.co.kr/cu/hpoint/hpoint.nhd#integrated-membership', 'hyundai_department', false),
      ('https://www.h-point.co.kr/cu/hpoint/hpoint.nhd#integrated-membership', 'kyobo', false),
      ('https://bluefit.shinsegae.com/service/membership/shinsegae-point.do', 'shinsegae_point', true),
      ('https://bluefit.shinsegae.com/service/membership/shinsegae-point.do', 'shinsegae_department', false),
      ('https://bluefit.shinsegae.com/service/membership/shinsegae-point.do', 'ssg', false),
      ('https://www.yes24.com/notice/FirstClass/infoYESPoint.aspx', 'yes24', true),
      ('https://www.aladin.co.kr/cs_center/wcs_guide_detail.aspx?pn=mybank_01', 'aladin', true),
      ('https://www.okcashbag.com/clientservice/faqDetail.do#membership-service', 'okcashbag', true),
      ('https://www.musinsa.com/onboarding/firstbuy', 'musinsa', true),
      ('https://aws.amazon.com/education/awseducate/', 'aws', true),
      ('https://www.tableau.com/academic/students', 'tableau', true),
      ('https://m.lpoint.com/app/card/LWCA100200.do?cdId=11001&desId=11001002', 'lpoint', true),
      ('https://m.lpoint.com/app/card/LWCA100200.do?cdId=11001&desId=11001002', 'lottecinema', false),
      ('https://m.lpoint.com/app/card/LWCA100200.do?cdId=11001&desId=11001002', 'seven_eleven', false),
      ('https://m.lpoint.com/app/card/LWCA100200.do?cdId=11001&desId=11001002', 'lotteria', false),
      ('https://m.lpoint.com/app/card/LWCA100200.do?cdId=11001&desId=11001002', 'angelinus', false),
      ('https://m.lpoint.com/app/card/LWCA100200.do?cdId=11001&desId=11001002', 'krispy_kreme', false),
      ('https://m.lpoint.com/app/card/LWCA100200.do?cdId=11001&desId=11001002', 'lotteworld', false),
      ('https://www.lotteeatz.com/#membership-lotteria', 'lotteria', true),
      ('https://www.lotteeatz.com/#membership-angelinus', 'angelinus', true),
      ('https://www.lotteeatz.com/#membership-krispy-kreme', 'krispy_kreme', true)
)
INSERT INTO program_brands (
    program_id, brand_id, relationship_type, is_primary
)
SELECT
    program.id,
    target.brand_id,
    'redeemable_at',
    link.is_primary
FROM link
JOIN programs AS program
  ON program.source_type = 'corporate_official'
 AND program.source_id = link.source_id
JOIN brand_discovery_targets AS target
  ON target.target_key = link.target_key
 AND target.brand_id IS NOT NULL
ON CONFLICT (program_id, brand_id, relationship_type) DO NOTHING;

-- ---------------------------------------------------------------------------
-- 4. Install source monitoring immediately. Unlike the first expansion, this
--    migration runs after the fail-closed monitoring constraint exists.
-- ---------------------------------------------------------------------------

WITH batch_program AS (
    SELECT
        program.id AS program_id,
        regexp_replace(btrim(program.official_url), '#.*$', '') AS canonical_url,
        lower(substring(btrim(program.official_url) FROM '^https://([^/?#]+)')) AS source_domain,
        CASE
            WHEN program.benefit_category IN ('software', 'student_discount')
                THEN 7776000
            ELSE 2592000
        END AS interval_seconds,
        COALESCE(program.source_verified_at, program.last_synced_at, now()) AS verified_at
    FROM programs AS program
    WHERE program.source_type = 'corporate_official'
      AND program.raw_payload -> 'curation' ->> 'batch' = '2026-07-21-expansion-2'
      AND program.official_url ~ '^https://'
), matched AS (
    SELECT DISTINCT ON (batch_program.canonical_url)
        batch_program.canonical_url,
        source.id AS domain_id,
        min(batch_program.interval_seconds) OVER (
            PARTITION BY batch_program.canonical_url
        ) AS interval_seconds,
        min(batch_program.verified_at) OVER (
            PARTITION BY batch_program.canonical_url
        ) AS verified_at
    FROM batch_program
    JOIN benefit_source_domains AS source
      ON source.verification_status = 'verified'
     AND source.is_active = true
     AND (
         source.domain = batch_program.source_domain
         OR (
             source.allow_subdomains = true
             AND batch_program.source_domain LIKE '%.' || source.domain
         )
     )
    ORDER BY batch_program.canonical_url, char_length(source.domain) DESC
)
INSERT INTO benefit_source_candidates (
    domain_id, url, canonical_url, discovered_via, status,
    candidate_kind, discovery_depth, next_check_at,
    recheck_interval_seconds, discovered_at, last_discovered_at,
    created_at, updated_at
)
SELECT
    matched.domain_id,
    matched.canonical_url,
    matched.canonical_url,
    'manual',
    'approved',
    'detail',
    0,
    matched.verified_at + make_interval(secs => matched.interval_seconds),
    matched.interval_seconds,
    now(),
    now(),
    now(),
    now()
FROM matched
ON CONFLICT (canonical_url) DO UPDATE SET
    status = CASE
        WHEN benefit_source_candidates.status = 'rejected'
        THEN benefit_source_candidates.status
        ELSE 'approved'
    END,
    recheck_interval_seconds = LEAST(
        benefit_source_candidates.recheck_interval_seconds,
        EXCLUDED.recheck_interval_seconds
    ),
    next_check_at = CASE
        WHEN benefit_source_candidates.next_check_at IS NULL
        THEN EXCLUDED.next_check_at
        ELSE LEAST(benefit_source_candidates.next_check_at, EXCLUDED.next_check_at)
    END,
    last_discovered_at = now(),
    raw_text = NULL,
    extracted_payload = NULL,
    updated_at = now();

WITH batch_program AS (
    SELECT
        program.id AS program_id,
        regexp_replace(btrim(program.official_url), '#.*$', '') AS canonical_url,
        CASE
            WHEN program.benefit_category IN ('software', 'student_discount')
                THEN 7776000
            ELSE 2592000
        END AS interval_seconds,
        COALESCE(program.source_verified_at, program.last_synced_at, now()) AS verified_at
    FROM programs AS program
    WHERE program.source_type = 'corporate_official'
      AND program.raw_payload -> 'curation' ->> 'batch' = '2026-07-21-expansion-2'
)
INSERT INTO program_source_monitors (
    program_id, candidate_id, is_primary, verification_state,
    verified_content_hash, verified_at, review_due_at,
    recheck_interval_seconds
)
SELECT
    batch_program.program_id,
    candidate.id,
    true,
    'current',
    candidate.content_hash,
    batch_program.verified_at,
    batch_program.verified_at + make_interval(secs => batch_program.interval_seconds),
    batch_program.interval_seconds
FROM batch_program
JOIN benefit_source_candidates AS candidate
  ON candidate.canonical_url = batch_program.canonical_url
 AND candidate.status <> 'rejected'
ON CONFLICT (program_id, candidate_id) DO NOTHING;

INSERT INTO benefit_source_checks (
    candidate_id, check_kind, previous_content_hash, content_hash,
    http_status, fetched_bytes, review_status, detected_at,
    reviewed_at, review_note
)
SELECT DISTINCT
    candidate.id,
    'manual_confirmation',
    candidate.content_hash,
    candidate.content_hash,
    candidate.http_status,
    candidate.fetched_bytes,
    'confirmed',
    now(),
    now(),
    'Official page manually reviewed for the 2026-07-21 expansion batch.'
FROM programs AS program
JOIN program_source_monitors AS monitor ON monitor.program_id = program.id
JOIN benefit_source_candidates AS candidate ON candidate.id = monitor.candidate_id
WHERE program.source_type = 'corporate_official'
  AND program.raw_payload -> 'curation' ->> 'batch' = '2026-07-21-expansion-2'
  AND NOT EXISTS (
      SELECT 1
      FROM benefit_source_checks AS existing
      WHERE existing.candidate_id = candidate.id
        AND existing.check_kind = 'manual_confirmation'
        AND existing.review_note =
            'Official page manually reviewed for the 2026-07-21 expansion batch.'
  );

UPDATE programs AS program
SET source_health = 'current',
    source_verified_at = monitor.verified_at,
    source_review_due_at = monitor.review_due_at,
    source_change_detected_at = NULL,
    updated_at = now()
FROM program_source_monitors AS monitor
WHERE program.id = monitor.program_id
  AND program.source_type = 'corporate_official'
  AND program.raw_payload -> 'curation' ->> 'batch' = '2026-07-21-expansion-2'
  AND monitor.verification_state = 'current';

-- Fail the deployment atomically if any reviewed record is missing its public
-- graph edge, source monitor, or data-quality contract.
DO $$
DECLARE
    published_count INT;
    unlinked_count INT;
    unmonitored_count INT;
    quality_issue_count INT;
BEGIN
    SELECT count(*)
    INTO published_count
    FROM programs
    WHERE source_type = 'corporate_official'
      AND is_active = true
      AND source_health = 'current'
      AND raw_payload -> 'curation' ->> 'batch' = '2026-07-21-expansion-2';

    IF published_count <> 17 THEN
        RAISE EXCEPTION
            'Expected 17 verified round-two corporate benefits, found %',
            published_count;
    END IF;

    SELECT count(*)
    INTO unlinked_count
    FROM programs AS program
    WHERE program.source_type = 'corporate_official'
      AND program.raw_payload -> 'curation' ->> 'batch' = '2026-07-21-expansion-2'
      AND NOT EXISTS (
          SELECT 1 FROM program_brands AS link
          WHERE link.program_id = program.id
      );

    IF unlinked_count <> 0 THEN
        RAISE EXCEPTION
            'Round-two expansion contains % unlinked benefits', unlinked_count;
    END IF;

    SELECT count(*)
    INTO unmonitored_count
    FROM programs AS program
    WHERE program.source_type = 'corporate_official'
      AND program.raw_payload -> 'curation' ->> 'batch' = '2026-07-21-expansion-2'
      AND NOT EXISTS (
          SELECT 1
          FROM program_source_monitors AS monitor
          WHERE monitor.program_id = program.id
            AND monitor.verification_state = 'current'
      );

    IF unmonitored_count <> 0 THEN
        RAISE EXCEPTION
            'Round-two expansion contains % unmonitored benefits', unmonitored_count;
    END IF;

    SELECT count(*)
    INTO quality_issue_count
    FROM program_data_quality_issues AS issue
    JOIN programs AS program ON program.id = issue.program_id
    WHERE issue.status = 'open'
      AND program.raw_payload -> 'curation' ->> 'batch' = '2026-07-21-expansion-2';

    IF quality_issue_count <> 0 THEN
        RAISE EXCEPTION
            'Round-two expansion contains % open data-quality issues',
            quality_issue_count;
    END IF;
END
$$;
