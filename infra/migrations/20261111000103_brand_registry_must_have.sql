-- Must-have corporate brands (corporate-doors review slice 0, D7):
-- the brands people in their 20s obviously use, as registry data. Names
-- come from the 2026-11-10 door census (.omc/research/corporate-doors-
-- 20261110/doors-A..F, H). AWAITING THE OWNER'S APPROVAL: the list is data
-- only and changes nothing a user sees; it is what the door job and its
-- metrics count against.
--
-- brand_discovery_targets gains:
--   - must_have: counted by the coverage metrics;
--   - aliases: the names a hub row or page may print for the brand. A row
--     binds a brand only when the full visible text of one element equals
--     an alias after NFKC, lowercasing and whitespace removal (review H2:
--     never a substring, so "이마트 에브리데이" never binds 이마트). The worker
--     test `must_have_aliases_are_unique` refuses two brands sharing one.
-- Issuers get their holder key on brands (20261111000102): SK텔레콤, KT, LG
-- 유플러스 (telecom_carrier), nine card issuers (card_issuers), four pay
-- apps (pay_apps). Brands and targets missing from the catalogue are added.
--
-- Rollback: UPDATE brand_discovery_targets SET must_have = false,
-- aliases = '{}'; UPDATE brands SET holder_field = NULL, holder_value =
-- NULL; then drop the two columns.
-- Added brands/targets carry source_type 'registry_seed' and may stay.

ALTER TABLE brand_discovery_targets
    ADD COLUMN IF NOT EXISTS must_have BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS aliases TEXT[] NOT NULL DEFAULT '{}'
        CHECK (cardinality(aliases) <= 12);

CREATE INDEX IF NOT EXISTS idx_brand_targets_must_have
    ON brand_discovery_targets (discovery_priority DESC) WHERE must_have AND is_active;

DO $must_have$
DECLARE
    r RECORD;
    target UUID;
    brand UUID;
