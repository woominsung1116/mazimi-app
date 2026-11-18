-- Curated corporate-benefit baseline from official pages reviewed on
-- 2026-07-16. These rows are review drafts, never automatic publications.
--
-- is_active=false is an intentional fail-closed boundary. An administrator
-- must re-check the current official terms and explicitly publish each row.
-- Existing rows are preserved so a production review or publication can never
-- be undone by this baseline migration.

WITH draft(
    source_id, title, summary, provider_name, program_status,
    min_age, max_age, benefit_category, tags
) AS (
    VALUES
      (
        'https://www.adobe.com/kr/education/students/creativecloud.html',
        'Adobe Creative Cloud 학생·교사 할인',
        '13세 이상으로 적격 학교·대학 또는 홈스쿨에 등록한 학생과 교사가 Creative Cloud 20개 이상 앱을 연간 약정·월 결제로 65% 할인받습니다. 첫해 월 26,400원, 이후 월 47,300원(부가세 포함)이며 학교 이메일 또는 재학 증빙이 필요할 수 있습니다.',
        'Adobe', 'always_open', 13, NULL,
        'student_discount',
        '["학생","교사","소프트웨어","65% 할인","Creative Cloud"]'::JSONB
      ),
      (
        'https://support.apple.com/ko-kr/106008',
        'Apple Music 학생 구독 할인',
        '학위 수여 대학의 준학사·학사·석사 또는 이에 준하는 고등교육 과정 학생은 Apple Music 학생 구독을 할인된 월정액으로 최대 48개월 이용할 수 있습니다. 학생 신분을 매년 다시 확인해야 하며, 학생 구독 기간에는 Apple TV+도 추가 비용 없이 이용할 수 있습니다.',
        'Apple', 'always_open', NULL, NULL,
        'subscription',
        '["대학생","음악","학생 할인","Apple TV+","최대 48개월"]'::JSONB
      ),
      (
        'https://www.apple.com/kr-edu/store',
        'Apple 교육 할인',
        '대학 재학생·신입생·교직원 등 대상자가 Mac, iPad, Apple Watch 등 교육 대상 제품을 특별 할인가로 구매할 수 있습니다. 자격 확인과 제품별 대상·가격 확인이 필요하며, 적격 학생·교육자는 Mac 또는 iPad용 AppleCare+도 최대 10% 할인받을 수 있습니다.',
        'Apple', 'always_open', NULL, NULL,
        'student_discount',
        '["대학생","신입생","교직원","교육 할인","AppleCare+"]'::JSONB
      ),
      (
        'https://education.github.com/pack',
        'GitHub Student Developer Pack',
        '학생 인증 후 GitHub Pro를 재학 중 무료로 이용하고, Codespaces Pro 및 여러 파트너의 개발 도구·학습 혜택을 묶어서 이용할 수 있습니다. 개별 제휴 혜택과 신청 가능 여부는 수시로 달라질 수 있어 공식 팩 페이지 확인이 필요합니다.',
        'GitHub Education', 'always_open', NULL, NULL,
        'software',
        '["학생","개발자","GitHub Pro","Codespaces","무료"]'::JSONB
      ),
      (
        'https://www.notion.com/help/notion-for-education',
        'Notion 학생·교육자용 Plus 플랜 무료',
        'WHED에 등재된 대학의 학생과 교육자가 유효한 학교 이메일로 인증하면 1인용 워크스페이스의 Education Plus 플랜을 무료로 이용할 수 있습니다. 무제한 업로드, 최대 100명 게스트, 30일 페이지 기록 등이 포함되며 K-12와 개인 이메일 계정은 대상이 아닙니다.',
        'Notion', 'always_open', NULL, NULL,
        'software',
        '["대학생","교육자","Plus 플랜","무료","학교 이메일"]'::JSONB
      ),
      (
        'https://www.samsung.com/sec/galaxycampus/',
        'Samsung 갤럭시 캠퍼스 교육 할인',
        '삼성전자가 학생과 교사를 대상으로 운영하는 공식 교육 할인몰로, 인증 회원에게 갤럭시 제품 교육 가격과 회원 이벤트·전용 콘텐츠·커뮤니티 혜택을 제공합니다. 고등학생과 대학생·대학원생, 초·중·고 교사 등 대상별 인증이 필요하며 실제 할인 가격은 제품과 시점에 따라 달라집니다.',
        '삼성전자', 'always_open', NULL, NULL,
        'student_discount',
        '["학생","교사","갤럭시","교육 할인","인증"]'::JSONB
      ),
      (
        'https://www.spotify.com/kr-ko/premium/',
        'Spotify Premium 학생 요금제',
        '만 18세 이상으로 승인된 대학 교육기관에 재학 중인 학생은 Premium을 월 6,600원(부가세 포함)에 이용할 수 있습니다. 신규 Premium 이용자는 1개월 무료이며, SheerID 학생 인증을 12개월마다 갱신하면 최대 4년까지 할인받을 수 있습니다.',
        'Spotify', 'always_open', 18, NULL,
        'subscription',
        '["대학생","음악","구독","학생 할인","SheerID"]'::JSONB
      ),
      (
        'https://www.starbucks.co.kr/msr/msreward/level_benefit.do',
        '스타벅스 리워드 등급별 혜택',
        '스타벅스 카드 등록 회원은 Welcome 등급 첫 구매 후 아메리카노 Tall 무료 쿠폰과 카드 결제 시 Free Extra 1개 혜택을 받을 수 있습니다. Green·Gold 등급은 생일 무료 음료와 적립한 별을 이용한 사이즈업, 음료, 푸드 및 MD 쿠폰 교환 혜택이 제공되며 등급별 조건과 제외 품목은 공식 페이지에서 확인해야 합니다.',
        '스타벅스 코리아', 'always_open', NULL, NULL,
        'membership',
        '["스타벅스리워드","멤버십","무료음료","쿠폰","별적립"]'::JSONB
      ),
      (
        'https://www.oliveyoung.co.kr/store/planshop/getPlanShopDetail.do?dispCatNo=500000100016770',
        '올리브영 나라사랑카드 캐시백 혜택',
        '올리브영 결제 시 신한 나라사랑카드는 3천원 이상 결제하면 월 3천원 캐시백을 제공하고, 하나 나라사랑카드는 결제금액의 20%를 월 최대 5천원까지 캐시백합니다. 적용 조건과 조기 종료 여부는 공식 페이지에서 확인해야 합니다.',
        '신한카드·하나카드', 'active', NULL, NULL,
        'discount',
        '["나라사랑카드","캐시백","신한카드","하나카드","올리브영"]'::JSONB
      ),
      (
        'https://www.korail.com/ticket/discountSystem/youth',
        '코레일 청소년 드림 KTX 할인',
        '코레일멤버십 회원 중 13~24세가 청소년 인증 후 KTX 지정 좌석 운임을 열차별 승차율에 따라 10~30% 할인받습니다. 출발 1일 전까지 홈페이지나 코레일톡에서 구매해야 하며, 본인만 1일 2회·월 8회까지 이용할 수 있고 명절 특별 수송기간에는 운영하지 않습니다.',
        '한국철도공사', 'always_open', 13, 24,
        'transport',
        '["청소년","KTX","교통","10~30% 할인","코레일멤버십"]'::JSONB
      ),
      (
        'https://www.korail.com/ticket/discountSystem/cheerUp',
        '코레일 힘내라 청춘 KTX 할인',
        '코레일멤버십 회원 중 25~33세가 청년 인증 후 KTX 지정 좌석 운임을 열차별 승차율에 따라 10~40% 할인받습니다. 출발 1일 전까지 홈페이지나 코레일톡에서 구매해야 하며, 본인만 1일 2회·월 8회까지 이용할 수 있고 명절 특별 수송기간에는 운영하지 않습니다.',
        '한국철도공사', 'always_open', 25, 33,
        'transport',
        '["청년","KTX","교통","10~40% 할인","코레일멤버십"]'::JSONB
      ),
      (
        'https://www.microsoft.com/ko-kr/education/products/office',
        '학생·교육자용 Office 365 A1 무료',
        '적격 학교 이메일이 있는 학생과 교육자가 Office 365 A1을 무료로 이용할 수 있습니다. Outlook, Word, Excel, PowerPoint, OneNote의 웹 버전이 포함되며, 적격 학교에 재학·재직하는 동안 이용하고 자격은 다시 확인될 수 있습니다.',
        'Microsoft', 'always_open', NULL, NULL,
        'software',
        '["학생","교육자","Office 365","무료","학교 이메일"]'::JSONB
      )
)
INSERT INTO programs (
    program_type, source_type, source_id, title, summary,
    provider_name, official_url, program_status,
    tags, search_tsv, min_age, max_age, regions,
    is_active, last_synced_at, company_name, benefit_category, provider_kind
)
SELECT
    'corporate_benefit',
    'corporate_official',
    draft.source_id,
    draft.title,
    draft.summary,
    draft.provider_name,
    draft.source_id,
    draft.program_status,
    draft.tags,
    to_tsvector(
        'simple',
        coalesce(draft.title, '') || ' ' ||
        coalesce(draft.summary, '') || ' ' ||
        coalesce(draft.provider_name, '')
    ),
    draft.min_age,
    draft.max_age,
    ARRAY[]::TEXT[],
    false,
    now(),
    draft.provider_name,
    draft.benefit_category,
    'corporate'
