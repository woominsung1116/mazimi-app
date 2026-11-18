-- Broad, multi-channel corporate-benefit discovery.
--
-- A coverage target is not a crawl permission. It records what Mazimi wants
-- to monitor and which official/search/manual channels are available. Direct
-- fetching remains separately gated by benefit_source_domains.

ALTER TABLE benefit_source_domains
    DROP CONSTRAINT IF EXISTS benefit_source_approval_basis_check,
    DROP CONSTRAINT IF EXISTS benefit_source_crawl_requires_approval;

ALTER TABLE benefit_source_domains
    ADD COLUMN terms_review_outcome TEXT,
    ADD COLUMN collection_mode TEXT NOT NULL DEFAULT 'manual_review',
    ADD COLUMN policy_notes TEXT,
    ADD COLUMN robots_reviewed_path_prefixes TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    ADD COLUMN next_request_at TIMESTAMPTZ;

ALTER TABLE benefit_source_domains
    ADD CONSTRAINT benefit_source_approval_basis_check CHECK (
        approval_basis IS NULL OR approval_basis IN (
            'written_permission', 'terms_explicit', 'partner_contract',
            'open_license', 'public_facts_review'
        )
    ),
    ADD CONSTRAINT benefit_source_terms_outcome_check CHECK (
        terms_review_outcome IS NULL OR terms_review_outcome IN (
            'explicit_permission', 'no_automated_access_restriction',
            'restricted', 'not_found'
        )
    ),
    ADD CONSTRAINT benefit_source_collection_mode_check CHECK (
        collection_mode IN (
            'direct_public', 'official_feed', 'licensed_search',
            'manual_review', 'blocked'
        )
    );

CREATE INDEX idx_benefit_source_domain_request_slot
    ON benefit_source_domains (next_request_at)
    WHERE crawl_allowed = true AND is_active = true;

-- Older approval records marked robots as allowed without retaining a body
-- hash. They cannot prove which policy was reviewed, so fail closed and make
-- them pass the new preflight-backed approval flow again.
UPDATE benefit_source_domains
SET crawl_allowed = false,
    compliance_status = 'pending',
    robots_status = 'pending',
    collection_mode = 'manual_review',
    policy_notes = concat_ws(
        ' ',
        nullif(policy_notes, ''),
        'Direct collection disabled: a fresh robots preflight with content hash is required.'
    ),
    updated_at = now()
WHERE crawl_allowed = true
  AND (
      robots_content_hash IS NULL
      OR robots_content_hash !~ '^[0-9A-Fa-f]{32,128}$'
  );

-- Preserve older approvals that do have auditable robots evidence.
-- `collection_mode` was introduced above with a fail-closed default, so
-- existing approved rows must be mapped before the stronger constraint is
-- installed.
UPDATE benefit_source_domains
SET terms_review_outcome = 'explicit_permission',
    collection_mode = 'direct_public',
    robots_reviewed_path_prefixes = allowed_path_prefixes
WHERE crawl_allowed = true
  AND approval_basis IN (
      'written_permission', 'terms_explicit', 'partner_contract', 'open_license'
  );

ALTER TABLE benefit_source_domains
    ADD CONSTRAINT benefit_source_crawl_requires_approval CHECK (
        NOT crawl_allowed OR (
            verification_status = 'verified'
            AND verification_reference IS NOT NULL
            AND char_length(btrim(verification_reference)) > 0
            AND compliance_status = 'approved'
            AND approval_basis IS NOT NULL
            AND approval_reference IS NOT NULL
            AND char_length(btrim(approval_reference)) > 0
            AND terms_checked_at IS NOT NULL
            AND terms_review_outcome IN (
                'explicit_permission', 'no_automated_access_restriction', 'not_found'
            )
            AND robots_status = 'allowed'
            AND robots_checked_at IS NOT NULL
            AND robots_content_hash ~ '^[0-9A-Fa-f]{32,128}$'
            AND cardinality(allowed_path_prefixes) > 0
            AND robots_reviewed_path_prefixes @> allowed_path_prefixes
            AND crawler_contact_url IS NOT NULL
            AND compliance_reviewed_by IS NOT NULL
            AND compliance_reviewed_at IS NOT NULL
            AND approval_expires_at IS NOT NULL
            AND collection_mode IN ('direct_public', 'official_feed')
        )
    );