BEGIN
    FOR r IN SELECT * FROM (VALUES
    ('starbucks', '스타벅스', '카페·디저트', ARRAY['스타벅스', 'Starbucks']::text[], NULL, NULL),
    ('mega_mgc', '메가MGC커피', '카페·디저트', ARRAY['메가MGC커피', '메가커피']::text[], NULL, NULL),
    ('twosome', '투썸플레이스', '카페·디저트', ARRAY['투썸플레이스', '투썸']::text[], NULL, NULL),
    ('ediya', '이디야커피', '카페·디저트', ARRAY['이디야커피', '이디야']::text[], NULL, NULL),
    ('paik_coffee', '빽다방', '카페·디저트', ARRAY['빽다방']::text[], NULL, NULL),
    ('compose', '컴포즈커피', '카페·디저트', ARRAY['컴포즈커피']::text[], NULL, NULL),
    ('gongcha', '공차', '카페·디저트', ARRAY['공차']::text[], NULL, NULL),
    ('hollys', '할리스', '카페·디저트', ARRAY['할리스', '할리스커피']::text[], NULL, NULL),
    ('paul_bassett', '폴바셋', '카페·디저트', ARRAY['폴바셋']::text[], NULL, NULL),
    ('baskin_robbins', '배스킨라빈스', '카페·디저트', ARRAY['배스킨라빈스']::text[], NULL, NULL),
    ('dunkin', '던킨', '카페·디저트', ARRAY['던킨', '던킨도너츠']::text[], NULL, NULL),
    ('paris_baguette', '파리바게뜨', '카페·디저트', ARRAY['파리바게뜨']::text[], NULL, NULL),
    ('tous_les_jours', '뚜레쥬르', '카페·디저트', ARRAY['뚜레쥬르']::text[], NULL, NULL),
    ('sulbing', '설빙', '카페·디저트', ARRAY['설빙']::text[], NULL, NULL),
    ('coffee_bean', '커피빈', '카페·디저트', ARRAY['커피빈']::text[], NULL, NULL),
    ('angelinus', '엔제리너스', '카페·디저트', ARRAY['엔제리너스']::text[], NULL, NULL),
    ('pascucci', '파스쿠찌', '카페·디저트', ARRAY['파스쿠찌']::text[], NULL, NULL),
    ('cu', 'CU', '편의점', ARRAY['CU']::text[], NULL, NULL),
    ('gs25', 'GS25', '편의점', ARRAY['GS25']::text[], NULL, NULL),
    ('seven_eleven', '세븐일레븐', '편의점', ARRAY['세븐일레븐']::text[], NULL, NULL),
    ('emart24', '이마트24', '편의점', ARRAY['이마트24']::text[], NULL, NULL),
    ('mcdonalds', '맥도날드', '외식·배달', ARRAY['맥도날드']::text[], NULL, NULL),
    ('burgerking', '버거킹', '외식·배달', ARRAY['버거킹']::text[], NULL, NULL),
    ('lotteria', '롯데리아', '외식·배달', ARRAY['롯데리아']::text[], NULL, NULL),
    ('momstouch', '맘스터치', '외식·배달', ARRAY['맘스터치']::text[], NULL, NULL),
    ('kfc', 'KFC', '외식·배달', ARRAY['KFC']::text[], NULL, NULL),
    ('subway', '서브웨이', '외식·배달', ARRAY['서브웨이', '써브웨이']::text[], NULL, NULL),
    ('dominos', '도미노피자', '외식·배달', ARRAY['도미노피자']::text[], NULL, NULL),
    ('pizzahut', '피자헛', '외식·배달', ARRAY['피자헛']::text[], NULL, NULL),
    ('papajohns', '파파존스', '외식·배달', ARRAY['파파존스', '파파존스피자']::text[], NULL, NULL),
    ('kyochon', '교촌치킨', '외식·배달', ARRAY['교촌치킨']::text[], NULL, NULL),
    ('bhc', 'BHC치킨', '외식·배달', ARRAY['BHC치킨', 'bhc']::text[], NULL, NULL),
    ('bbq', 'BBQ치킨', '외식·배달', ARRAY['BBQ치킨', 'BBQ']::text[], NULL, NULL),
    ('outback', '아웃백스테이크하우스', '외식·배달', ARRAY['아웃백스테이크하우스', '아웃백']::text[], NULL, NULL),
    ('vips', 'VIPS', '외식·배달', ARRAY['VIPS', '빕스']::text[], NULL, NULL),
    ('baemin', '배달의민족', '외식·배달', ARRAY['배달의민족', '배민']::text[], NULL, NULL),
    ('coupang_eats', '쿠팡이츠', '외식·배달', ARRAY['쿠팡이츠']::text[], NULL, NULL),
    ('yogiyo', '요기요', '외식·배달', ARRAY['요기요']::text[], NULL, NULL),
    ('oliveyoung', '올리브영', '뷰티·쇼핑', ARRAY['올리브영']::text[], NULL, NULL),
    ('coupang', '쿠팡', '뷰티·쇼핑', ARRAY['쿠팡']::text[], NULL, NULL),
    ('daiso', '다이소', '뷰티·쇼핑', ARRAY['다이소']::text[], NULL, NULL),
    ('musinsa', '무신사', '뷰티·쇼핑', ARRAY['무신사']::text[], NULL, NULL),
    ('kurly', '컬리', '뷰티·쇼핑', ARRAY['컬리', '마켓컬리']::text[], NULL, NULL),
    ('ably', '에이블리', '뷰티·쇼핑', ARRAY['에이블리']::text[], NULL, NULL),
    ('zigzag', '지그재그', '뷰티·쇼핑', ARRAY['지그재그']::text[], NULL, NULL),
    ('29cm', '29CM', '뷰티·쇼핑', ARRAY['29CM']::text[], NULL, NULL),
    ('wconcept', 'W컨셉', '뷰티·쇼핑', ARRAY['W컨셉']::text[], NULL, NULL),
    ('kream', 'KREAM', '뷰티·쇼핑', ARRAY['KREAM', '크림']::text[], NULL, NULL),
    ('emart', '이마트', '뷰티·쇼핑', ARRAY['이마트']::text[], NULL, NULL),
    ('ssg', 'SSG닷컴', '뷰티·쇼핑', ARRAY['SSG닷컴']::text[], NULL, NULL),
    ('homeplus', '홈플러스', '뷰티·쇼핑', ARRAY['홈플러스']::text[], NULL, NULL),
    ('lottemart', '롯데마트', '뷰티·쇼핑', ARRAY['롯데마트']::text[], NULL, NULL),
    ('naver_store', '네이버플러스스토어', '뷰티·쇼핑', ARRAY['네이버플러스스토어', '네이버플러스 스토어']::text[], NULL, NULL),
    ('elevenst', '11번가', '뷰티·쇼핑', ARRAY['11번가']::text[], NULL, NULL),
    ('gmarket', 'G마켓', '뷰티·쇼핑', ARRAY['G마켓']::text[], NULL, NULL),
    ('auction', '옥션', '뷰티·쇼핑', ARRAY['옥션']::text[], NULL, NULL),
    ('lotteon', '롯데ON', '뷰티·쇼핑', ARRAY['롯데ON', '롯데온']::text[], NULL, NULL),
    ('cj_onstyle', 'CJ온스타일', '뷰티·쇼핑', ARRAY['CJ온스타일']::text[], NULL, NULL),
    ('hyundai_department', '현대백화점', '뷰티·쇼핑', ARRAY['현대백화점']::text[], NULL, NULL),
    ('lotte_department', '롯데백화점', '뷰티·쇼핑', ARRAY['롯데백화점']::text[], NULL, NULL),
    ('shinsegae_department', '신세계백화점', '뷰티·쇼핑', ARRAY['신세계백화점']::text[], NULL, NULL),
    ('ak_plaza', 'AK플라자', '뷰티·쇼핑', ARRAY['AK플라자', 'AK PLAZA']::text[], NULL, NULL),
    ('uniqlo', '유니클로', '생활·쇼핑', ARRAY['유니클로']::text[], NULL, NULL),
    ('spao', '스파오', '생활·쇼핑', ARRAY['스파오']::text[], NULL, NULL),
    ('topten10', '탑텐', '생활·쇼핑', ARRAY['탑텐']::text[], NULL, NULL),
    ('abc_mart', 'ABC마트', '생활·쇼핑', ARRAY['ABC마트']::text[], NULL, NULL),
    ('ohouse', '오늘의집', '생활·쇼핑', ARRAY['오늘의집']::text[], NULL, NULL),
    ('muji', '무인양품', '생활·쇼핑', ARRAY['무인양품']::text[], NULL, NULL),
    ('artbox', '아트박스', '생활·쇼핑', ARRAY['아트박스']::text[], NULL, NULL),
    ('modernhouse', '모던하우스', '생활·쇼핑', ARRAY['모던하우스']::text[], NULL, NULL),
    ('jaju', '자주', '생활·쇼핑', ARRAY['자주', 'JAJU']::text[], NULL, NULL),
    ('coverage_나이키', '나이키', '생활·쇼핑', ARRAY['나이키']::text[], NULL, NULL),
    ('coverage_아디다스', '아디다스', '생활·쇼핑', ARRAY['아디다스']::text[], NULL, NULL),
    ('cgv', 'CGV', '영화·문화', ARRAY['CGV']::text[], NULL, NULL),
    ('lottecinema', '롯데시네마', '영화·문화', ARRAY['롯데시네마']::text[], NULL, NULL),
    ('megabox', '메가박스', '영화·문화', ARRAY['메가박스']::text[], NULL, NULL),
    ('yes24', '예스24', '영화·문화', ARRAY['예스24']::text[], NULL, NULL),
    ('kyobo', '교보문고', '영화·문화', ARRAY['교보문고']::text[], NULL, NULL),
    ('aladin', '알라딘', '영화·문화', ARRAY['알라딘']::text[], NULL, NULL),
    ('youngpoong', '영풍문고', '영화·문화', ARRAY['영풍문고']::text[], NULL, NULL),
    ('life4cuts', '인생네컷', '영화·문화', ARRAY['인생네컷']::text[], NULL, NULL),
    ('photoism', '포토이즘', '영화·문화', ARRAY['포토이즘']::text[], NULL, NULL),
    ('youtube_premium', '유튜브 프리미엄', '콘텐츠·구독', ARRAY['유튜브 프리미엄', 'YouTube Premium']::text[], NULL, NULL),
    ('netflix', '넷플릭스', '콘텐츠·구독', ARRAY['넷플릭스']::text[], NULL, NULL),
    ('tving', '티빙', '콘텐츠·구독', ARRAY['티빙', 'TVING']::text[], NULL, NULL),
    ('wavve', '웨이브', '콘텐츠·구독', ARRAY['웨이브', 'wavve']::text[], NULL, NULL),
    ('watcha', '왓챠', '콘텐츠·구독', ARRAY['왓챠']::text[], NULL, NULL),
    ('spotify', 'Spotify', '콘텐츠·구독', ARRAY['Spotify', '스포티파이']::text[], NULL, NULL),
    ('apple_music', 'Apple Music', '콘텐츠·구독', ARRAY['Apple Music', '애플뮤직']::text[], NULL, NULL),
    ('melon', '멜론', '콘텐츠·구독', ARRAY['멜론']::text[], NULL, NULL),
    ('genie_music', '지니뮤직', '콘텐츠·구독', ARRAY['지니뮤직']::text[], NULL, NULL),
    ('flo', 'FLO', '콘텐츠·구독', ARRAY['FLO']::text[], NULL, NULL),
    ('millie', '밀리의서재', '콘텐츠·구독', ARRAY['밀리의서재']::text[], NULL, NULL),
    ('ridi', '리디', '콘텐츠·구독', ARRAY['리디']::text[], NULL, NULL),
    ('naver_webtoon', '네이버웹툰', '콘텐츠·구독', ARRAY['네이버웹툰']::text[], NULL, NULL),
    ('skt', 'SK텔레콤', '통신·디지털', ARRAY['SK텔레콤', 'SKT', 'T멤버십', 'T 멤버십']::text[], 'telecom_carrier', 'skt'),
    ('kt', 'KT', '통신·디지털', ARRAY['KT', 'KT멤버십']::text[], 'telecom_carrier', 'kt'),
    ('lguplus', 'LG유플러스', '통신·디지털', ARRAY['LG유플러스', 'LG U+', 'U+멤버십']::text[], 'telecom_carrier', 'lgu'),
    ('kbcard', 'KB국민카드', '금융·결제', ARRAY['KB국민카드', '국민카드']::text[], 'card_issuers', 'kb'),
    ('shinhancard', '신한카드', '금융·결제', ARRAY['신한카드']::text[], 'card_issuers', 'shinhan'),
    ('samsungcard', '삼성카드', '금융·결제', ARRAY['삼성카드']::text[], 'card_issuers', 'samsung'),
    ('hyundaicard', '현대카드', '금융·결제', ARRAY['현대카드']::text[], 'card_issuers', 'hyundai'),
    ('lottecard', '롯데카드', '금융·결제', ARRAY['롯데카드']::text[], 'card_issuers', 'lotte'),
    ('wooricard', '우리카드', '금융·결제', ARRAY['우리카드']::text[], 'card_issuers', 'woori'),
    ('hanacard', '하나카드', '금융·결제', ARRAY['하나카드']::text[], 'card_issuers', 'hana'),
    ('nhcard', 'NH농협카드', '금융·결제', ARRAY['NH농협카드', '농협카드']::text[], 'card_issuers', 'nh'),
    ('bccard', 'BC카드', '금융·결제', ARRAY['BC카드', '비씨카드']::text[], 'card_issuers', 'bc'),
    ('naver_pay', '네이버페이', '금융·결제', ARRAY['네이버페이', 'N페이']::text[], 'pay_apps', 'naverpay'),
    ('kakao_pay', '카카오페이', '금융·결제', ARRAY['카카오페이']::text[], 'pay_apps', 'kakaopay'),
    ('toss', '토스', '금융·결제', ARRAY['토스', '토스페이']::text[], 'pay_apps', 'tosspay'),
    ('payco', 'PAYCO', '금융·결제', ARRAY['PAYCO', '페이코']::text[], 'pay_apps', 'payco'),
    ('toss_bank', '토스뱅크', '금융·결제', ARRAY['토스뱅크']::text[], NULL, NULL),
    ('kakao_bank', '카카오뱅크', '금융·결제', ARRAY['카카오뱅크']::text[], NULL, NULL),
    ('kbank', '케이뱅크', '금융·결제', ARRAY['케이뱅크']::text[], NULL, NULL),
    ('korail', '코레일', '교통·여행', ARRAY['코레일']::text[], NULL, NULL),
    ('srt', 'SRT', '교통·여행', ARRAY['SRT']::text[], NULL, NULL),
    ('socar', '쏘카', '교통·여행', ARRAY['쏘카']::text[], NULL, NULL),
    ('yanolja', '야놀자', '교통·여행', ARRAY['야놀자', 'NOL']::text[], NULL, NULL),
    ('yeogi', '여기어때', '교통·여행', ARRAY['여기어때']::text[], NULL, NULL),
    ('koreanair', '대한항공', '교통·여행', ARRAY['대한항공']::text[], NULL, NULL),
    ('jejuair', '제주항공', '교통·여행', ARRAY['제주항공']::text[], NULL, NULL),
    ('jinair', '진에어', '교통·여행', ARRAY['진에어']::text[], NULL, NULL),
    ('tway', '티웨이항공', '교통·여행', ARRAY['티웨이항공']::text[], NULL, NULL),
    ('kakao_t', '카카오T', '교통·여행', ARRAY['카카오T']::text[], NULL, NULL),
    ('apple', 'Apple', '전자·소프트웨어', ARRAY['Apple', '애플']::text[], NULL, NULL),
    ('samsung_electronics', '삼성전자', '전자·소프트웨어', ARRAY['삼성전자', 'SAMSUNG.COM']::text[], NULL, NULL),
    ('lg_electronics', 'LG전자', '전자·소프트웨어', ARRAY['LG전자', 'LGE.COM']::text[], NULL, NULL),
    ('adobe', 'Adobe', '전자·소프트웨어', ARRAY['Adobe', '어도비']::text[], NULL, NULL),
    ('microsoft', 'Microsoft', '전자·소프트웨어', ARRAY['Microsoft', '마이크로소프트']::text[], NULL, NULL),
    ('github', 'GitHub', '전자·소프트웨어', ARRAY['GitHub', '깃허브']::text[], NULL, NULL),
    ('notion', 'Notion', '전자·소프트웨어', ARRAY['Notion', '노션']::text[], NULL, NULL),
    ('figma', 'Figma', '전자·소프트웨어', ARRAY['Figma', '피그마']::text[], NULL, NULL),
    ('canva', 'Canva', '전자·소프트웨어', ARRAY['Canva', '캔바']::text[], NULL, NULL),
    ('miro', 'Miro', '전자·소프트웨어', ARRAY['Miro']::text[], NULL, NULL),
    ('inflearn', '인프런', '교육·커리어', ARRAY['인프런']::text[], NULL, NULL),
    ('fastcampus', '패스트캠퍼스', '교육·커리어', ARRAY['패스트캠퍼스']::text[], NULL, NULL),
    ('class101', '클래스101', '교육·커리어', ARRAY['클래스101']::text[], NULL, NULL),
    ('hackers', '해커스', '교육·커리어', ARRAY['해커스']::text[], NULL, NULL),
    ('ybm', 'YBM', '교육·커리어', ARRAY['YBM']::text[], NULL, NULL),
    ('everland', '에버랜드', '레저·테마파크', ARRAY['에버랜드']::text[], NULL, NULL),
    ('lotteworld', '롯데월드', '레저·테마파크', ARRAY['롯데월드', '롯데월드 어드벤처']::text[], NULL, NULL),
    ('cj_one', 'CJ ONE', '멤버십·포인트', ARRAY['CJ ONE']::text[], NULL, NULL),
    ('happy_point', '해피포인트', '멤버십·포인트', ARRAY['해피포인트']::text[], NULL, NULL),
    ('lpoint', 'L.POINT', '멤버십·포인트', ARRAY['L.POINT', '엘포인트']::text[], NULL, NULL),
    ('okcashbag', 'OK캐쉬백', '멤버십·포인트', ARRAY['OK캐쉬백']::text[], NULL, NULL)
    ) AS t(target_key, display_name, category, aliases, holder_field, holder_value) LOOP
        SELECT id, brand_id INTO target, brand FROM brand_discovery_targets WHERE target_key = r.target_key;
        IF brand IS NULL THEN
            SELECT id INTO brand FROM brands
            WHERE normalized_name = lower(r.display_name) AND is_active
            ORDER BY discovery_priority DESC, created_at LIMIT 1;
        END IF;
        IF brand IS NULL THEN
            INSERT INTO brands (name, normalized_name, source_type, source_id, discovery_priority)
            VALUES (r.display_name, lower(r.display_name), 'registry_seed', r.target_key, 70)
            ON CONFLICT (source_type, source_id) DO UPDATE SET updated_at = now()
            RETURNING id INTO brand;
        END IF;
        IF target IS NULL THEN
            INSERT INTO brand_discovery_targets (target_key, brand_id, display_name, coverage_category,
                                                 discovery_priority, must_have, aliases)
            VALUES (r.target_key, brand, r.display_name, r.category, 70, true, r.aliases);
        ELSE
            UPDATE brand_discovery_targets
            SET must_have = true, aliases = r.aliases, brand_id = COALESCE(brand_id, brand),
                updated_at = now()
            WHERE id = target;
        END IF;
        IF r.holder_field IS NOT NULL THEN
            -- The holder key is a fact of the company, so it goes on every
            -- active brand row of that name, not only the target's: the SKT
            -- 청년 seeds (20261107000002) pick their sponsor row by priority
            -- and age, which need not be the target's row.
            UPDATE brands SET holder_field = r.holder_field, holder_value = r.holder_value,
                              updated_at = now()
            WHERE id = brand OR (is_active AND normalized_name = lower(r.display_name));
        END IF;
    END LOOP;
END
$must_have$;
