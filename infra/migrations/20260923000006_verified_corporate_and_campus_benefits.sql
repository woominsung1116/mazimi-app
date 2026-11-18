-- Facts checked against the first-party URLs on 2026-09-23.
-- This registers provenance; it never grants scraping or submission authority.
DO $seed$
DECLARE r JSONB; brand UUID; v_domain UUID; program UUID; candidate UUID; host TEXT;
BEGIN
FOR r IN SELECT value FROM jsonb_array_elements($data$[{"key": "nike", "brand": "나이키", "title": "나이키 학생 10% 할인", "summary": "학생 자격 인증 후 대상 상품에 10% 할인을 적용할 수 있습니다. 제외 상품과 쿠폰 사용 조건이 있으며 자격 인증은 30일마다 갱신해야 합니다.", "url": "https://www.nike.com/kr/membership/student-discount-program", "category": "student_discount", "conditions": "학생 자격 인증, 대상 상품 및 제외 조건 확인", "method": "공식 페이지에서 학생 인증 후 발급된 할인 혜택을 적용합니다.", "type": "corporate_benefit", "source": "corporate_official", "provider": "corporate", "school": null, "statuses": null, "end": null}, {"key": "lg_electronics", "brand": "LG전자", "title": "LG전자 아카데미 스토어", "summary": "대학생·대학원생은 학교 이메일 인증 후 아카데미 스토어의 PC·모니터·프로젝터·오디오 대상 제품 할인 혜택을 확인할 수 있습니다. 상품별 가격과 행사 조건은 구매 시점에 달라질 수 있습니다.", "url": "https://www.lge.co.kr/benefits/exhibitions/detail-PE00518001", "category": "student_discount", "conditions": "대학생·대학원생 및 학교 이메일 인증, 대상 제품 확인", "method": "LG전자 공식 아카데미 스토어 안내에 따라 학교 이메일을 인증합니다.", "type": "corporate_benefit", "source": "corporate_official", "provider": "corporate", "school": null, "statuses": null, "end": null}, {"key": "ikea", "brand": "IKEA", "title": "IKEA Family 멤버십 혜택", "summary": "IKEA Family 회원은 멤버 특별가와 매장별 회원 혜택을 이용할 수 있습니다. 평일 따뜻한 커피·차 제공 등은 참여 매장과 공식 이용 조건을 확인해야 합니다.", "url": "https://www.ikea.com/kr/ko/ikea-family/benefits/", "category": "membership", "conditions": "IKEA Family 회원, 매장·품목·이용일 조건 확인", "method": "IKEA Family 가입 후 매장에서 멤버십을 제시하거나 온라인 계정을 사용합니다.", "type": "corporate_benefit", "source": "corporate_official", "provider": "corporate", "school": null, "statuses": null, "end": null}, {"key": "unity", "brand": "Unity", "title": "Unity Student 무료 플랜", "summary": "만 16세 이상 적격 교육기관 재학생은 학생 인증 후 Unity Student 플랜을 무료로 이용할 수 있습니다. 학위·자격 과정 재학 조건과 학생 자격 유지 여부를 확인해야 합니다.", "url": "https://unity.com/products/unity-student", "category": "software", "conditions": "만 16세 이상, 적격 교육기관 재학 및 SheerID 인증", "method": "공식 Unity Student 페이지에서 학생 자격을 인증합니다.", "type": "corporate_benefit", "source": "corporate_official", "provider": "corporate", "school": null, "statuses": null, "end": null}, {"key": "linear", "brand": "Linear", "title": "Linear 학생 단체 무료 1년", "summary": "대학교 전일제 학생의 비상업적 학생 단체는 조건을 충족하면 Basic 또는 Business 플랜을 1년간 무료로 이용할 수 있습니다. 개인의 상업 프로젝트와 일반 사업체는 대상이 아닙니다.", "url": "https://linear.app/docs/billing-and-plans", "category": "software", "conditions": "전일제 대학생 단체, 비상업적 사용, 학교 소속 확인 자료", "method": "공식 청구·요금 안내의 교육 할인 신청 절차에 따라 신청합니다.", "type": "corporate_benefit", "source": "corporate_official", "provider": "corporate", "school": null, "statuses": null, "end": null}, {"key": "obsidian", "brand": "Obsidian", "title": "Obsidian 교육 할인", "summary": "인증 대상 학생과 교직원은 Obsidian Sync 및 Publish 교육 할인을 신청할 수 있습니다. 할인율은 40%이며 상업용 라이선스 할인에는 적용되지 않습니다.", "url": "https://obsidian.md/help/discounts", "category": "software", "conditions": "학생 또는 교직원 인증, Sync·Publish 대상", "method": "공식 교육 할인 안내에 따라 학교 소속을 확인합니다.", "type": "corporate_benefit", "source": "corporate_official", "provider": "corporate", "school": null, "statuses": null, "end": null}, {"key": "1password", "brand": "1Password", "title": "1Password 학생 개인 플랜 1년", "summary": "GitHub 학생 자격 인증을 완료한 고등교육기관 학생은 1Password 개인 플랜 1년 혜택을 이용할 수 있습니다. 1회성 혜택이며 가족 플랜은 대상이 아닙니다.", "url": "https://1password.com/developers/students", "category": "software", "conditions": "GitHub 학생 자격 인증, 고등교육기관 학생, 개인 플랜·1회 한정", "method": "공식 학생 혜택 페이지에서 GitHub 학생 인증 계정으로 이용 자격을 확인합니다.", "type": "corporate_benefit", "source": "corporate_official", "provider": "corporate", "school": null, "statuses": null, "end": null}, {"key": "pnu_google_ai", "brand": "부산대학교", "title": "부산대 Google AI Pro for Education", "summary": "부산대학교는 학교 Google 계정을 통해 Gemini·NotebookLM 등 교육용 AI와 Google Workspace for Education Plus를 제공합니다. 현재 공지상 2027년 6월 23일까지 제공 예정이며 휴학생·졸업생은 제외됩니다.", "url": "https://www.pusan.ac.kr/kor/CMS/Board/PopupBoard.do?board_seq=1509799&mgr_seq=3&mode=view", "category": "software", "conditions": "부산대 학부·대학원 재학생, 등록된 수료 후 연구생 등. 휴학생·졸업생 제외. 학교 @pusan.ac.kr 계정 필요.", "method": "재학생은 기존 학교 Gmail 계정으로 로그인하여 이용합니다. 계정이 없으면 학교 웹메일 서비스에서 발급합니다.", "type": "campus_benefit", "source": "university_official", "provider": "university", "school": "pusan", "statuses": ["enrolled", "재학", "research_student", "수료후연구생"], "end": "2027-06-23T23:59:59+09:00"}, {"key": "pnu_sanjini_ai", "brand": "부산대학교", "title": "부산대 산지니 AI", "summary": "부산대 교직원과 재학·휴학 중인 학생은 산지니 AI에서 여러 생성형 AI 모델과 교내 안내 기능을 이용할 수 있습니다. 모델 구성과 제공 기능은 학교 운영 정책에 따라 달라질 수 있습니다.", "url": "https://stat.pusan.ac.kr/bbs/stat/502/968595/artclView.do", "category": "software", "conditions": "부산대학교 재학·휴학 중인 학생. 학교 계정 로그인 필요.", "method": "공식 안내의 산지니 AI 이용 매뉴얼에 따라 학교 계정으로 접속합니다.", "type": "campus_benefit", "source": "university_official", "provider": "university", "school": "pusan", "statuses": ["enrolled", "재학", "on_leave", "휴학"], "end": null}, {"key": "pnu_microsoft365", "brand": "부산대학교", "title": "부산대 Microsoft 365", "summary": "부산대학교 재적생은 학교 계정을 통해 Microsoft 365를 이용할 수 있습니다. 현재 공식 안내상 OneDrive는 5GB이며, 졸업·일반 수료·제적자는 대상에서 제외됩니다.", "url": "https://onestop.pusan.ac.kr/page?menuCD=000000000000385&mode=DETAIL&seq=243", "category": "software", "conditions": "부산대 재적생(재학·휴학·수료 후 연구생 등). @pusan.ac.kr 계정 필요. @pnu.kr·@pnu.edu 계정 사용 불가.", "method": "학생지원시스템 로그인 → Quick Menu → Microsoft365에서 가입·이용합니다.", "type": "campus_benefit", "source": "university_official", "provider": "university", "school": "pusan", "statuses": ["enrolled", "재학", "on_leave", "휴학", "research_student", "수료후연구생"], "end": null}]$data$::JSONB) LOOP
  brand := NULL;
  IF r->>'provider'='corporate' THEN
    SELECT id INTO brand FROM brands WHERE normalized_name=lower(regexp_replace(r->>'brand','[^0-9A-Za-z가-힣]','','g')) ORDER BY discovery_priority DESC LIMIT 1;
    IF brand IS NULL THEN
      INSERT INTO brands(name,normalized_name,source_type,source_id,discovery_priority)
      VALUES(r->>'brand',lower(regexp_replace(r->>'brand','[^0-9A-Za-z가-힣]','','g')),'coverage_seed',r->>'key',85)
      ON CONFLICT(source_type,source_id) DO UPDATE SET updated_at=now() RETURNING id INTO brand;
    END IF;
  END IF;
  host := lower(substring(r->>'url' FROM '^https://([^/?#]+)'));
  INSERT INTO benefit_source_domains(brand_id,organization_name,domain,source_role,crawl_allowed,verification_status,verification_reference,verified_at,compliance_status,collection_mode)
  VALUES(brand,r->>'brand',host,'benefit_provider',false,'verified',r->>'url',now(),'pending','manual_review')
  ON CONFLICT(domain) DO NOTHING;
  SELECT id INTO v_domain FROM benefit_source_domains WHERE domain=host AND verification_status='verified' AND is_active;
  IF v_domain IS NULL THEN CONTINUE; END IF;
  INSERT INTO programs(program_type,source_type,source_id,title,summary,provider_name,company_name,official_url,
      program_status,provider_kind,benefit_category,application_method,normalized_payload,raw_payload,
      school_scope,university_id,application_end_at,regions,is_active,source_health,source_verified_at,source_review_due_at,last_synced_at)
  VALUES(r->>'type',r->>'source','verified:20260923:'||(r->>'key'),r->>'title',r->>'summary',r->>'brand',
      CASE WHEN brand IS NOT NULL THEN r->>'brand' ELSE NULL END,r->>'url','always_open',r->>'provider',r->>'category',r->>'method',
      jsonb_build_object('conditions',r->>'conditions','enrollment_statuses',r->'statuses'),
      jsonb_build_object('curation',jsonb_build_object('batch','2026-09-23-official-expansion','status','verified','reviewed_at','2026-09-23','official_url',r->>'url')),
      CASE WHEN r->>'school' IS NOT NULL THEN '["부산대학교"]'::JSONB ELSE '[]'::JSONB END,r->>'school',(r->>'end')::TIMESTAMPTZ,
      ARRAY[]::TEXT[],true,'current',now(),now()+interval '30 days',now())
  ON CONFLICT(source_type,source_id) DO NOTHING RETURNING id INTO program;
  IF program IS NULL THEN CONTINUE; END IF;
  IF brand IS NOT NULL THEN
    INSERT INTO program_brands(program_id,brand_id,is_primary) VALUES(program,brand,true) ON CONFLICT DO NOTHING;
    INSERT INTO benefit_source_domain_brands(domain_id,brand_id) VALUES(v_domain,brand) ON CONFLICT DO NOTHING;
  END IF;
  INSERT INTO benefit_source_candidates(domain_id,url,canonical_url,discovered_via,status,candidate_kind,next_check_at,recheck_interval_seconds)
  VALUES(v_domain,r->>'url',r->>'url','manual','approved','detail',now()+interval '30 days',2592000)
  ON CONFLICT(canonical_url) DO UPDATE SET last_discovered_at=now() RETURNING id INTO candidate;
  INSERT INTO program_source_monitors(program_id,candidate_id,verified_at,review_due_at,recheck_interval_seconds)
  VALUES(program,candidate,now(),now()+interval '30 days',2592000) ON CONFLICT DO NOTHING;
  IF r->>'school' IS NOT NULL THEN
    INSERT INTO eligibility_rules(program_id,rule_json,hard_filter_json,explain_json,version)
    VALUES(program,
      jsonb_build_object('all',jsonb_build_array(jsonb_build_object('field','school_name','op','in','value',jsonb_build_array('부산대학교','부산대','PNU'),'weight',50),jsonb_build_object('field','enrollment_status','op','in','value',r->'statuses','weight',50))),
      jsonb_build_object('all',jsonb_build_array(jsonb_build_object('field','school_name','op','in','value',jsonb_build_array('부산대학교','부산대','PNU')),jsonb_build_object('field','enrollment_status','op','in','value',r->'statuses'))),
      jsonb_build_object('school_name','부산대학교 구성원 대상','enrollment_status',r->>'conditions'),1);
  END IF;
END LOOP;
END $seed$;