ALTER TABLE benefit_source_candidates
    DROP CONSTRAINT IF EXISTS benefit_source_candidates_discovered_via_check;

ALTER TABLE benefit_source_candidates
    ADD COLUMN candidate_kind TEXT NOT NULL DEFAULT 'unknown',
    ADD COLUMN discovery_depth INT NOT NULL DEFAULT 0,
    ADD COLUMN parent_candidate_id UUID REFERENCES benefit_source_candidates(id) ON DELETE SET NULL,
    ADD CONSTRAINT benefit_source_candidates_discovered_via_check CHECK (
        discovered_via IN (
            'search_api', 'sitemap', 'official_page', 'official_feed',
            'manual', 'user_submission', 'partner_api'
        )
    ),
    ADD CONSTRAINT benefit_source_candidate_kind_check CHECK (
        candidate_kind IN ('index', 'detail', 'unknown')
    ),
    ADD CONSTRAINT benefit_source_candidate_depth_check CHECK (
        discovery_depth BETWEEN 0 AND 3
    );

CREATE INDEX idx_benefit_candidates_parent
    ON benefit_source_candidates (parent_candidate_id)
    WHERE parent_candidate_id IS NOT NULL;

-- A corporate domain may legitimately operate several brands (for example a
-- parent-company campaign site). Keep the legacy primary brand pointer for
-- compatibility, but model full ownership as a junction instead of forcing a
-- false one-domain/one-brand invariant.
CREATE TABLE benefit_source_domain_brands (
    domain_id       UUID        NOT NULL REFERENCES benefit_source_domains(id) ON DELETE CASCADE,
    brand_id        UUID        NOT NULL REFERENCES brands(id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (domain_id, brand_id)
);

CREATE INDEX idx_benefit_source_domain_brands_brand
    ON benefit_source_domain_brands (brand_id, domain_id);

ALTER TABLE benefit_source_domain_brands ENABLE ROW LEVEL SECURITY;
ALTER TABLE benefit_source_domain_brands FORCE ROW LEVEL SECURITY;
CREATE POLICY "benefit_source_domain_brands_all_service" ON benefit_source_domain_brands
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

INSERT INTO benefit_source_domain_brands (domain_id, brand_id)
SELECT id, brand_id
FROM benefit_source_domains
WHERE brand_id IS NOT NULL
ON CONFLICT (domain_id, brand_id) DO NOTHING;

CREATE TABLE brand_discovery_targets (
    id                          UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    target_key                  TEXT        NOT NULL UNIQUE,
    brand_id                    UUID        REFERENCES brands(id) ON DELETE SET NULL,
    display_name                TEXT        NOT NULL CHECK (char_length(display_name) BETWEEN 1 AND 200),
    coverage_category           TEXT        NOT NULL,
    discovery_priority          INT         NOT NULL DEFAULT 50 CHECK (discovery_priority BETWEEN 0 AND 100),
    official_domain             TEXT,
    official_homepage_url       TEXT,
    domain_status               TEXT        NOT NULL DEFAULT 'unresolved' CHECK (
                                    domain_status IN ('unresolved', 'candidate', 'verified', 'rejected')
                                ),
    preferred_collection_modes TEXT[]      NOT NULL DEFAULT ARRAY[
                                    'official_feed', 'direct_public',
                                    'licensed_search', 'manual_review'
                                ]::TEXT[],
    seed_urls                   TEXT[]      NOT NULL DEFAULT ARRAY[]::TEXT[],
    search_terms                TEXT[]      NOT NULL DEFAULT ARRAY[
                                    '혜택', '할인', '이벤트', '멤버십', '쿠폰', '청년', '대학생'
                                ]::TEXT[],
    policy_status               TEXT        NOT NULL DEFAULT 'unreviewed' CHECK (
                                    policy_status IN (
                                        'unreviewed', 'direct_eligible', 'search_only',
                                        'manual_only', 'blocked'
                                    )
                                ),
    policy_notes                TEXT,
    last_search_at              TIMESTAMPTZ,
    next_search_at              TIMESTAMPTZ,
    last_error                  TEXT,
    is_active                   BOOLEAN     NOT NULL DEFAULT true,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (official_domain IS NULL OR char_length(official_domain) BETWEEN 3 AND 253),
    CHECK (cardinality(preferred_collection_modes) > 0),
    CHECK (preferred_collection_modes <@ ARRAY[
        'official_feed', 'direct_public', 'licensed_search', 'manual_review'
    ]::TEXT[])
);

CREATE INDEX idx_brand_discovery_targets_due
    ON brand_discovery_targets (discovery_priority DESC, next_search_at NULLS FIRST)
    WHERE is_active = true;
CREATE INDEX idx_brand_discovery_targets_category
    ON brand_discovery_targets (coverage_category, discovery_priority DESC)
    WHERE is_active = true;

CREATE TABLE brand_source_suggestions (
    id                  UUID        PRIMARY KEY DEFAULT uuid_generate_v4(),
    target_id           UUID        NOT NULL REFERENCES brand_discovery_targets(id) ON DELETE CASCADE,
    url                 TEXT        NOT NULL,
    canonical_url       TEXT        NOT NULL,
    domain              TEXT        NOT NULL,
    page_title          TEXT,
    discovered_via      TEXT        NOT NULL CHECK (
                            discovered_via IN ('naver_search', 'search_api', 'manual')
                        ),
    suggestion_kind     TEXT        NOT NULL CHECK (
                            suggestion_kind IN ('official_homepage', 'benefit_page')
                        ),
    status              TEXT        NOT NULL DEFAULT 'pending' CHECK (
                            status IN ('pending', 'approved', 'rejected')
                        ),
    first_seen_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    reviewed_at         TIMESTAMPTZ,
    reviewed_by         TEXT,
    review_note         TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (target_id, canonical_url)
);

CREATE INDEX idx_brand_source_suggestions_review
    ON brand_source_suggestions (status, last_seen_at DESC);

ALTER TABLE brand_discovery_targets ENABLE ROW LEVEL SECURITY;
ALTER TABLE brand_discovery_targets FORCE ROW LEVEL SECURITY;
CREATE POLICY "brand_discovery_targets_all_service" ON brand_discovery_targets
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

ALTER TABLE brand_source_suggestions ENABLE ROW LEVEL SECURITY;
ALTER TABLE brand_source_suggestions FORCE ROW LEVEL SECURITY;
CREATE POLICY "brand_source_suggestions_all_service" ON brand_source_suggestions
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

-- High-recall coverage catalogue. These rows express monitoring intent only;
-- they do not assert an available benefit or grant permission to fetch a site.
WITH seed(target_key, display_name, category, priority) AS (
    VALUES
      ('starbucks', '스타벅스', '카페·디저트', 100),
      ('ediya', '이디야커피', '카페·디저트', 88),
      ('twosome', '투썸플레이스', '카페·디저트', 90),
      ('mega_mgc', '메가MGC커피', '카페·디저트', 90),
      ('compose', '컴포즈커피', '카페·디저트', 87),
      ('paik_coffee', '빽다방', '카페·디저트', 85),
      ('hollys', '할리스', '카페·디저트', 82),
      ('paul_bassett', '폴바셋', '카페·디저트', 78),
      ('the_venti', '더벤티', '카페·디저트', 80),
      ('tomntoms', '탐앤탐스', '카페·디저트', 70),
      ('gongcha', '공차', '카페·디저트', 83),
      ('sulbing', '설빙', '카페·디저트', 77),
      ('baskin_robbins', '배스킨라빈스', '카페·디저트', 88),
      ('dunkin', '던킨', '카페·디저트', 80),
      ('mcdonalds', '맥도날드', '외식·배달', 93),
      ('lotteria', '롯데리아', '외식·배달', 90),
      ('burgerking', '버거킹', '외식·배달', 91),
      ('kfc', 'KFC', '외식·배달', 85),
      ('momstouch', '맘스터치', '외식·배달', 90),
      ('subway', '서브웨이', '외식·배달', 85),
      ('nobrand_burger', '노브랜드버거', '외식·배달', 76),
      ('dominos', '도미노피자', '외식·배달', 84),
      ('pizzahut', '피자헛', '외식·배달', 78),
      ('mr_pizza', '미스터피자', '외식·배달', 72),
      ('bbq', 'BBQ치킨', '외식·배달', 84),
      ('bhc', 'BHC치킨', '외식·배달', 84),
      ('kyochon', '교촌치킨', '외식·배달', 84),
      ('goobne', '굽네치킨', '외식·배달', 80),
      ('baemin', '배달의민족', '외식·배달', 95),
      ('yogiyo', '요기요', '외식·배달', 88),
      ('coupang_eats', '쿠팡이츠', '외식·배달', 90),
      ('cu', 'CU', '편의점', 96),
      ('gs25', 'GS25', '편의점', 96),
      ('seven_eleven', '세븐일레븐', '편의점', 90),
      ('emart24', '이마트24', '편의점', 88),
      ('oliveyoung', '올리브영', '뷰티·쇼핑', 100),
      ('daiso', '다이소', '뷰티·쇼핑', 94),
      ('musinsa', '무신사', '뷰티·쇼핑', 94),
      ('29cm', '29CM', '뷰티·쇼핑', 86),
      ('ably', '에이블리', '뷰티·쇼핑', 90),
      ('zigzag', '지그재그', '뷰티·쇼핑', 89),
      ('wconcept', 'W컨셉', '뷰티·쇼핑', 80),
      ('emart', '이마트', '뷰티·쇼핑', 90),
      ('lottemart', '롯데마트', '뷰티·쇼핑', 87),
      ('homeplus', '홈플러스', '뷰티·쇼핑', 87),
      ('costco', '코스트코', '뷰티·쇼핑', 80),
      ('coupang', '쿠팡', '뷰티·쇼핑', 95),
      ('naver_store', '네이버플러스스토어', '뷰티·쇼핑', 90),
      ('cgv', 'CGV', '영화·문화', 100),
      ('lottecinema', '롯데시네마', '영화·문화', 96),
      ('megabox', '메가박스', '영화·문화', 96),
      ('yes24', '예스24', '영화·문화', 85),
      ('interpark_ticket', 'NOL 티켓', '영화·문화', 85),
      ('kyobo', '교보문고', '영화·문화', 83),
      ('aladin', '알라딘', '영화·문화', 82),
      ('skt', 'SK텔레콤', '통신·디지털', 98),
      ('kt', 'KT', '통신·디지털', 98),
      ('lguplus', 'LG유플러스', '통신·디지털', 98),
      ('naver', '네이버', '통신·디지털', 90),
      ('kakao', '카카오', '통신·디지털', 90),
      ('toss', '토스', '통신·디지털', 92),
      ('kakao_pay', '카카오페이', '금융·결제', 92),
      ('naver_pay', '네이버페이', '금융·결제', 92),
      ('kbcard', 'KB국민카드', '금융·결제', 94),
      ('shinhancard', '신한카드', '금융·결제', 94),
      ('samsungcard', '삼성카드', '금융·결제', 92),
      ('hyundaicard', '현대카드', '금융·결제', 92),
      ('lottecard', '롯데카드', '금융·결제', 88),
      ('wooricard', '우리카드', '금융·결제', 88),
      ('hanacard', '하나카드', '금융·결제', 88),
      ('nhcard', 'NH농협카드', '금융·결제', 88),
      ('korail', '코레일', '교통·여행', 96),
      ('srt', 'SRT', '교통·여행', 94),
      ('tmoney', '티머니', '교통·여행', 90),
      ('kakao_t', '카카오T', '교통·여행', 88),
      ('socar', '쏘카', '교통·여행', 92),
      ('greencar', '그린카', '교통·여행', 80),
      ('yanolja', '야놀자', '교통·여행', 90),
      ('yeogi', '여기어때', '교통·여행', 90),
      ('jejuair', '제주항공', '교통·여행', 88),
      ('jinair', '진에어', '교통·여행', 84),
      ('tway', '티웨이항공', '교통·여행', 86),
      ('airbusan', '에어부산', '교통·여행', 82),
      ('koreanair', '대한항공', '교통·여행', 85),
      ('asiana', '아시아나항공', '교통·여행', 83),
      ('fastcampus', '패스트캠퍼스', '교육·커리어', 82),
      ('class101', '클래스101', '교육·커리어', 82),
      ('taling', '탈잉', '교육·커리어', 78),
      ('wanted', '원티드', '교육·커리어', 86),
      ('saramin', '사람인', '교육·커리어', 84),
      ('jobkorea', '잡코리아', '교육·커리어', 84),
      ('hackers', '해커스', '교육·커리어', 86),
      ('ybm', 'YBM', '교육·커리어', 82),
      ('eduwill', '에듀윌', '교육·커리어', 80),
      ('siwonschool', '시원스쿨', '교육·커리어', 78),
      ('inflearn', '인프런', '교육·커리어', 88),
      ('codeit', '코드잇', '교육·커리어', 82),
      ('tving', '티빙', '콘텐츠·구독', 94),
      ('wavve', '웨이브', '콘텐츠·구독', 92),
      ('watcha', '왓챠', '콘텐츠·구독', 86),
      ('netflix', '넷플릭스', '콘텐츠·구독', 88),
      ('youtube_premium', '유튜브 프리미엄', '콘텐츠·구독', 94),
      ('spotify', 'Spotify', '콘텐츠·구독', 94),
      ('apple_music', 'Apple Music', '콘텐츠·구독', 94),
      ('melon', '멜론', '콘텐츠·구독', 90),
      ('genie_music', '지니뮤직', '콘텐츠·구독', 84),
      ('flo', 'FLO', '콘텐츠·구독', 82),
      ('ridi', '리디', '콘텐츠·구독', 86),
      ('millie', '밀리의서재', '콘텐츠·구독', 88),
      ('naver_webtoon', '네이버웹툰', '콘텐츠·구독', 86),
      ('samsung_electronics', '삼성전자', '전자·소프트웨어', 94),
      ('apple', 'Apple', '전자·소프트웨어', 98),
      ('lg_electronics', 'LG전자', '전자·소프트웨어', 90),
      ('microsoft', 'Microsoft', '전자·소프트웨어', 96),
      ('adobe', 'Adobe', '전자·소프트웨어', 98),
      ('notion', 'Notion', '전자·소프트웨어', 94),
      ('github', 'GitHub', '전자·소프트웨어', 96),
      ('canva', 'Canva', '전자·소프트웨어', 90),
      ('jetbrains', 'JetBrains', '전자·소프트웨어', 92),
      ('autodesk', 'Autodesk', '전자·소프트웨어', 90),
      ('everland', '에버랜드', '레저·테마파크', 94),
      ('lotteworld', '롯데월드', '레저·테마파크', 94),
      ('seoulland', '서울랜드', '레저·테마파크', 86),
      ('eworld', '이월드', '레저·테마파크', 82),
      ('aqua_planet', '아쿠아플라넷', '레저·테마파크', 86),
      ('legoland', '레고랜드 코리아', '레저·테마파크', 84),
      ('kb_bank', 'KB국민은행', '금융·결제', 94),
      ('shinhan_bank', '신한은행', '금융·결제', 94),
      ('woori_bank', '우리은행', '금융·결제', 90),
      ('hana_bank', '하나은행', '금융·결제', 90),
      ('nh_bank', 'NH농협은행', '금융·결제', 90),
      ('ibk', 'IBK기업은행', '금융·결제', 90),
      ('kakao_bank', '카카오뱅크', '금융·결제', 94),
      ('toss_bank', '토스뱅크', '금융·결제', 94),
      ('ohouse', '오늘의집', '생활·쇼핑', 90),
      ('ikea', 'IKEA', '생활·쇼핑', 84),
      ('abc_mart', 'ABC마트', '생활·쇼핑', 84),
      ('uniqlo', '유니클로', '생활·쇼핑', 88),
      ('spao', '스파오', '생활·쇼핑', 86),
      ('topten10', '탑텐', '생활·쇼핑', 84)
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

-- Ensure every coverage target is also visible in the public brand catalogue,
-- while avoiding another row when the FTC/manual catalogue already has it.
INSERT INTO brands (
    name, normalized_name, source_type, source_id, discovery_priority
)
SELECT
    t.display_name,
    lower(regexp_replace(t.display_name, '[^0-9A-Za-z가-힣]', '', 'g')),
    'coverage_seed',
    t.target_key,
    t.discovery_priority
FROM brand_discovery_targets t
WHERE NOT EXISTS (
    SELECT 1 FROM brands b
    WHERE b.normalized_name = lower(
              regexp_replace(t.display_name, '[^0-9A-Za-z가-힣]', '', 'g')
          )
      AND b.is_active = true
)
ON CONFLICT (source_type, source_id) DO UPDATE SET
    discovery_priority = GREATEST(brands.discovery_priority, EXCLUDED.discovery_priority),
    updated_at = now();

WITH ranked_matches AS (
    SELECT
        t.id AS target_id,
        b.id AS brand_id,
        row_number() OVER (
            PARTITION BY t.id
            ORDER BY b.discovery_priority DESC, b.created_at ASC
        ) AS match_rank
    FROM brand_discovery_targets t
    JOIN brands b
      ON b.normalized_name = lower(
             regexp_replace(t.display_name, '[^0-9A-Za-z가-힣]', '', 'g')
         )
     AND b.is_active = true
    WHERE t.brand_id IS NULL
)
UPDATE brand_discovery_targets t
SET brand_id = matched.brand_id,
    updated_at = now()
FROM ranked_matches matched
WHERE t.id = matched.target_id
  AND matched.match_rank = 1;

-- Official roots verified during the 2026-07-16 source audit. Their policy
-- status deliberately remains search/manual unless direct access is separately
-- approved with current evidence.
WITH audited_source(
    target_key, official_domain, homepage_url, seed_urls, policy_notes
) AS (
    VALUES
      (
        'starbucks',
        'www.starbucks.co.kr',
        'https://www.starbucks.co.kr/',
        ARRAY[]::TEXT[],
        'robots could not be reliably verified; search/manual review only.'
      ),
      (
        'oliveyoung',
        'www.oliveyoung.co.kr',
        'https://www.oliveyoung.co.kr/store/main/main.do',
        ARRAY['https://www.oliveyoung.co.kr/store/main/getEventList.do']::TEXT[],
        'Bot challenge observed; terms restrict commercial reuse. Search/manual review only until permission.'
      ),
      (
        'cgv',
        'cgv.co.kr',
        'https://cgv.co.kr/',
        ARRAY['https://cgv.co.kr/evt/eventDetail?evntNo=1071']::TEXT[],
        'Generic-bot disallow rule observed. Search/manual review only.'
      ),
      (
        'lottecinema',
        'www.lottecinema.co.kr',
        'https://www.lottecinema.co.kr/NLCHS',
        ARRAY['https://www.lottecinema.co.kr/NLCHS/Event']::TEXT[],
        'robots allows indexing but service terms restrict commercial activity; separate legal approval required.'
      )
)
UPDATE brand_discovery_targets target
SET official_domain = source.official_domain,
    official_homepage_url = source.homepage_url,
    domain_status = 'verified',
    policy_status = 'search_only',
    preferred_collection_modes = ARRAY['licensed_search', 'manual_review']::TEXT[],
    seed_urls = source.seed_urls,
    policy_notes = source.policy_notes,
    updated_at = now()
FROM audited_source source
WHERE target.target_key = source.target_key;

UPDATE brands b
SET homepage_url = COALESCE(b.homepage_url, t.official_homepage_url),
    discovery_priority = GREATEST(b.discovery_priority, t.discovery_priority),
    updated_at = now()
FROM brand_discovery_targets t
WHERE t.brand_id = b.id
  AND t.official_homepage_url IS NOT NULL;

INSERT INTO benefit_source_domains (
    brand_id, organization_name, domain, source_role,
    crawl_allowed, verification_status, verification_reference, verified_at,
    compliance_status, collection_mode, policy_notes, is_active
)
SELECT
    t.brand_id, t.display_name, t.official_domain, 'brand_official',
    false, 'verified', t.official_homepage_url, now(),
    'pending', 'licensed_search', t.policy_notes, true
FROM brand_discovery_targets t
WHERE t.domain_status = 'verified' AND t.official_domain IS NOT NULL
ON CONFLICT (domain) DO UPDATE SET
    brand_id = COALESCE(benefit_source_domains.brand_id, EXCLUDED.brand_id),
    organization_name = EXCLUDED.organization_name,
    verification_status = 'verified',
    verification_reference = EXCLUDED.verification_reference,
    verified_at = now(),
    collection_mode = CASE
        WHEN benefit_source_domains.crawl_allowed THEN benefit_source_domains.collection_mode
        ELSE EXCLUDED.collection_mode
    END,
    policy_notes = CASE
        WHEN benefit_source_domains.crawl_allowed THEN benefit_source_domains.policy_notes
        ELSE EXCLUDED.policy_notes
    END,
    updated_at = now();

INSERT INTO benefit_source_domain_brands (domain_id, brand_id)
SELECT id, brand_id
FROM benefit_source_domains
WHERE brand_id IS NOT NULL
ON CONFLICT (domain_id, brand_id) DO NOTHING;

COMMENT ON TABLE brand_discovery_targets IS
    'High-recall monitoring universe; a target is not evidence of a benefit or crawl permission.';
COMMENT ON TABLE brand_source_suggestions IS
    'Search-discovered official-domain/benefit URL suggestions awaiting verification or factual review.';