FROM draft
ON CONFLICT (source_type, source_id) DO NOTHING;

WITH link(source_id, brand_normalized_name) AS (
    VALUES
      ('https://www.adobe.com/kr/education/students/creativecloud.html', 'adobe'),
      ('https://support.apple.com/ko-kr/106008', 'applemusic'),
      ('https://www.apple.com/kr-edu/store', 'apple'),
      ('https://education.github.com/pack', 'github'),
      ('https://www.notion.com/help/notion-for-education', 'notion'),
      ('https://www.samsung.com/sec/galaxycampus/', '삼성전자'),
      ('https://www.spotify.com/kr-ko/premium/', 'spotify'),
      ('https://www.starbucks.co.kr/msr/msreward/level_benefit.do', '스타벅스'),
      ('https://www.oliveyoung.co.kr/store/planshop/getPlanShopDetail.do?dispCatNo=500000100016770', '올리브영'),
      ('https://www.korail.com/ticket/discountSystem/youth', '코레일'),
      ('https://www.korail.com/ticket/discountSystem/cheerUp', '코레일'),
      ('https://www.microsoft.com/ko-kr/education/products/office', 'microsoft')
), selected AS (
    SELECT link.source_id, brand.id AS brand_id
    FROM link
    JOIN LATERAL (
        SELECT id
        FROM brands
        WHERE normalized_name = link.brand_normalized_name
          AND is_active = true
        ORDER BY discovery_priority DESC, created_at
        LIMIT 1
    ) AS brand ON true
)
INSERT INTO program_brands (
    program_id, brand_id, relationship_type, is_primary
)
SELECT
    program.id,
    selected.brand_id,
    'redeemable_at',
    true
FROM selected
JOIN programs AS program
  ON program.source_type = 'corporate_official'
 AND program.source_id = selected.source_id
ON CONFLICT (program_id, brand_id, relationship_type) DO NOTHING;

