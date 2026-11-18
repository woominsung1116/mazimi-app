-- Publish only the curated corporate benefits whose current terms were
-- re-checked on their official pages on 2026-07-16.
--
-- The previous migration intentionally inserted every row as a draft. Keep
-- this review as a separate migration so the publication decision is
-- reproducible and an already-applied migration never needs to be edited.

DO $$
DECLARE
    publishable_count INT;
BEGIN
    SELECT count(*)
    INTO publishable_count
    FROM programs
    WHERE source_type = 'corporate_official'
      AND source_id = ANY (ARRAY[
          'https://www.adobe.com/kr/education/students/creativecloud.html',
          'https://support.apple.com/ko-kr/106008',
          'https://www.apple.com/kr-edu/store',
          'https://education.github.com/pack',
          'https://www.notion.com/help/notion-for-education',
          'https://www.samsung.com/sec/galaxycampus/',
          'https://www.spotify.com/kr-ko/premium/',
          'https://www.starbucks.co.kr/msr/msreward/level_benefit.do',
          'https://www.korail.com/ticket/discountSystem/youth',
          'https://www.korail.com/ticket/discountSystem/cheerUp',
          'https://www.microsoft.com/ko-kr/education/products/office'
      ]::TEXT[]);

    IF publishable_count <> 11 THEN
        RAISE EXCEPTION
            'Expected 11 verified corporate-benefit drafts, found %',
            publishable_count;
    END IF;
END
$$;

