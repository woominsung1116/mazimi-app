-- Expand Mazimi's corporate-benefit catalogue from the initial proof batch.
--
-- This migration keeps three different concepts deliberately separate:
--   1. brand_discovery_targets: high-recall monitoring coverage;
--   2. benefit_source_domains: verified provenance, never crawl permission;
--   3. programs: facts manually re-checked on official pages and safe to publish.
--
-- Every source registered below remains fail-closed (`crawl_allowed=false`).
-- The public rows were manually reviewed against their official pages on
-- 2026-07-16; discovery candidates are never promoted automatically.

-- ---------------------------------------------------------------------------
-- 1. Broaden the monitoring universe. A target does not claim that a benefit
--    exists and does not authorise direct access to the target's website.
-- ---------------------------------------------------------------------------

WITH seed(target_key, display_name, category, priority) AS (
    VALUES
      ('paris_baguette', '파리바게뜨', '카페·디저트', 92),
      ('tous_les_jours', '뚜레쥬르', '카페·디저트', 90),
      ('pascucci', '파스쿠찌', '카페·디저트', 84),
      ('coffee_bean', '커피빈', '카페·디저트', 82),
      ('angelinus', '엔제리너스', '카페·디저트', 80),
      ('blue_bottle', '블루보틀', '카페·디저트', 78),
      ('caffe_bene', '카페베네', '카페·디저트', 72),
      ('artisee', '아티제', '카페·디저트', 75),
      ('mammoth_coffee', '매머드커피', '카페·디저트', 76),
      ('ten_percent', '텐퍼센트커피', '카페·디저트', 74),
      ('dessert39', '디저트39', '카페·디저트', 76),
      ('krispy_kreme', '크리스피크림도넛', '카페·디저트', 80),
      ('godiva', '고디바', '카페·디저트', 72),
      ('cafe_knotted', '노티드', '카페·디저트', 78),
      ('london_bagel', '런던베이글뮤지엄', '카페·디저트', 70),
      ('nene_chicken', '네네치킨', '외식·배달', 82),
      ('puradak', '푸라닭', '외식·배달', 80),
      ('cheogatjip', '처갓집양념치킨', '외식·배달', 78),
      ('norang_tongdak', '노랑통닭', '외식·배달', 78),
      ('sixty_chicken', '60계치킨', '외식·배달', 78),
      ('popeyes', '파파이스', '외식·배달', 75),
      ('shake_shack', '쉐이크쉑', '외식·배달', 82),
      ('outback', '아웃백스테이크하우스', '외식·배달', 84),
      ('vips', 'VIPS', '외식·배달', 82),
      ('ashley', '애슐리', '외식·배달', 82),
      ('mad_for_garlic', '매드포갈릭', '외식·배달', 74),
      ('yupdduk', '동대문엽기떡볶이', '외식·배달', 84),
      ('sinjeon', '신전떡볶이', '외식·배달', 80),
      ('jaws_tteokbokki', '죠스떡볶이', '외식·배달', 72),
      ('bonjuk', '본죽', '외식·배달', 80),
      ('hansot', '한솥도시락', '외식·배달', 82),
      ('hongkong_banjeom', '홍콩반점0410', '외식·배달', 78),
      ('rolling_pasta', '롤링파스타', '외식·배달', 72),
      ('pizza_school', '피자스쿨', '외식·배달', 78),
      ('pizza_alvolo', '피자알볼로', '외식·배달', 76),
      ('myeongryun_jinsa', '명륜진사갈비', '외식·배달', 76),
      ('kurly', '컬리', '뷰티·쇼핑', 92),
      ('ssg', 'SSG닷컴', '뷰티·쇼핑', 90),
      ('lotteon', '롯데ON', '뷰티·쇼핑', 86),
      ('elevenst', '11번가', '뷰티·쇼핑', 88),
      ('gmarket', 'G마켓', '뷰티·쇼핑', 88),
      ('auction', '옥션', '뷰티·쇼핑', 82),
      ('cj_onstyle', 'CJ온스타일', '뷰티·쇼핑', 84),
      ('amoremall', '아모레몰', '뷰티·쇼핑', 82),
      ('innisfree', '이니스프리', '뷰티·쇼핑', 82),
      ('etude', '에뛰드', '뷰티·쇼핑', 78),
      ('aritaum', '아리따움', '뷰티·쇼핑', 75),
      ('thefaceshop', '더페이스샵', '뷰티·쇼핑', 78),
      ('nature_republic', '네이처리퍼블릭', '뷰티·쇼핑', 76),
      ('tonymoly', '토니모리', '뷰티·쇼핑', 76),
      ('missha', '미샤', '뷰티·쇼핑', 78),
      ('chicor', '시코르', '뷰티·쇼핑', 76),
      ('lotte_department', '롯데백화점', '뷰티·쇼핑', 88),
      ('shinsegae_department', '신세계백화점', '뷰티·쇼핑', 88),
      ('hyundai_department', '현대백화점', '뷰티·쇼핑', 88),
      ('ak_plaza', 'AK플라자', '뷰티·쇼핑', 78),
      ('nc_department', 'NC백화점', '뷰티·쇼핑', 74),
      ('lotte_dutyfree', '롯데면세점', '뷰티·쇼핑', 82),
      ('shilla_dutyfree', '신라면세점', '뷰티·쇼핑', 82),
      ('shinsegae_dutyfree', '신세계면세점', '뷰티·쇼핑', 80),
      ('hyundai_dutyfree', '현대면세점', '뷰티·쇼핑', 78),
      ('photoism', '포토이즘', '영화·문화', 82),
      ('life4cuts', '인생네컷', '영화·문화', 82),
      ('photo_signature', '포토시그니처', '영화·문화', 76),
      ('harufilm', '하루필름', '영화·문화', 74),
      ('youngpoong', '영풍문고', '영화·문화', 80),
      ('cultureland', '컬쳐랜드', '영화·문화', 84),
      ('kt_m_mobile', 'KT M모바일', '통신·디지털', 84),
      ('uplus_u_mobile', 'U+유모바일', '통신·디지털', 84),
      ('sk7mobile', 'SK세븐모바일', '통신·디지털', 84),
      ('hello_mobile', '헬로모바일', '통신·디지털', 80),
      ('toss_mobile', '토스모바일', '통신·디지털', 82),
      ('figma', 'Figma', '전자·소프트웨어', 94),
      ('miro', 'Miro', '전자·소프트웨어', 90),
      ('azure', 'Microsoft Azure', '전자·소프트웨어', 92),
      ('aws', 'AWS', '전자·소프트웨어', 92),
      ('google_cloud', 'Google Cloud', '전자·소프트웨어', 92),
      ('zoom', 'Zoom', '전자·소프트웨어', 84),
      ('dropbox', 'Dropbox', '전자·소프트웨어', 82),
      ('slack', 'Slack', '전자·소프트웨어', 82),
      ('mathworks', 'MathWorks', '전자·소프트웨어', 84),
      ('wolfram', 'Wolfram', '전자·소프트웨어', 80),
      ('dell', 'Dell', '전자·소프트웨어', 82),
      ('hp', 'HP', '전자·소프트웨어', 82),
      ('lenovo', 'Lenovo', '전자·소프트웨어', 84),
      ('asus', 'ASUS', '전자·소프트웨어', 84),
      ('acer', 'Acer', '전자·소프트웨어', 76),
      ('logitech', 'Logitech', '전자·소프트웨어', 82),
      ('sony', 'Sony', '전자·소프트웨어', 82),
      ('nintendo', 'Nintendo', '전자·소프트웨어', 80),
      ('playstation', 'PlayStation', '전자·소프트웨어', 82),
      ('airseoul', '에어서울', '교통·여행', 80),
      ('eastar', '이스타항공', '교통·여행', 80),
      ('lotte_rentacar', '롯데렌터카', '교통·여행', 82),
      ('sk_rentacar', 'SK렌터카', '교통·여행', 82),
      ('tmap', '티맵', '교통·여행', 84),
      ('uber', 'Uber', '교통·여행', 84),
      ('kickgoing', '킥고잉', '교통·여행', 78),
      ('swing', 'SWING', '교통·여행', 76),
      ('trip_com', 'Trip.com', '교통·여행', 82),
      ('agoda', 'Agoda', '교통·여행', 82),
      ('booking_com', 'Booking.com', '교통·여행', 82),
      ('airbnb', 'Airbnb', '교통·여행', 84),
      ('megastudy', '메가스터디', '교육·커리어', 88),
      ('etoos', '이투스', '교육·커리어', 84),
      ('daesung_mymac', '대성마이맥', '교육·커리어', 84),
      ('pagoda', '파고다', '교육·커리어', 80),
      ('duolingo', 'Duolingo', '교육·커리어', 84),
      ('cake', 'Cake', '교육·커리어', 78),
      ('ringle', '링글', '교육·커리어', 82),
      ('cambly', 'Cambly', '교육·커리어', 80),
      ('coursera', 'Coursera', '교육·커리어', 86),
      ('udemy', 'Udemy', '교육·커리어', 86),
      ('coloso', '콜로소', '교육·커리어', 82),
      ('datacamp', 'DataCamp', '교육·커리어', 82),
      ('ocean_world', '오션월드', '레저·테마파크', 82),
      ('vivaldi_park', '비발디파크', '레저·테마파크', 80),
      ('high1', '하이원리조트', '레저·테마파크', 80),
      ('yongpyong', '모나용평', '레저·테마파크', 78),
      ('kidzania', '키자니아', '레저·테마파크', 78),
      ('seoul_sky', '서울스카이', '레저·테마파크', 80),
      ('lotte_waterpark', '롯데워터파크', '레저·테마파크', 78),
      ('spoany', '스포애니', '레저·테마파크', 76),
      ('ablegym', '에이블짐', '레저·테마파크', 72),
      ('muji', '무인양품', '생활·쇼핑', 84),
      ('jaju', '자주', '생활·쇼핑', 80),
      ('modernhouse', '모던하우스', '생활·쇼핑', 80),
      ('butter', '버터', '생활·쇼핑', 76),
      ('artbox', '아트박스', '생활·쇼핑', 82),
      ('lpoint', 'L.POINT', '멤버십·포인트', 88),
      ('happy_point', '해피포인트', '멤버십·포인트', 90),
      ('cj_one', 'CJ ONE', '멤버십·포인트', 92),
      ('hpoint', 'H.Point', '멤버십·포인트', 84),
      ('okcashbag', 'OK캐쉬백', '멤버십·포인트', 86),
      ('gs_point', 'GS&POINT', '멤버십·포인트', 84),
      ('nike', 'Nike', '패션·스포츠', 88),
      ('adidas', 'Adidas', '패션·스포츠', 88),
      ('newbalance', 'New Balance', '패션·스포츠', 86),
      ('zara', 'ZARA', '패션·스포츠', 84),
      ('hm', 'H&M', '패션·스포츠', 84),
      ('kream', 'KREAM', '패션·스포츠', 88)
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
WHERE NOT EXISTS (
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
    WHERE target.brand_id IS NULL
)
UPDATE brand_discovery_targets AS target
SET brand_id = matched.brand_id,
    updated_at = now()
FROM ranked_matches AS matched
WHERE target.id = matched.target_id
  AND matched.match_rank = 1;

-- Only roots confirmed during the review are marked verified here. This is
-- search attribution metadata; direct collection still requires the separate
-- compliance workflow.
WITH official(
    target_key, official_domain, homepage_url
) AS (
    VALUES
      ('figma', 'www.figma.com', 'https://www.figma.com/'),
      ('canva', 'www.canva.com', 'https://www.canva.com/'),
      ('autodesk', 'www.autodesk.com', 'https://www.autodesk.com/'),
      ('jetbrains', 'www.jetbrains.com', 'https://www.jetbrains.com/'),
      ('miro', 'miro.com', 'https://miro.com/'),
      ('azure', 'azure.microsoft.com', 'https://azure.microsoft.com/'),
      ('cj_one', 'www.cjone.com', 'https://www.cjone.com/'),
      ('happy_point', 'www.happypointcard.com', 'https://www.happypointcard.com/'),
      ('ediya', 'members.ediya.com', 'https://members.ediya.com/'),
      ('gongcha', 'www.gong-cha.co.kr', 'https://www.gong-cha.co.kr/'),
      ('hollys', 'www.hollys.co.kr', 'https://www.hollys.co.kr/'),
      ('twosome', 'www.twosome.co.kr', 'https://www.twosome.co.kr/')
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
-- 2. Register the precise official-page domains used as evidence. Domain
--    verification is intentionally not crawl permission.
-- ---------------------------------------------------------------------------

WITH source(
    domain, organization_name, verification_reference, source_role,
    primary_target_key
) AS (
    VALUES
      ('help.figma.com', 'Figma Education', 'https://help.figma.com/hc/en-us/articles/360041061214-Figma-for-Education', 'brand_official', 'figma'),
      ('www.canva.com', 'Canva for Education', 'https://www.canva.com/education/', 'brand_official', 'canva'),
      ('www.autodesk.com', 'Autodesk Education', 'https://www.autodesk.com/education/home', 'brand_official', 'autodesk'),
      ('sales.jetbrains.com', 'JetBrains Education', 'https://sales.jetbrains.com/hc/en-gb/articles/207241195-Do-you-offer-free-educational-licenses-for-students-and-teachers', 'brand_official', 'jetbrains'),
      ('help.miro.com', 'Miro Education', 'https://help.miro.com/hc/en-us/articles/360017730473-Education-plan', 'brand_official', 'miro'),
      ('azure.microsoft.com', 'Microsoft Azure', 'https://azure.microsoft.com/en-us/free/students/', 'brand_official', 'azure'),
      ('docs.github.com', 'GitHub Copilot', 'https://docs.github.com/en/copilot/how-tos/copilot-on-github/set-up-copilot/enable-copilot/set-up-for-students', 'brand_official', 'github'),
      ('m.tworld.co.kr', 'SK텔레콤', 'https://m.tworld.co.kr/product/callplan?prod_id=NA00009785', 'brand_official', 'skt'),
      ('product.kt.com', 'KT', 'https://product.kt.com/benefit/membership/mobile/y-bonus.html', 'brand_official', 'kt'),
      ('www.lguplus.com', 'LG유플러스', 'https://www.lguplus.com/benefit-membership/rank-info', 'brand_official', 'lguplus'),
      ('www.cjone.com', 'CJ ONE', 'https://www.cjone.com/cjmweb/point-card/brand.do', 'benefit_provider', 'cj_one'),
      ('www.megabox.co.kr', '메가박스', 'https://www.megabox.co.kr/benefit/membership', 'brand_official', 'megabox'),
      ('members.ediya.com', '이디야커피', 'https://members.ediya.com/guide/membership', 'brand_official', 'ediya'),
      ('www.gong-cha.co.kr', '공차코리아', 'https://www.gong-cha.co.kr/brand/member/membership1.php', 'brand_official', 'gongcha'),
      ('m.hollys.co.kr', '할리스', 'https://m.hollys.co.kr/customer/faq.do', 'brand_official', 'hollys'),
      ('mo.twosome.co.kr', '투썸플레이스', 'https://mo.twosome.co.kr/co/faqList.do', 'brand_official', 'twosome'),
      ('www.happypointcard.com', '해피포인트', 'https://www.happypointcard.com/page/presentation/brand.spc', 'benefit_provider', 'happy_point')
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
    'Official source verified for factual review; direct collection remains disabled pending separate compliance approval.',
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
      ('help.figma.com', 'figma'),
      ('www.canva.com', 'canva'),
      ('www.autodesk.com', 'autodesk'),
      ('sales.jetbrains.com', 'jetbrains'),
      ('help.miro.com', 'miro'),
      ('azure.microsoft.com', 'azure'),
      ('docs.github.com', 'github'),
      ('m.tworld.co.kr', 'skt'),
      ('product.kt.com', 'kt'),
      ('www.lguplus.com', 'lguplus'),
      ('www.cjone.com', 'cj_one'),
      ('www.cjone.com', 'cgv'),
      ('www.cjone.com', 'oliveyoung'),
      ('www.cjone.com', 'mega_mgc'),
      ('www.cjone.com', 'cu'),
      ('www.cjone.com', 'emart24'),
      ('www.cjone.com', 'tving'),
      ('www.cjone.com', 'aqua_planet'),
      ('www.cjone.com', 'yanolja'),
      ('www.cjone.com', 'photoism'),
      ('www.cjone.com', 'tous_les_jours'),
      ('www.cjone.com', 'nene_chicken'),
      ('www.megabox.co.kr', 'megabox'),
      ('members.ediya.com', 'ediya'),
      ('www.gong-cha.co.kr', 'gongcha'),
      ('m.hollys.co.kr', 'hollys'),
      ('mo.twosome.co.kr', 'twosome'),
      ('www.happypointcard.com', 'happy_point'),
      ('www.happypointcard.com', 'paris_baguette'),
      ('www.happypointcard.com', 'baskin_robbins'),
      ('www.happypointcard.com', 'dunkin'),
      ('www.happypointcard.com', 'pascucci'),
      ('www.happypointcard.com', 'shake_shack')
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
-- 3. Publish only the facts manually checked on the official pages above.
--    Shared membership pages use a stable fragment in source_id so each
--    independently reviewed brand benefit retains its own identity.
-- ---------------------------------------------------------------------------

WITH benefit(
    source_id, official_url, target_key, title, summary, provider_name,
    min_age, max_age, benefit_category, tags
) AS (
    VALUES
      (
        'https://help.figma.com/hc/en-us/articles/360041061214-Figma-for-Education',
        'https://help.figma.com/hc/en-us/articles/360041061214-Figma-for-Education',
        'figma',
        'Figma for Education 무료 플랜',
        '인증된 학교의 학생과 교육자는 Figma for Education을 무료로 이용할 수 있습니다. 고등교육 학생은 Professional 수준의 교육용 팀을 이용하며 자격은 1년 단위로 다시 확인될 수 있습니다. 학교 유형과 지역에 따라 제공 플랜 및 인증 방식이 다르므로 공식 자격 안내를 확인해야 합니다.',
        'Figma', NULL, NULL, 'software',
        '["학생","교육자","디자인","Figma","무료","재인증"]'::JSONB
      ),
      (
        'https://www.canva.com/education/',
        'https://www.canva.com/education/',
        'canva',
        'Canva for Education 무료 이용',
        '적격 초·중·고 교사는 Canva for Education을 100% 무료로 이용하고 학생을 수업에 초대할 수 있습니다. 학생은 인증된 교사의 초대를 통해 참여하며 일반 대학·대학교 과정은 이 K-12 혜택 대상이 아니므로 학교 유형별 자격을 확인해야 합니다.',
        'Canva', NULL, NULL, 'software',
        '["초중고","교사","학생","디자인","무료","교사 초대"]'::JSONB
      ),
      (
        'https://www.autodesk.com/education/home',
        'https://www.autodesk.com/education/home',
        'autodesk',
        'Autodesk 학생·교육자 교육용 액세스',
        '적격 교육기관의 학생과 교육자는 Autodesk 소프트웨어 및 서비스를 1년간 무료로 이용할 수 있고, 자격이 유지되면 매년 갱신할 수 있습니다. 교육·훈련·연구 목적의 비상업적 사용에 한정되며 재학·재직 인증이 필요합니다.',
        'Autodesk', NULL, NULL, 'software',
        '["학생","교육자","Autodesk","1년 무료","매년 갱신","비상업용"]'::JSONB
      ),
      (
        'https://sales.jetbrains.com/hc/en-gb/articles/207241195-Do-you-offer-free-educational-licenses-for-students-and-teachers',
        'https://sales.jetbrains.com/hc/en-gb/articles/207241195-Do-you-offer-free-educational-licenses-for-students-and-teachers',
        'jetbrains',
        'JetBrains 학생·교사용 무료 라이선스',
        '인증된 학생과 교사는 JetBrains Student/Teacher Pack을 무료로 이용할 수 있습니다. IDE와 .NET 도구가 포함되며 교육·학습 목적의 비상업적 사용에 한정됩니다. 졸업 후에는 조건 충족 시 2년간 개인용 구독 40% 할인도 받을 수 있습니다.',
        'JetBrains', NULL, NULL, 'software',
        '["학생","교사","개발도구","IDE","무료","졸업 할인"]'::JSONB
      ),
      (
        'https://help.miro.com/hc/en-us/articles/360017730473-Education-plan',
        'https://help.miro.com/hc/en-us/articles/360017730473-Education-plan',
        'miro',
        'Miro Education 플랜 무료',
        '인가된 교육기관의 학생과 교직원은 Miro Education 플랜을 무료로 신청할 수 있습니다. 학생 플랜은 2년 동안 최대 10명 팀으로 제공되며 무제한 활성 보드 등 교육용 협업 기능을 이용할 수 있습니다. 기관 이메일과 재학·재직 증빙이 요구될 수 있습니다.',
        'Miro', NULL, NULL, 'software',
        '["학생","교직원","협업","화이트보드","2년","무료"]'::JSONB
      ),
      (
        'https://azure.microsoft.com/en-us/free/students/',
        'https://azure.microsoft.com/en-us/free/students/',
        'azure',
        'Azure for Students 무료 크레딧',
        '적격 전문대·대학교의 전일제 학생은 신용카드 없이 Azure for Students에 가입해 12개월 동안 미화 100달러 크레딧과 일부 무료 서비스를 이용할 수 있습니다. 학생 신분과 지역 자격을 확인하며, 자격이 유지되면 매년 갱신할 수 있습니다.',
        'Microsoft Azure', NULL, NULL, 'software',
        '["대학생","클라우드","Azure","100달러 크레딧","신용카드 불필요"]'::JSONB
      ),
      (
        'https://docs.github.com/en/copilot/how-tos/copilot-on-github/set-up-copilot/enable-copilot/set-up-for-students',
        'https://docs.github.com/en/copilot/how-tos/copilot-on-github/set-up-copilot/enable-copilot/set-up-for-students',
        'github',
        'GitHub Copilot 학생 무료 이용',
        'GitHub Education에서 학생 신분을 인증한 이용자는 Copilot Student를 무료로 이용할 수 있습니다. GitHub가 자격을 정기적으로 다시 확인하므로 학생 인증을 최신 상태로 유지해야 하며, 제공 기능과 사용 한도는 현재 Copilot Student 조건을 따릅니다.',
        'GitHub', NULL, NULL, 'software',
        '["학생","개발자","GitHub Copilot","AI 코딩","무료","학생 인증"]'::JSONB
      ),
      (
        'https://m.tworld.co.kr/product/callplan?prod_id=NA00009785',
        'https://m.tworld.co.kr/product/callplan?prod_id=NA00009785',
        'skt',
        'SKT 라이트 59 청년 추가 혜택',
        'SK텔레콤 라이트 59 요금제 이용자 중 만 34세 이하 고객은 기본 제공량 외 데이터 12GB를 추가로 받고 커피·영화·로밍 50% 할인 혜택을 이용할 수 있습니다. 커피와 영화는 각각 월 1회, 로밍은 연 2회이며 세부 제휴처와 이용 조건은 T world에서 확인해야 합니다.',
        'SK텔레콤', NULL, 34, 'telecom',
        '["청년","통신","데이터 12GB","커피 할인","영화 할인","로밍 할인"]'::JSONB
      ),
      (
        'https://product.kt.com/benefit/membership/mobile/y-bonus.html',
        'https://product.kt.com/benefit/membership/mobile/y-bonus.html',
        'kt',
        'KT Y덤 청년 데이터 혜택',
        '만 19~34세가 대상 5G 요금제를 이용하면 별도 비용 없이 Y덤이 적용되어 기본 데이터가 최대 2배로 늘고 스마트기기·테더링 공유 데이터도 추가 제공됩니다. 요금제별 제공량과 적용 시점이 다르며 만 35세가 되는 달의 다음 달부터 종료됩니다.',
        'KT', 19, 34, 'telecom',
        '["청년","통신","5G","데이터 2배","Y덤","무료"]'::JSONB
      ),
      (
        'https://www.lguplus.com/benefit-membership/rank-info#youth',
        'https://www.lguplus.com/benefit-membership/rank-info',
        'lguplus',
        'LG유플러스 유쓰 청년 멤버십 혜택',
        'LG유플러스 모바일을 이용하는 만 19~29세 고객은 유쓰 대상자로서 매월 청년 전용 제휴 혜택을 이용할 수 있습니다. 월별 혜택과 제휴처는 변경될 수 있고 멤버십 등급·요금제별 일반 혜택과 이용 횟수도 다르므로 U+ 멤버십의 현재 조건을 확인해야 합니다.',
        'LG유플러스', 19, 29, 'telecom',
        '["청년","통신","유쓰","멤버십","월간 혜택"]'::JSONB
      ),
      (
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=1000&cat_cd=&coopco_cd=7010&mcht_no=1000',
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=1000&cat_cd=&coopco_cd=7010&mcht_no=1000',
        'cgv',
        'CGV CJ ONE 포인트 적립·사용',
        'CJ ONE 회원은 CGV 영화 예매 시 결제 방식에 따라 결제금액의 3~5%, 매점 이용 시 행사 조건에서 0.5%를 포인트로 적립할 수 있습니다. 포인트는 현장 500P, 온라인 10P부터 사용할 수 있으며 일부 영화·상품·결제수단은 적립 또는 사용에서 제외됩니다.',
        'CGV·CJ ONE', NULL, NULL, 'membership',
        '["CGV","영화","CJ ONE","3~5% 적립","포인트 사용"]'::JSONB
      ),
      (
        'https://www.megabox.co.kr/benefit/membership',
        'https://www.megabox.co.kr/benefit/membership',
        'megabox',
        '메가박스 멤버십 포인트 혜택',
        '메가박스 멤버십 일반 회원은 영화 결제금액의 5%, 매점 결제금액의 2%를 포인트로 적립하고 VIP 회원은 영화 결제금액의 10%를 적립할 수 있습니다. 포인트는 영화와 매점 상품에 사용할 수 있으며 생일 쿠폰도 제공됩니다. 일부 지점·상품·결제수단은 제외됩니다.',
        '메가박스', NULL, NULL, 'membership',
        '["메가박스","영화","멤버십","포인트","생일 쿠폰"]'::JSONB
      ),
      (
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=3000&coopco_cd=7030&mcht_no=3000',
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=3000&coopco_cd=7030&mcht_no=3000',
        'oliveyoung',
        '올리브영 CJ ONE 포인트 적립·사용',
        'CJ ONE 회원은 올리브영 온·오프라인 구매 시 올리브영 회원 등급에 따라 결제금액의 0.1~1%를 포인트로 적립하고 10P 단위로 사용할 수 있습니다. 적립률과 제외 상품·결제수단은 현재 올리브영 및 CJ ONE 조건을 확인해야 합니다.',
        '올리브영·CJ ONE', NULL, NULL, 'membership',
        '["올리브영","CJ ONE","0.1~1% 적립","포인트 사용","멤버십"]'::JSONB
      ),
      (
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=3701&coopco_cd=7370&mcht_no=3701',
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=3701&coopco_cd=7370&mcht_no=3701',
        'mega_mgc',
        '메가MGC커피 CJ ONE 포인트 혜택',
        'CJ ONE 회원은 메가MGC커피 앱과 참여 매장에서 결제금액의 0.5%를 포인트로 적립할 수 있습니다. 1,000P 이상 보유 시 10P 단위로 결제에 사용할 수 있고 메가MGC커피 스탬프와 함께 적립할 수 있으나 일부 매장과 상품은 제외됩니다.',
        '메가MGC커피·CJ ONE', NULL, NULL, 'membership',
        '["메가MGC커피","CJ ONE","0.5% 적립","포인트","스탬프"]'::JSONB
      ),
      (
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=2701&coopco_cd=7270&mcht_no=2701',
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=2701&coopco_cd=7270&mcht_no=2701',
        'cu',
        'CU CJ ONE 포인트 결제',
        'CJ ONE 회원은 전국 약 1만 6천 개 CU 참여 매장에서 보유 포인트를 10P 단위로 결제에 사용할 수 있습니다. CU에서는 CJ ONE 포인트가 별도로 적립되지 않으며 1회 최대 10만P까지 사용할 수 있고 담배·상품권·서비스 상품 등 일부 품목은 제외됩니다.',
        'CU·CJ ONE', NULL, NULL, 'membership',
        '["CU","편의점","CJ ONE","포인트 결제","10P 단위"]'::JSONB
      ),
      (
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=5701&coopco_cd=7570&mcht_no=5701',
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=5701&coopco_cd=7570&mcht_no=5701',
        'emart24',
        '이마트24 CJ ONE 포인트 혜택',
        'CJ ONE 회원은 이마트24 참여 매장에서 결제금액의 0.5%를 포인트로 적립할 수 있습니다. 1,000P 이상 보유 시 10P 단위로 사용할 수 있으며 담배·주류·상품권·택배 등 일부 품목과 서비스는 적립 또는 사용에서 제외됩니다.',
        '이마트24·CJ ONE', NULL, NULL, 'membership',
        '["이마트24","편의점","CJ ONE","0.5% 적립","포인트 사용"]'::JSONB
      ),
      (
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=1900&coopco_cd=7190&mcht_no=1900',
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=1900&coopco_cd=7190&mcht_no=1900',
        'tving',
        '티빙 CJ ONE 포인트 적립·사용',
        'CJ ONE 회원은 티빙 공식 결제 채널에서 이용권을 결제할 때 결제 방식에 따라 0.3~1%를 포인트로 적립하고 10P 단위로 사용할 수 있습니다. 앱마켓 인앱 결제와 제휴사 결제 등은 제외될 수 있어 결제 전 적용 여부를 확인해야 합니다.',
        '티빙·CJ ONE', NULL, NULL, 'subscription',
        '["티빙","OTT","CJ ONE","포인트 적립","포인트 사용"]'::JSONB
      ),
      (
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=3501&coopco_cd=7350&mcht_no=3501',
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=3501&coopco_cd=7350&mcht_no=3501',
        'aqua_planet',
        '아쿠아플라넷 CJ ONE 포인트 혜택',
        'CJ ONE 회원은 아쿠아플라넷 광교·일산 현장 매표소 결제 시 결제금액의 1%를 포인트로 적립할 수 있습니다. 1,000P 이상 보유 시 10P 단위로 최대 10만P까지 사용할 수 있으며 온라인 예매와 일부 상품은 제외됩니다.',
        '아쿠아플라넷·CJ ONE', NULL, NULL, 'membership',
        '["아쿠아플라넷","CJ ONE","1% 적립","포인트 사용","광교","일산"]'::JSONB
      ),
      (
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=5601&coopco_cd=7560&mcht_no=5601',
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=5601&coopco_cd=7560&mcht_no=5601',
        'yanolja',
        'NOL CJ ONE 포인트 혜택',
        'CJ ONE 회원은 NOL 공식 온라인 채널의 대상 상품 결제 시 0.1%를 포인트로 적립할 수 있습니다. 1,000P 이상 보유 시 최대 1,000P까지 사용할 수 있으며 항공·공연 등 일부 상품과 결제 방식은 제외됩니다.',
        'NOL·CJ ONE', NULL, NULL, 'membership',
        '["NOL","여행","CJ ONE","0.1% 적립","포인트 사용"]'::JSONB
      ),
      (
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=6701&coopco_cd=7670&mcht_no=6701',
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=6701&coopco_cd=7670&mcht_no=6701',
        'photoism',
        '포토이즘 CJ ONE 포인트 혜택',
        'CJ ONE 회원은 전국 포토이즘 참여 매장에서 결제금액의 0.5%를 포인트로 적립할 수 있습니다. 1,000P 이상 보유 시 10P 단위로 최대 9,900P까지 사용할 수 있으며 팝업·대관 매장 등은 제외됩니다.',
        '포토이즘·CJ ONE', NULL, NULL, 'membership',
        '["포토이즘","셀프사진","CJ ONE","0.5% 적립","포인트 사용"]'::JSONB
      ),
      (
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=2010&coopco_cd=7020&mcht_no=2010',
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=2010&coopco_cd=7020&mcht_no=2010',
        'tous_les_jours',
        '뚜레쥬르 CJ ONE 포인트 혜택',
        'CJ ONE 회원은 뚜레쥬르 참여 매장에서 일반 결제 시 0.5%, 제휴 할인 적용 후에는 0.1%를 포인트로 적립할 수 있습니다. 보유 포인트는 10P 단위로 사용할 수 있으며 일부 매장·상품·결제수단은 제외됩니다.',
        '뚜레쥬르·CJ ONE', NULL, NULL, 'membership',
        '["뚜레쥬르","베이커리","CJ ONE","0.5% 적립","포인트 사용"]'::JSONB
      ),
      (
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=4901&coopco_cd=7490&mcht_no=4901',
        'https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=4901&coopco_cd=7490&mcht_no=4901',
        'nene_chicken',
        '네네치킨 CJ ONE 포인트 혜택',
        'CJ ONE 회원은 네네치킨 앱의 대상 주문에서 결제금액의 2%를 포인트로 적립할 수 있습니다. 1,000P 이상 보유 시 10P 단위로 사용할 수 있으며 배달비와 일부 상품·결제 방식은 적립 또는 사용에서 제외됩니다.',
        '네네치킨·CJ ONE', NULL, NULL, 'membership',
        '["네네치킨","배달","CJ ONE","2% 적립","앱 주문"]'::JSONB
      ),
      (
        'https://members.ediya.com/guide/membership',
        'https://members.ediya.com/guide/membership',
        'ediya',
        '이디야멤버스 스탬프·등급 혜택',
        '이디야멤버스 회원은 제조 음료 구매 시 스탬프를 적립하고 등급별 쿠폰 혜택을 받을 수 있습니다. White 등급은 스탬프 5개 달성 시 쿠폰팩을 1회, Blue 등급은 20개 달성 시 연간 쿠폰팩을 받으며 이후 스탬프와 리워드 혜택이 이어집니다. 스탬프 유효기간은 1년입니다.',
        '이디야커피', NULL, NULL, 'membership',
        '["이디야","멤버십","스탬프","쿠폰","등급 혜택"]'::JSONB
      ),
      (
        'https://www.gong-cha.co.kr/brand/member/membership1.php',
        'https://www.gong-cha.co.kr/brand/member/membership1.php',
        'gongcha',
        '공차 멤버십 스탬프 혜택',
        '공차 멤버십 회원은 제조 음료 1잔당 스탬프 1개를 적립하고 스탬프 10개를 모으면 무료 음료 쿠폰 1장을 받을 수 있습니다. 무료 음료 쿠폰의 유효기간은 발급일로부터 30일이며 일부 메뉴·옵션·매장에는 제한이 있습니다.',
        '공차코리아', NULL, NULL, 'membership',
        '["공차","멤버십","스탬프 10개","무료 음료","쿠폰"]'::JSONB
      ),
      (
        'https://m.hollys.co.kr/customer/faq.do#membership-crown',
        'https://m.hollys.co.kr/customer/faq.do',
        'hollys',
        '할리스 멤버십 크라운·등급 혜택',
        '할리스 멤버십은 4천원 이상 1만원 미만 결제 시 크라운 1개를 적립하고, 1만원 이상부터 5천원당 크라운을 추가 적립합니다. Silver 가입 후 누적 12개면 Gold, 48개면 Red 등급이 되며 등급에 따라 사이즈업·생일 미니케이크·자동충전 1+1 등 혜택이 제공됩니다.',
        '할리스', NULL, NULL, 'membership',
        '["할리스","멤버십","크라운","등급 혜택","생일 혜택"]'::JSONB
      ),
      (
        'https://mo.twosome.co.kr/co/faqList.do#membership-heart',
        'https://mo.twosome.co.kr/co/faqList.do',
        'twosome',
        '투썸하트 멤버십 적립·쿠폰 혜택',
        '투썸하트 회원은 3천원 이상 결제 시 기본 하트 1개를 받고, 1만원을 초과하는 결제금액에는 구간별 하트가 추가됩니다. 등급이 올라가면 아메리카노 또는 제조 음료 쿠폰 등 보상이 제공되며 Red 등급은 하트 12개 적립 시 무료 제조 음료 쿠폰을 받을 수 있습니다. 현재 앱 조건을 함께 확인해야 합니다.',
        '투썸플레이스', NULL, NULL, 'membership',
        '["투썸플레이스","투썸하트","멤버십","하트 적립","무료 음료"]'::JSONB
      ),
      (
        'https://www.happypointcard.com/page/presentation/brand.spc#paris-baguette',
        'https://www.happypointcard.com/page/presentation/brand.spc',
        'paris_baguette',
        '파리바게뜨 해피포인트 적립·사용',
        '해피포인트 모바일카드 회원은 파리바게뜨에서 1천원 이상 결제 시 결제금액의 5%를 적립할 수 있습니다. 일반 카드 회원과 할인·쿠폰 적용 거래는 적립률이 낮아질 수 있으며, 100P 이상 보유 시 10P 단위로 사용할 수 있습니다. 일부 매장·상품은 제외됩니다.',
        '파리바게뜨·해피포인트', NULL, NULL, 'membership',
        '["파리바게뜨","해피포인트","5% 적립","포인트 사용","베이커리"]'::JSONB
      ),
      (
        'https://www.happypointcard.com/page/presentation/brand.spc#baskin-robbins',
        'https://www.happypointcard.com/page/presentation/brand.spc',
        'baskin_robbins',
        '배스킨라빈스 해피포인트 적립·사용',
        '해피포인트 모바일카드 회원은 배스킨라빈스에서 1천원 이상 결제 시 결제금액의 5%를 적립할 수 있습니다. 일반 카드 회원과 할인·쿠폰 적용 거래는 적립률이 낮아질 수 있으며, 100P 이상 보유 시 10P 단위로 사용할 수 있습니다. 일부 매장·상품은 제외됩니다.',
        '배스킨라빈스·해피포인트', NULL, NULL, 'membership',
        '["배스킨라빈스","해피포인트","5% 적립","포인트 사용","아이스크림"]'::JSONB
      ),
      (
        'https://www.happypointcard.com/page/presentation/brand.spc#dunkin',
        'https://www.happypointcard.com/page/presentation/brand.spc',
        'dunkin',
        '던킨 해피포인트 적립·사용',
        '해피포인트 모바일카드 회원은 던킨에서 1천원 이상 결제 시 결제금액의 5%를 적립할 수 있습니다. 일반 카드 회원과 할인·쿠폰 적용 거래는 적립률이 낮아질 수 있으며, 100P 이상 보유 시 10P 단위로 사용할 수 있습니다. 일부 매장·상품은 제외됩니다.',
        '던킨·해피포인트', NULL, NULL, 'membership',
        '["던킨","해피포인트","5% 적립","포인트 사용","도넛"]'::JSONB
      ),
      (
        'https://www.happypointcard.com/page/presentation/brand.spc#pascucci',
        'https://www.happypointcard.com/page/presentation/brand.spc',
        'pascucci',
        '파스쿠찌 해피포인트 적립·사용',
        '해피포인트 모바일카드 회원은 파스쿠찌에서 1천원 이상 결제 시 결제금액의 5%를 적립할 수 있습니다. 일반 카드 회원과 할인·쿠폰 적용 거래는 적립률이 낮아질 수 있으며, 100P 이상 보유 시 10P 단위로 사용할 수 있습니다. 일부 매장·상품은 제외됩니다.',
        '파스쿠찌·해피포인트', NULL, NULL, 'membership',
        '["파스쿠찌","해피포인트","5% 적립","포인트 사용","카페"]'::JSONB
      ),
      (
        'https://www.happypointcard.com/page/presentation/brand.spc#shake-shack',
        'https://www.happypointcard.com/page/presentation/brand.spc',
        'shake_shack',
        '쉐이크쉑 해피포인트 적립·사용',
        '해피포인트 모바일카드 회원은 쉐이크쉑에서 1천원 이상 결제 시 결제금액의 2%를 적립할 수 있습니다. 일반 카드 회원과 할인·쿠폰 적용 거래는 적립률이 낮아질 수 있으며, 100P 이상 보유 시 10P 단위로 사용할 수 있습니다. 일부 매장·상품은 제외됩니다.',
        '쉐이크쉑·해피포인트', NULL, NULL, 'membership',
        '["쉐이크쉑","해피포인트","2% 적립","포인트 사용","외식"]'::JSONB
      )
)
INSERT INTO programs (
    program_type, source_type, source_id, title, summary,
    provider_name, official_url, program_status,
    tags, raw_payload, search_tsv, min_age, max_age, regions,
    is_active, last_synced_at, company_name, benefit_category, provider_kind
)
SELECT
    'corporate_benefit',
    'corporate_official',
    benefit.source_id,
    benefit.title,
    benefit.summary,
    benefit.provider_name,
    benefit.official_url,
    'active',
    benefit.tags,
    jsonb_build_object(
        'curation', jsonb_build_object(
            'batch', '2026-07-16-expansion-1',
            'status', 'verified',
            'reviewed_at', '2026-07-16',
            'review_method', 'manual_official_page',
            'official_url', benefit.official_url
        )
    ),
    to_tsvector(
        'simple',
        concat_ws(' ', benefit.title, benefit.summary, benefit.provider_name)
    ),
    benefit.min_age,
    benefit.max_age,
    ARRAY[]::TEXT[],
    true,
    now(),
    benefit.provider_name,
    benefit.benefit_category,
    'corporate'
FROM benefit
ON CONFLICT (source_type, source_id) DO NOTHING;

WITH link(source_id, target_key) AS (
    VALUES
      ('https://help.figma.com/hc/en-us/articles/360041061214-Figma-for-Education', 'figma'),
      ('https://www.canva.com/education/', 'canva'),
      ('https://www.autodesk.com/education/home', 'autodesk'),
      ('https://sales.jetbrains.com/hc/en-gb/articles/207241195-Do-you-offer-free-educational-licenses-for-students-and-teachers', 'jetbrains'),
      ('https://help.miro.com/hc/en-us/articles/360017730473-Education-plan', 'miro'),
      ('https://azure.microsoft.com/en-us/free/students/', 'azure'),
      ('https://docs.github.com/en/copilot/how-tos/copilot-on-github/set-up-copilot/enable-copilot/set-up-for-students', 'github'),
      ('https://m.tworld.co.kr/product/callplan?prod_id=NA00009785', 'skt'),
      ('https://product.kt.com/benefit/membership/mobile/y-bonus.html', 'kt'),
      ('https://www.lguplus.com/benefit-membership/rank-info#youth', 'lguplus'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=1000&cat_cd=&coopco_cd=7010&mcht_no=1000', 'cgv'),
      ('https://www.megabox.co.kr/benefit/membership', 'megabox'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=3000&coopco_cd=7030&mcht_no=3000', 'oliveyoung'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=3701&coopco_cd=7370&mcht_no=3701', 'mega_mgc'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=2701&coopco_cd=7270&mcht_no=2701', 'cu'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=5701&coopco_cd=7570&mcht_no=5701', 'emart24'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=1900&coopco_cd=7190&mcht_no=1900', 'tving'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=3501&coopco_cd=7350&mcht_no=3501', 'aqua_planet'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=5601&coopco_cd=7560&mcht_no=5601', 'yanolja'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=6701&coopco_cd=7670&mcht_no=6701', 'photoism'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=2010&coopco_cd=7020&mcht_no=2010', 'tous_les_jours'),
      ('https://www.cjone.com/cjmweb/point-card/brand/detail.do?brnd_cd=4901&coopco_cd=7490&mcht_no=4901', 'nene_chicken'),
      ('https://members.ediya.com/guide/membership', 'ediya'),
      ('https://www.gong-cha.co.kr/brand/member/membership1.php', 'gongcha'),
      ('https://m.hollys.co.kr/customer/faq.do#membership-crown', 'hollys'),
      ('https://mo.twosome.co.kr/co/faqList.do#membership-heart', 'twosome'),
      ('https://www.happypointcard.com/page/presentation/brand.spc#paris-baguette', 'paris_baguette'),
      ('https://www.happypointcard.com/page/presentation/brand.spc#baskin-robbins', 'baskin_robbins'),
      ('https://www.happypointcard.com/page/presentation/brand.spc#dunkin', 'dunkin'),
      ('https://www.happypointcard.com/page/presentation/brand.spc#pascucci', 'pascucci'),
      ('https://www.happypointcard.com/page/presentation/brand.spc#shake-shack', 'shake_shack')
)
INSERT INTO program_brands (
    program_id, brand_id, relationship_type, is_primary
)
SELECT
    program.id,
    target.brand_id,
    'redeemable_at',
    true
FROM link
JOIN programs AS program
  ON program.source_type = 'corporate_official'
 AND program.source_id = link.source_id
JOIN brand_discovery_targets AS target
  ON target.target_key = link.target_key
 AND target.brand_id IS NOT NULL
ON CONFLICT (program_id, brand_id, relationship_type) DO NOTHING;

-- Treat a partial catalogue as a failed deployment, not a silent success.
DO $$
DECLARE
    published_count INT;
    unlinked_count INT;
BEGIN
    SELECT count(*)
    INTO published_count
    FROM programs
    WHERE source_type = 'corporate_official'
      AND is_active = true
      AND raw_payload -> 'curation' ->> 'batch' = '2026-07-16-expansion-1';

    IF published_count <> 31 THEN
        RAISE EXCEPTION
            'Expected 31 verified expansion benefits, found %',
            published_count;
    END IF;

    SELECT count(*)
    INTO unlinked_count
    FROM programs AS program
    WHERE program.source_type = 'corporate_official'
      AND program.raw_payload -> 'curation' ->> 'batch' = '2026-07-16-expansion-1'
      AND NOT EXISTS (
          SELECT 1
          FROM program_brands AS link
          WHERE link.program_id = program.id
      );

    IF unlinked_count <> 0 THEN
        RAISE EXCEPTION
            'Verified expansion contains % unlinked benefits',
            unlinked_count;
    END IF;
END
$$;
