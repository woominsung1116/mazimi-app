-- First doors (data only; review slices 2-4). Every URL below was read by
-- the 2026-11-10 door census as MazimiBenefitBot after robots.txt allowed
-- the path for our token (.omc/research/corporate-doors-20261110/doors-A,
-- D, H); the saved pages are the worker's test fixtures.
--
-- Nothing here causes a request: the door job does nothing while
-- CORPORATE_DOORS_MODE is off (the default), creates no candidate for a
-- door until it runs, and never reads a hub_rows door whose domain has no
-- recorded terms_review_outcome of explicit_permission or
-- no_automated_access_restriction. No hub domain gets an outcome here: a
-- person reads the hub's terms and records it (review C3, D2). The PAYCO
-- door is read only while CORPORATE_DOOR_FLAG_PAYCO is on as well.
-- Domains are added verified (the census read them as the brand's own
-- hosts), compliance pending, crawl not approved: ELIGIBLE_DOMAIN_SQL sees
-- them only once a monitor or a branded candidate exists, and the door job
-- adds neither.
--
-- No listing door is seeded: no saved page of a real list (다이소 news,
-- 도미노 events) shows that the list reader finds articles there (code
-- review H-4). A listing door is added only with such a page as a fixture.
--
-- Rollback: DELETE FROM corporate_doors WHERE entry_url IN (the URLs below);
-- the domain rows may stay (unused without candidates).

DO $doors$
DECLARE
    r RECORD;
    brand UUID;
    v_domain UUID;
BEGIN
    FOR r IN SELECT * FROM (VALUES
        -- (target key, host, entry URL, kind, flag, max bytes, census report)
        ('skt', 'sktmembership.tworld.co.kr',
         'https://sktmembership.tworld.co.kr/mps/pc-bff/benefitbrand/list-tab1.do',
         'hub_rows', NULL, 1048576, 'doors-H'),
        ('kbcard', 'card.kbcard.com',
         'https://card.kbcard.com/BON/DVIEW/HBBMCXCRVNEC0002',
         'hub_rows', NULL, 1048576, 'doors-H'),
        ('payco', 'coupons.payco.com',
         'https://coupons.payco.com/coupon-list',
         'hub_rows', 'payco', 4194304, 'doors-H'),
        ('gongcha', 'www.gong-cha.co.kr',
         'https://www.gong-cha.co.kr/brand/store/skt_membership',
         'page', NULL, 1048576, 'doors-D'),
        ('gongcha', 'www.gong-cha.co.kr',
         'https://www.gong-cha.co.kr/brand/store/kt_membership',
         'page', NULL, 1048576, 'doors-D'),
        ('socar', 'www.socar.kr', 'https://www.socar.kr/benefit', 'page', NULL, 1048576, 'doors-F'),
        ('yes24', 'event.yes24.com', 'https://event.yes24.com/monthlygift', 'page', NULL, 1048576, 'doors-A'),
        ('kyobo', 'www.kyobobook.co.kr', 'https://www.kyobobook.co.kr/gift-promotions',
         'page', NULL, 1048576, 'doors-A')
    ) AS t(target_key, host, url, kind, flag, max_bytes, report) LOOP
        SELECT brand_id INTO brand FROM brand_discovery_targets WHERE target_key = r.target_key;
        IF brand IS NULL THEN
            RAISE NOTICE 'door % skipped: target % has no brand', r.url, r.target_key;
            CONTINUE;
        END IF;
        INSERT INTO benefit_source_domains (brand_id, organization_name, domain, source_role, crawl_allowed,
                                            verification_status, verification_reference, verified_at,
                                            compliance_status, collection_mode)
        SELECT brand, b.name, r.host,
               CASE WHEN r.kind = 'hub_rows' THEN 'benefit_provider' ELSE 'brand_official' END,
               false, 'verified', 'corporate door census 2026-11-10 (' || r.report || ')', now(),
               'pending', 'manual_review'
        FROM brands b WHERE b.id = brand
        ON CONFLICT (domain) DO NOTHING;
        SELECT id INTO v_domain FROM benefit_source_domains WHERE domain = r.host;
        INSERT INTO benefit_source_domain_brands (domain_id, brand_id) VALUES (v_domain, brand)
        ON CONFLICT DO NOTHING;
        INSERT INTO corporate_doors (domain_id, entry_url, grantor_brand_id, kind, flag, max_bytes)
        VALUES (v_domain, r.url, brand, r.kind, r.flag, r.max_bytes)
        ON CONFLICT (entry_url) DO NOTHING;
    END LOOP;
END
$doors$;