WITH verified(
    source_id, official_url, summary, min_age, max_age, tags
) AS (
    VALUES
      (
        'https://www.adobe.com/kr/education/students/creativecloud.html',
        'https://www.adobe.com/kr/education/students/creativecloud.html',
        '13세 이상으로 적격 학교·대학 또는 홈스쿨에 등록한 학생과 교사가 Creative Cloud 20개 이상 앱을 연간 약정·월 결제로 65% 할인받습니다. 첫해 월 26,400원, 이후 월 47,300원(부가세 포함)이며 학교 이메일 또는 재학 증빙이 필요할 수 있습니다.',
        13, NULL,
        '["학생","교사","소프트웨어","65% 할인","Creative Cloud"]'::JSONB
      ),
      (
        'https://support.apple.com/ko-kr/106008',
        'https://support.apple.com/ko-kr/106008',
        '학위 수여 대학의 준학사·학사·석사 또는 이에 준하는 고등교육 과정 학생은 Apple Music 학생 구독을 할인된 월정액으로 최대 48개월 이용할 수 있습니다. 학생 신분을 1년에 한 번 다시 확인해야 하며, 학생 구독 기간에는 Apple TV+도 추가 비용 없이 이용할 수 있습니다.',
        NULL, NULL,
        '["대학생","음악","학생 할인","Apple TV+","최대 48개월"]'::JSONB
      ),
      (
        'https://www.apple.com/kr-edu/store',
        'https://www.apple.com/kr-edu/store',
        '대학 재학생·신입생·교직원 등 대상자가 Mac, iPad, Apple Watch 등 교육 대상 제품을 특별 할인가로 구매할 수 있습니다. 자격 확인과 제품별 대상·가격 확인이 필요하며, 적격 학생·교육자는 Mac 또는 iPad용 AppleCare+도 최대 10% 할인받을 수 있습니다.',
        NULL, NULL,
        '["대학생","신입생","교직원","교육 할인","AppleCare+"]'::JSONB
      ),
      (
        'https://education.github.com/pack',
        'https://education.github.com/pack',
        '학위·졸업장 수여 과정에 재학 중인 13세 이상 학생이 학교 이메일 또는 재학 서류로 인증하면 GitHub Pro를 재학 중 무료로 이용하고, Codespaces Pro와 여러 파트너의 개발 도구·학습 혜택을 이용할 수 있습니다. 개별 제휴 혜택은 수시로 변경될 수 있습니다.',
        13, NULL,
        '["학생","개발자","GitHub Pro","Codespaces","무료"]'::JSONB
      ),
      (
        'https://www.notion.com/help/notion-for-education',
        'https://www.notion.com/help/notion-for-education',
        'WHED에 등재된 대학의 학생과 교육자가 유효한 학교 이메일로 인증하면 1인용 워크스페이스의 Education Plus 플랜을 무료로 이용할 수 있습니다. 무제한 업로드, 최대 100명 게스트, 30일 페이지 기록 등이 포함되며 K-12와 개인 이메일 계정은 대상이 아닙니다.',
        NULL, NULL,
        '["대학생","교육자","Plus 플랜","무료","학교 이메일"]'::JSONB
      ),
      (
        'https://www.samsung.com/sec/galaxycampus/',
        'https://www.samsungebiz.com/event/galaxycampus/about/?anc=condition',
        '갤럭시 캠퍼스 인증 회원은 삼성전자 제품 교육할인과 회원 전용 이벤트·콘텐츠·커뮤니티를 이용할 수 있습니다. 국내외 대학생·대학원생, 국내 고등학생, 국내 대학 교직원과 초·중·고 교사가 대상이며 대상별 학교 이메일·톡학생증·본인 또는 서류 인증이 필요합니다. 실제 할인 가격은 제품과 시점에 따라 달라집니다.',
        NULL, NULL,
        '["학생","교사","교직원","갤럭시","교육 할인","인증"]'::JSONB
      ),
      (
        'https://www.spotify.com/kr-ko/premium/',
        'https://www.spotify.com/kr-ko/student/',
        '만 18세 이상으로 승인된 대학 교육기관에 재학 중인 학생은 Premium을 월 6,600원(부가세 포함)에 이용할 수 있습니다. Premium을 체험한 적 없는 이용자는 1개월 무료이며, SheerID로 학생 신분을 확인하고 12개월마다 재인증하면 최대 4년까지 할인받을 수 있습니다.',
        18, NULL,
        '["대학생","음악","구독","학생 할인","SheerID"]'::JSONB
      ),
      (
        'https://www.starbucks.co.kr/msr/msreward/level_benefit.do',
        'https://www.starbucks.co.kr/msr/msreward/level_benefit.do',
        '스타벅스 카드 등록 회원은 Welcome 등급에서 카드 결제 시 Free Extra 1개와 방문별이 적립되는 첫 구매 다음 날 아메리카노 무료 쿠폰을 받을 수 있습니다. Green 등급은 생일 무료 음료와 별 2·8개 교환 혜택, Gold 등급은 별 12·15·30개로 음료·푸드·MD 쿠폰을 교환하는 혜택이 추가됩니다. 등급 유지와 적용 제외 조건은 공식 페이지에서 확인해야 합니다.',
        NULL, NULL,
        '["스타벅스리워드","멤버십","무료음료","쿠폰","별적립"]'::JSONB
      ),
      (
        'https://www.korail.com/ticket/discountSystem/youth',
        'https://www.korail.com/ticket/discountSystem/youth',
        '코레일멤버십 회원 중 24세까지 청소년이 청소년 인증 후 KTX 운임을 열차별 승차율에 따라 10~30% 할인받습니다. 출발 1일 전까지 홈페이지나 코레일톡에서 구매해야 하며, 본인만 1일 2회·월 8회까지 이용할 수 있고 명절 특별 수송기간에는 운영하지 않습니다.',
        13, 24,
        '["청소년","KTX","교통","10~30% 할인","코레일멤버십"]'::JSONB
      ),
      (
        'https://www.korail.com/ticket/discountSystem/cheerUp',
        'https://www.korail.com/ticket/discountSystem/cheerUp',
        '코레일멤버십 회원 중 25~33세 청년이 인증 후 KTX 지정 좌석을 열차별 승차율에 따라 10~40% 할인받습니다. 출발 1일 전까지 홈페이지나 코레일톡에서 구매해야 하며, 본인만 1일 2회·월 8회까지 이용할 수 있고 명절 특별 수송기간에는 운영하지 않습니다.',
        25, 33,
        '["청년","KTX","교통","10~40% 할인","코레일멤버십"]'::JSONB
      ),
      (
        'https://www.microsoft.com/ko-kr/education/products/office',
        'https://www.microsoft.com/ko-kr/education/products/office',
        '적격 학교 이메일이 있는 학생과 교육자가 Office 365 A1을 무료로 이용할 수 있습니다. Outlook, Word, Excel, PowerPoint, OneNote의 웹 버전이 포함되며, 적격 학교에 재학·재직하는 동안 이용하고 자격은 다시 확인될 수 있습니다.',
        NULL, NULL,
        '["학생","교육자","Office 365","무료","학교 이메일"]'::JSONB
      )
)
UPDATE programs AS program
SET official_url = verified.official_url,
    summary = verified.summary,
    min_age = verified.min_age,
    max_age = verified.max_age,
    tags = verified.tags,
    program_status = 'active',
    is_active = true,
    raw_payload = COALESCE(program.raw_payload, '{}'::JSONB) ||
        jsonb_build_object(
            'curation', jsonb_build_object(
                'status', 'verified',
                'reviewed_at', '2026-07-16',
                'review_method', 'manual_official_page',
                'official_url', verified.official_url
            )
        ),
    search_tsv = to_tsvector(
        'simple',
        concat_ws(' ', program.title, verified.summary, program.provider_name)
    ),
    last_synced_at = now(),
    updated_at = now()
