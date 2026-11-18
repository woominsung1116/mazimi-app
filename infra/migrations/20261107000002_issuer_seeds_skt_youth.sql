-- Issuer-paid youth offers on SK텔레콤's own page (.reports/corp-kr-routes.md
-- R1). Source: https://m.tworld.co.kr/product/callplan?prod_id=NA00009829
-- ("청년 혜택"), read 2026-11-06 as MazimiBenefitBot after robots.txt
-- (`User-agent: *`; /product/callplan is not disallowed), server-rendered.
-- Each offer was checked by hand against that page; the visible statements
-- it rests on are its evidence (issuer_seeds_20261106.json, inserted as facts
-- by the evidence job), so it is shown only once the evidence job confirms
-- them on the live page, and hidden again when the page stops saying them.
--
-- Only the youth items are taken, structured, with the link: no partner list
-- is mirrored (저작권법 §93②) and no body text is republished (T world
-- legal notice); the quotes are internal evidence. The coffee item is left
-- out: its partner brand changes monthly ("커피 제휴사는 매달 변경될 수
-- 있으며"), so no redeem brand can be stated.
--
-- Rollback: DELETE FROM programs WHERE source_id LIKE 'issuer:20261106:%'
-- (cascades to facts, monitors, holds and brand links). No user data is
-- written here; programmes users saved are kept by not running the rollback
-- once they are shown.

DO $seed$
DECLARE
    skt UUID;
    cgv UUID;
    v_domain UUID;
    candidate UUID;
    url CONSTANT TEXT := 'https://m.tworld.co.kr/product/callplan?prod_id=NA00009829';
    condition CONSTANT TEXT := '만 34세 이하 베스트/ 라이트 요금제 가입 고객님께 제공됩니다.';
    r RECORD;
BEGIN
    SELECT id INTO skt FROM brands WHERE normalized_name = 'sk텔레콤' AND is_active
        ORDER BY discovery_priority DESC, created_at LIMIT 1;
    SELECT id INTO cgv FROM brands WHERE normalized_name = 'cgv' AND is_active
        ORDER BY discovery_priority DESC, created_at LIMIT 1;
    IF skt IS NULL THEN
        INSERT INTO brands (name, normalized_name, source_type, source_id, discovery_priority)
        VALUES ('SK텔레콤', 'sk텔레콤', 'issuer_seed', 'skt', 80)
        ON CONFLICT (source_type, source_id) DO UPDATE SET updated_at = now() RETURNING id INTO skt;
    END IF;
    IF cgv IS NULL THEN
        INSERT INTO brands (name, normalized_name, source_type, source_id, discovery_priority)
        VALUES ('CGV', 'cgv', 'issuer_seed', 'cgv', 80)
        ON CONFLICT (source_type, source_id) DO UPDATE SET updated_at = now() RETURNING id INTO cgv;
    END IF;

    INSERT INTO benefit_source_domains (brand_id, organization_name, domain, source_role, crawl_allowed,
                                        verification_status, verification_reference, verified_at,
                                        compliance_status, collection_mode)
    VALUES (skt, 'SK텔레콤', 'm.tworld.co.kr', 'brand_official', false, 'verified',
            'https://www.sktelecom.com/', now(), 'pending', 'manual_review')
    ON CONFLICT (domain) DO NOTHING;
    SELECT id INTO v_domain FROM benefit_source_domains
    WHERE domain = 'm.tworld.co.kr' AND verification_status = 'verified' AND is_active;
    IF v_domain IS NULL THEN
        RAISE NOTICE 'm.tworld.co.kr is not a verified active source domain; issuer seeds skipped';
        RETURN;
    END IF;
    INSERT INTO benefit_source_domain_brands (domain_id, brand_id) VALUES (v_domain, skt)
    ON CONFLICT DO NOTHING;

    INSERT INTO benefit_source_candidates (domain_id, url, canonical_url, discovered_via, status, candidate_kind,
                                           next_check_at, recheck_interval_seconds)
    VALUES (v_domain, url, url, 'manual', 'verification_due', 'detail', now(), 86400)
    ON CONFLICT (canonical_url) DO UPDATE SET last_discovered_at = now()
    RETURNING id INTO candidate;
    INSERT INTO benefit_candidate_brands (candidate_id, brand_id) VALUES (candidate, skt)
    ON CONFLICT DO NOTHING;

    FOR r IN SELECT * FROM (VALUES
        ('7a1c2e30-6b1f-4f7e-9d61-0c3e9a5b1101'::uuid, 'cgv',
         'SKT 청년 혜택 CGV 영화 50% 할인',
         '만 34세 이하 SK텔레콤 베스트·라이트 요금제 가입 고객은 CGV 영화 관람권을 월 1장 50% 할인받을 수 있습니다. T 멤버십 앱의 청년 혜택 신청 페이지에서 예매합니다.',
         'discount', true),
        ('7a1c2e30-6b1f-4f7e-9d61-0c3e9a5b1102'::uuid, 'roaming',
         'SKT 청년 혜택 로밍 baro 요금제 50% 할인',
         '만 34세 이하 SK텔레콤 베스트·라이트 요금제 가입 고객은 baro 로밍 요금제를 연 2회 50% 할인받을 수 있습니다. 해외 이용 전 청년로밍할인 신청이 필요합니다.',
         'telecom', false)
    ) AS t(id, key, title, summary, category, redeem_cgv) LOOP
        INSERT INTO programs (id, program_type, source_type, source_id, title, summary, provider_name, company_name,
                              official_url, program_status, provider_kind, benefit_category, eligibility_condition,
                              max_age, regions, is_active, source_health, last_synced_at, raw_payload)
        VALUES (r.id, 'corporate_benefit', 'corporate_official', 'issuer:20261106:skt:' || r.key, r.title, r.summary,
                'SK텔레콤', 'SK텔레콤', url, 'review_required', 'corporate', r.category, condition,
                34, ARRAY[]::text[], false, 'review_due', now(),
                jsonb_build_object('curation', jsonb_build_object(
                    'batch', 'issuer-20261106', 'model', 'issuer_redeem', 'reviewed_at', '2026-11-06',
                    'official_url', url)))
        ON CONFLICT (source_type, source_id) DO NOTHING;
        CONTINUE WHEN NOT FOUND;
        INSERT INTO program_brands (program_id, brand_id, relationship_type, is_primary)
        VALUES (r.id, skt, 'sponsor', true) ON CONFLICT DO NOTHING;
        IF r.redeem_cgv THEN
            INSERT INTO program_brands (program_id, brand_id, relationship_type, is_primary)
            VALUES (r.id, cgv, 'redeemable_at', false) ON CONFLICT DO NOTHING;
        END IF;
        INSERT INTO program_source_monitors (program_id, candidate_id, verification_state, verified_at,
                                             review_due_at, verification_mode)
        VALUES (r.id, candidate, 'review_due', now(), now(), 'human') ON CONFLICT DO NOTHING;
        -- Shown only through the evidence job's confirmation.
        INSERT INTO program_source_holds (program_id, candidate_id, hold_reason, previous_is_active,
                                          previous_program_status)
        VALUES (r.id, candidate, 'verification_overdue', true, 'always_open')
        ON CONFLICT DO NOTHING;
    END LOOP;
END
$seed$;
