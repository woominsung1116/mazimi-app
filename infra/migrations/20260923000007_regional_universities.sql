-- Busan municipal directory + Daegu university cooperation centre + official regional campus directories.
-- Selection coverage is distinct from the number of verified school offers.
ALTER TABLE universities ADD COLUMN region TEXT;
INSERT INTO universities(id,name,aliases,official_url,region) VALUES
('kit','경남정보대학교',ARRAY['경남정보대']::text[],'https://www.kit.ac.kr','busan'),
('ks','경성대학교',ARRAY['경성대']::text[],'https://www.ks.ac.kr','busan'),
('kosin','고신대학교',ARRAY['고신대']::text[],'https://www.kosin.ac.kr','busan'),
('pknu','국립부경대학교',ARRAY['부경대학교','부경대']::text[],'https://www.pknu.ac.kr','busan'),
('kmou','국립한국해양대학교',ARRAY['한국해양대학교','한국해양대']::text[],'https://www.kmou.ac.kr','busan'),
('daedong','대동대학교',ARRAY['대동대']::text[],'https://www.daedong.ac.kr','busan'),
('tu','동명대학교',ARRAY['동명대']::text[],'https://www.tu.ac.kr','busan'),
('dongseo','동서대학교',ARRAY['동서대']::text[],'https://www.dongseo.ac.kr','busan'),
('donga','동아대학교',ARRAY['동아대']::text[],'https://www.donga.ac.kr','busan'),
('dit','동의과학대학교',ARRAY['동의과학대']::text[],'https://www.dit.ac.kr','busan'),
('deu','동의대학교',ARRAY['동의대']::text[],'https://www.deu.ac.kr','busan'),
('cup','부산가톨릭대학교',ARRAY['부산가톨릭대']::text[],'https://www.cup.ac.kr','busan'),
('bsks','부산경상대학교',ARRAY['부산경상대']::text[],'https://www.bsks.ac.kr','busan'),
('bist','부산과학기술대학교',ARRAY['부산과기대']::text[],'https://www.bist.ac.kr','busan'),
('bnue','부산교육대학교',ARRAY['부산교대']::text[],'https://www.bnue.ac.kr','busan'),
('pusan','부산대학교',ARRAY['부산대','PNU','Pusan National University']::text[],'https://www.pusan.ac.kr','busan'),
('bdu','부산디지털대학교',ARRAY['부산디지털대']::text[],'https://www.bdu.ac.kr','busan'),
('bhu','부산보건대학교',ARRAY['동주대학교','동주대','부산보건대']::text[],'https://www.bhu.ac.kr','busan'),
('bwc','부산여자대학교',ARRAY['부산여대']::text[],'https://www.bwc.ac.kr','busan'),
('busanarts','부산예술대학교',ARRAY['부산예대']::text[],'https://www.busanarts.ac.kr','busan'),
('bufs','부산외국어대학교',ARRAY['부산외대']::text[],'https://www.bufs.ac.kr','busan'),
('silla','신라대학교',ARRAY['신라대']::text[],'https://www.silla.ac.kr','busan'),
('ysu-haeundae','영산대학교 해운대캠퍼스',ARRAY['영산대','영산대학교']::text[],'https://www.ysu.ac.kr','busan'),
('knou-busan','한국방송통신대학교 부산지역대학',ARRAY['방송대 부산','방통대 부산']::text[],'https://wbusan.knou.ac.kr','busan'),
('kopo-dongbusan','한국폴리텍대학 동부산캠퍼스',ARRAY['동부산폴리텍']::text[],'https://www.kopo.ac.kr/dongbusan/','busan'),
('kopo-busan','한국폴리텍대학 부산캠퍼스',ARRAY['부산폴리텍']::text[],'https://www.kopo.ac.kr/busan/','busan'),
('hscu','화신사이버대학교',ARRAY['화신사이버대']::text[],'https://www.hscu.ac.kr','busan'),
('knu','경북대학교',ARRAY['경북대','KNU']::text[],'https://www.knu.ac.kr','daegu'),
('kmu','계명대학교',ARRAY['계명대']::text[],'https://www.kmu.ac.kr','daegu'),
('kmcu','계명문화대학교',ARRAY['계명문화대']::text[],'https://www.kmcu.ac.kr','daegu'),
('dcu-daegu','대구가톨릭대학교 루가·유스티노캠퍼스',ARRAY['대구가톨릭대학교','대구가톨릭대']::text[],'https://www.cu.ac.kr','daegu'),
('dgist','대구경북과학기술원',ARRAY['DGIST','디지스트']::text[],'https://www.dgist.ac.kr','daegu'),
('ttc','대구공업대학교',ARRAY['대구공업대']::text[],'https://www.ttc.ac.kr','daegu'),
('tsu','대구과학대학교',ARRAY['대구과학대']::text[],'https://www.tsu.ac.kr','daegu'),
('dnue','대구교육대학교',ARRAY['대구교대']::text[],'https://www.dnue.ac.kr','daegu'),
('daegu-daegu','대구대학교 대구캠퍼스',ARRAY['대구대학교','대구대']::text[],'https://www.daegu.ac.kr','daegu'),
('dhc','대구보건대학교',ARRAY['대구보건대']::text[],'https://www.dhc.ac.kr','daegu'),
('sc','수성대학교',ARRAY['수성대']::text[],'https://www.sc.ac.kr','daegu'),
('yu-daegu','영남대학교 대구캠퍼스',ARRAY['영남대학교','영남대']::text[],'https://www.yu.ac.kr','daegu'),
('ync','영남이공대학교',ARRAY['영남이공대']::text[],'https://www.ync.ac.kr','daegu'),
('ycc','영진사이버대학교',ARRAY['영진사이버대']::text[],'https://www.ycc.ac.kr','daegu'),
('yju','영진전문대학교',ARRAY['영진전문대']::text[],'https://www.yju.ac.kr','daegu'),
('knou-daegu','한국방송통신대학교 대구경북지역대학',ARRAY['방송대 대구','방통대 대구']::text[],'https://wdaegu.knou.ac.kr','daegu'),
('kopo-namdaegu','한국폴리텍대학 남대구캠퍼스',ARRAY['남대구폴리텍']::text[],'https://www.kopo.ac.kr/namdaegu/','daegu'),
('kopo-daegu','한국폴리텍대학 대구캠퍼스',ARRAY['대구폴리텍']::text[],'https://www.kopo.ac.kr/daegu/','daegu'),
('kopo-ict','한국폴리텍대학 영남융합기술캠퍼스',ARRAY['영남융합기술']::text[],'https://www.kopo.ac.kr/ict/','daegu')
ON CONFLICT(id) DO UPDATE SET name=excluded.name, aliases=excluded.aliases, official_url=excluded.official_url, region=excluded.region;
-- Reuse the licensed search queue for schools. No crawling permission is inferred.
INSERT INTO benefit_source_domains(organization_name,domain,source_role,crawl_allowed,allow_subdomains,verification_status,verification_reference,verified_at,compliance_status,collection_mode)
SELECT name,regexp_replace(substring(official_url FROM '^https://([^/]+)'),'^(www\.)',''),'benefit_provider',false,true,'verified',official_url,now(),'pending','manual_review'
FROM universities ON CONFLICT(domain) DO NOTHING;
INSERT INTO brand_discovery_targets(target_key,display_name,coverage_category,discovery_priority,official_domain,official_homepage_url,domain_status,search_terms,policy_status,policy_notes)
SELECT 'university:'||id,name,'university',80,regexp_replace(substring(official_url FROM '^https://([^/]+)'),'^(www\.)',''),official_url,'verified',
ARRAY['학생 무료 소프트웨어','Microsoft 365','Gemini AI','도서관 전자책','학생 할인 제휴','취업 지원 무료'], 'search_only','Official school catalogue. Discovery does not grant automated page-collection rights.'
FROM universities ON CONFLICT(target_key) DO NOTHING;