FROM verified
WHERE program.source_type = 'corporate_official'
  AND program.source_id = verified.source_id;

-- The Olive Young page currently requires an interactive bot challenge and
-- its current card terms could not be confirmed automatically. Preserve the
-- draft for a human browser review, but keep it outside every public query.
UPDATE programs
SET program_status = 'review_required',
    is_active = false,
    raw_payload = COALESCE(raw_payload, '{}'::JSONB) ||
        jsonb_build_object(
            'curation', jsonb_build_object(
                'status', 'verification_required',
                'reviewed_at', '2026-07-16',
                'review_method', 'manual_official_page',
                'hold_reason', 'official_page_requires_interactive_bot_challenge'
            )
        ),
    last_synced_at = now(),
    updated_at = now()
WHERE source_type = 'corporate_official'
  AND source_id = 'https://www.oliveyoung.co.kr/store/planshop/getPlanShopDetail.do?dispCatNo=500000100016770';

-- Samsung's former samsung.com entry now redirects to this separately hosted
-- official domain. Register the domain for review/search attribution only;
-- this is deliberately not permission to crawl it.
WITH selected_brand AS (
    SELECT id AS brand_id
    FROM brands
    WHERE normalized_name = '삼성전자'
      AND is_active = true
    ORDER BY discovery_priority DESC, created_at
    LIMIT 1
)
INSERT INTO benefit_source_domains (
    brand_id, organization_name, domain, source_role,
    crawl_allowed, verification_status, verification_reference, verified_at,
    compliance_status, collection_mode, policy_notes, is_active
)
SELECT
    brand_id,
    '삼성전자 갤럭시 캠퍼스',
    'www.samsungebiz.com',
    'brand_official',
    false,
    'verified',
    'https://www.samsung.com/sec/galaxycampus/',
    now(),
    'pending',
    'manual_review',
    'Official redirect verified; direct collection remains disabled pending separate compliance approval.',
    true
FROM selected_brand
ON CONFLICT (domain) DO UPDATE SET
    brand_id = COALESCE(benefit_source_domains.brand_id, EXCLUDED.brand_id),
    verification_status = 'verified',
    verification_reference = EXCLUDED.verification_reference,
    verified_at = now(),
    collection_mode = EXCLUDED.collection_mode,
    policy_notes = COALESCE(benefit_source_domains.policy_notes, EXCLUDED.policy_notes),
    updated_at = now()
WHERE benefit_source_domains.verification_status = 'pending'
  AND benefit_source_domains.crawl_allowed = false;

INSERT INTO benefit_source_domain_brands (domain_id, brand_id)
SELECT source.id, brand.id
FROM benefit_source_domains AS source
JOIN brands AS brand
  ON brand.normalized_name = '삼성전자'
 AND brand.is_active = true
WHERE source.domain = 'www.samsungebiz.com'
  AND source.verification_status = 'verified'
ON CONFLICT (domain_id, brand_id) DO NOTHING;
