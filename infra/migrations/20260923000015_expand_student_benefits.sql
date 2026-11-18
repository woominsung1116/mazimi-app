-- Facts checked against the first-party URLs on 2026-09-23.
-- This registers provenance; it never grants scraping or submission authority.
DO $seed$
DECLARE r JSONB; brand UUID; v_domain UUID; program UUID; candidate UUID; host TEXT; school_name TEXT; school_aliases TEXT[];
BEGIN
FOR r IN SELECT value FROM jsonb_array_elements($data$[
  {
    "key": "termius",
    "brand": "Termius",
    "title": "Termius 학생 Pro·Team 무료",
    "summary": "유효한 GitHub Student Developer Pack 자격이 있는 학생은 Termius Pro와 Team 기능을 무료로 이용할 수 있습니다.",
    "url": "https://termius.com/education",
    "conditions": "유효한 GitHub Student Developer Pack 자격",
    "method": "공식 교육 페이지에서 학생 인증된 GitHub 계정을 연결합니다.",
    "school": null,
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "corporate_benefit",
    "source": "corporate_official",
    "provider": "corporate",
    "end": null
  },
  {
    "key": "tower",
    "brand": "Tower",
    "title": "Tower Pro 학생 12개월 무료",
    "summary": "공인 대학 재학생은 학생 인증 후 Tower Pro를 12개월 무료로 이용할 수 있습니다. 학생 자격을 다시 확인하면 연장할 수 있으며 결제 정보는 필요하지 않습니다.",
    "url": "https://www.git-tower.com/students",
    "conditions": "공인 대학 재학. 학교 이메일 또는 GitHub 학생 인증. 학교 이메일에 따라 날짜가 포함된 학생증 추가 제출",
    "method": "GitHub 학생 계정으로 로그인하거나 학교 이메일을 입력한 뒤 받은 확인 안내에 따라 라이선스를 신청합니다.",
    "school": null,
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "corporate_benefit",
    "source": "corporate_official",
    "provider": "corporate",
    "end": null
  },
  {
    "key": "axure",
    "brand": "Axure",
    "title": "Axure RP 학생 1년 무료",
    "summary": "적격 교육기관의 학위 과정 학생은 교육용 Axure RP 구독을 1년 무료로 신청하고 재학 중 갱신할 수 있습니다. 상업적 사용은 허용되지 않습니다.",
    "url": "https://www.axure.com/edu",
    "conditions": "공인 교육기관 학위 과정 재학, 교육·학습 목적, 비상업적 사용",
    "method": "공식 교육 페이지의 학생 구독 신청서를 작성합니다. 승인 후 받은 활성화 안내에 따라 이용합니다.",
    "school": null,
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "corporate_benefit",
    "source": "corporate_official",
    "provider": "corporate",
    "end": null
  },
  {
    "key": "framer",
    "brand": "Framer",
    "title": "Framer 학생 포트폴리오 Basic 무료",
    "summary": "학생은 개인 포트폴리오·수업 프로젝트·이력서 사이트용 Basic 플랜 할인 코드를 신청할 수 있습니다. 재학 중 재신청할 수 있으며 공식 FAQ상 코드 적용은 신규 고객 대상입니다.",
    "url": "https://www.framer.com/education/students/",
    "conditions": "공인 대면·온라인 교육과정 재학, 학교 이메일과 학생증, 개인 학생 사이트 용도, 코드 공유·재판매 금지",
    "method": "공식 학생 페이지에서 학교·졸업예정일과 이름·학교 로고가 보이는 학생증을 제출하고 승인된 코드를 적용합니다.",
    "school": null,
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "corporate_benefit",
    "source": "corporate_official",
    "provider": "corporate",
    "end": null
  },
  {
    "key": "sketch",
    "brand": "Sketch",
    "title": "Sketch Education 무료 워크스페이스",
    "summary": "적격 학생은 Sketch Education 워크스페이스를 무료로 신청할 수 있습니다. Mac 앱을 사용하려면 지원되는 macOS 기기가 필요하며 독학과 창업·사업체 용도는 대상이 아닙니다.",
    "url": "https://www.sketch.com/education/",
    "conditions": "만 16세 이상, 적격 교육기관 학생, 본인 워크스페이스와 학교 소속 증빙",
    "method": "Sketch 워크스페이스를 만든 뒤 공식 교육 자격 신청서를 제출합니다. 갱신은 재신청 후 심사를 받습니다.",
    "school": null,
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "corporate_benefit",
    "source": "corporate_official",
    "provider": "corporate",
    "end": null
  },
  {
    "key": "polypane",
    "brand": "Polypane",
    "title": "Polypane 학생 1년 무료",
    "summary": "GitHub Student Developer Pack 대상 학생은 웹 개발용 브라우저 Polypane을 1년 무료로 이용할 수 있습니다.",
    "url": "https://polypane.app/special-offers/",
    "conditions": "유효한 GitHub Student Developer Pack 자격",
    "method": "공식 특별 혜택 페이지의 학생 가입 링크에서 GitHub 학생 혜택을 활성화합니다.",
    "school": null,
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "corporate_benefit",
    "source": "corporate_official",
    "provider": "corporate",
    "end": null
  },
  {
    "key": "gitkraken",
    "brand": "GitKraken",
    "title": "GitKraken Student Pack 6개월",
    "summary": "GitHub 학생 인증을 완료하면 GitKraken Desktop·GitLens 등 Pro 기능의 6개월 학생 체험을 이용할 수 있습니다. 기간은 최초 계정 생성일 기준이며 이후 Pro 이용은 유료 할인 전환입니다.",
    "url": "https://help.gitkraken.com/gk-dev/gk-dev-student-pack/",
    "conditions": "GitHub 학생 인증, GitHub 방식으로 만든 GitKraken 계정, 최초 계정 생성일 기준 체험 기간",
    "method": "GitHub로 GitKraken에 로그인하고 학생 페이지에서 체험을 활성화한 뒤 구독 상태를 확인합니다.",
    "school": null,
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "corporate_benefit",
    "source": "corporate_official",
    "provider": "corporate",
    "end": null
  },
  {
    "key": "frontendmasters",
    "brand": "Frontend Masters",
    "title": "Frontend Masters 학생 강의 6개월 무료",
    "summary": "GitHub Student Developer Pack 인증 학생은 Frontend Masters 강의 라이브러리를 6개월 무료로 이용할 수 있습니다. 무료 등록에는 신용카드가 필요하지 않습니다.",
    "url": "https://frontendmasters.com/welcome/github-student-developers/",
    "conditions": "GitHub Student Developer Pack 가입, Frontend Masters 계정 및 이메일 확인",
    "method": "공식 학생 혜택 페이지에서 계정을 만들거나 로그인하고 GitHub 학생 자격으로 혜택을 활성화합니다.",
    "school": null,
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "education",
    "type": "corporate_benefit",
    "source": "corporate_official",
    "provider": "corporate",
    "end": null
  },
  {
    "key": "bootstrapstudio",
    "brand": "Bootstrap Studio",
    "title": "Bootstrap Studio 학생 무료 라이선스",
    "summary": "학생은 재학 기간 동안 사용할 수 있는 비상업적 Bootstrap Studio 라이선스를 무료로 신청할 수 있습니다.",
    "url": "https://bootstrapstudio.io/contact-us",
    "conditions": "재학 상태, 비상업적 사용, GitHub 학생 인증 또는 학교 이메일",
    "method": "공식 FAQ의 학생 포털에서 GitHub Student Developer Pack 또는 학교 이메일로 신청합니다.",
    "school": null,
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "corporate_benefit",
    "source": "corporate_official",
    "provider": "corporate",
    "end": null
  },
  {
    "key": "spotify",
    "brand": "Spotify",
    "title": "Spotify Premium 학생 요금제",
    "summary": "대상 대학 교육기관의 학생은 인증 후 Premium 학생 요금제를 이용할 수 있습니다. 첫 달 무료는 이전 Premium 체험 이력이 없는 대상자에게 적용되며 이후에는 유료로 갱신됩니다.",
    "url": "https://www.spotify.com/kr-ko/student/",
    "conditions": "승인된 대학 교육기관 재학, 학생 자격 인증, 체험 이력 및 갱신 조건 확인",
    "method": "한국 공식 학생 요금제 페이지에서 학생 자격을 확인하고 무료 기간 이후 요금과 갱신 조건을 확인합니다.",
    "school": null,
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "student_discount",
    "type": "corporate_benefit",
    "source": "corporate_official",
    "provider": "corporate",
    "end": null
  },
  {
    "key": "pknu_office365",
    "brand": "국립부경대학교",
    "title": "부경대 Office 365",
    "summary": "부경대학교 재학생과 휴학생은 학교 Office 365에 가입해 Excel·PowerPoint 등 계약 제품군을 이용할 수 있습니다. 새 가입 계정은 @office.pknu.ac.kr 형식입니다.",
    "url": "https://icc.pknu.ac.kr/main/43?action=view&no=9524",
    "conditions": "부경대 재학생·휴학생. 기존 @pknu.ac.kr·@pukyong.ac.kr 계정은 학교 안내에 따라 새 계정 확인",
    "method": "학교 AI정보혁신본부 Office365 안내의 가입·다운로드 절차에 따라 학교 계정을 발급합니다.",
    "school": "pknu",
    "statuses": [
      "enrolled",
      "재학",
      "on_leave",
      "휴학"
    ],
    "category": "software",
    "type": "campus_benefit",
    "source": "university_official",
    "provider": "university",
    "end": null
  },
  {
    "key": "tu_cloud",
    "brand": "동명대학교",
    "title": "동명대 Microsoft 365·Google 클라우드",
    "summary": "동명대학교 재학생은 학교가 제공하는 Microsoft Office 365와 Google 클라우드 서비스를 교내외에서 이용할 수 있습니다. 제공 기능은 학교 계약과 계정 정책에 따릅니다.",
    "url": "https://www.tu.ac.kr/help/sub03_03.do",
    "conditions": "동명대학교 재학생, 학교 클라우드 계정",
    "method": "IT 헬프데스크의 클라우드 서비스 안내에서 동명대학교 클라우드센터로 이동해 가입·이용합니다.",
    "school": "tu",
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "campus_benefit",
    "source": "university_official",
    "provider": "university",
    "end": null
  },
  {
    "key": "donga_cloud",
    "brand": "동아대학교",
    "title": "동아대 학생메일·Office 365",
    "summary": "동아대학교 학생은 학교 클라우드 서비스에서 학생메일과 Office 365 계정을 만들고 관리할 수 있습니다. 학부생과 대학원생의 접속 메뉴가 구분되어 있습니다.",
    "url": "https://www.donga.ac.kr/kor/CMS/Contents/Contents.do?mCode=MN196",
    "conditions": "동아대학교 학생, 학교 계정 및 Microsoft 스토리지 정책 확인. 휴학 등 학적 변경 시 이용 범위 별도 확인",
    "method": "학교 홈페이지 학생메일&오피스 365 메뉴에서 학부생 또는 대학원생 클라우드 서비스에 접속합니다.",
    "school": "donga",
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "campus_benefit",
    "source": "university_official",
    "provider": "university",
    "end": null
  },
  {
    "key": "ks_ai",
    "brand": "경성대학교",
    "title": "경성대 학내 AI Chat",
    "summary": "경성대학교는 LMS 로그인을 통해 GPT·Gemini 계열 모델을 연계한 학내 AI Chat을 제공합니다. 제공 모델과 기능은 학교 정책에 따라 변경될 수 있습니다.",
    "url": "https://kscms.ks.ac.kr/aiguide/CMS/Contents/Contents.do?mCode=MN021",
    "conditions": "경성대학교 재학생, LMS 계정. 학적에 따른 이용 제한 및 학교 생성형 AI 가이드라인 확인",
    "method": "학교 LMS에 로그인해 학내 AI Chat을 이용합니다. 개인정보·민감정보를 입력하지 않고 생성 결과를 확인합니다.",
    "school": "ks",
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "campus_benefit",
    "source": "university_official",
    "provider": "university",
    "end": null
  },
  {
    "key": "daegu_office365",
    "brand": "대구대학교",
    "title": "대구대 재학생 Office 365",
    "summary": "대구대학교 학부·대학원 재학생은 학교 계정으로 정품 Office 365를 개인 기기에 설치해 이용할 수 있습니다. 학적 변경 시 권한과 저장 공간 정책을 확인해야 합니다.",
    "url": "https://daegu.ac.kr/page/154",
    "conditions": "대구대학교 학부·대학원 재학생. 공식 이용대상 안내상 휴학·수료·졸업·자퇴·제적 제외",
    "method": "학교 Office365 홈페이지에서 계정을 만들고 학교 계정으로 로그인해 이용합니다.",
    "school": "daegu-daegu",
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "campus_benefit",
    "source": "university_official",
    "provider": "university",
    "end": null
  },
  {
    "key": "yu_office365",
    "brand": "영남대학교",
    "title": "영남대 재학생 Office 365",
    "summary": "영남대학교 재학생은 학교 Office 365를 통해 설치형 Office를 PC 5대와 스마트폰 5대에서 사용할 수 있습니다. 학교 서비스 가입 시 @ynu.kr 계정이 생성됩니다.",
    "url": "https://office.yu.ac.kr/Asp2/ServiceInfo.aspx",
    "conditions": "영남대학교 재학생, 학교 서비스 계정, 재학기간 내 이용",
    "method": "영남대학교 Office365 서비스의 가입하기 메뉴에서 계정을 만든 뒤 Microsoft 365에 로그인합니다.",
    "school": "yu-daegu",
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "campus_benefit",
    "source": "university_official",
    "provider": "university",
    "end": null
  },
  {
    "key": "dgist_matlab",
    "brand": "대구경북과학기술원",
    "title": "DGIST MATLAB 캠퍼스 라이선스",
    "summary": "DGIST 학생은 학교의 MATLAB Campus-Wide 라이선스를 장소와 설치 수 제한 없이 이용할 수 있습니다. 학교 이메일로 MathWorks 계정을 만들어야 합니다.",
    "url": "https://easyit.dgist.ac.kr/HOME/easyiten/sub.htm?nav_code=eas1670908603",
    "conditions": "DGIST 학생, 학교 이메일 인증, 학교 라이선스 및 제품 이용 조건 확인",
    "method": "DGIST 이메일로 MathWorks 계정을 만들고 이메일 확인을 마친 뒤 설치 파일을 다운로드합니다.",
    "school": "dgist",
    "statuses": [
      "enrolled",
      "재학"
    ],
    "category": "software",
    "type": "campus_benefit",
    "source": "university_official",
    "provider": "university",
    "end": null
  }
]$data$::JSONB) LOOP
  brand := NULL;
  school_name := NULL;
  school_aliases := NULL;
  IF r->>'school' IS NOT NULL THEN
    SELECT u.name,u.aliases || ARRAY[u.name,u.id] INTO STRICT school_name,school_aliases FROM universities u WHERE u.id=r->>'school' AND u.is_active;
  END IF;
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
  IF v_domain IS NULL THEN RAISE EXCEPTION 'Reviewed domain unavailable: %',host; END IF;
  INSERT INTO programs(program_type,source_type,source_id,title,summary,provider_name,company_name,official_url,
      program_status,provider_kind,benefit_category,application_method,normalized_payload,raw_payload,
      school_scope,university_id,application_end_at,regions,is_active,source_health,source_verified_at,source_review_due_at,last_synced_at)
  VALUES(r->>'type',r->>'source','verified:20260923:expansion2:'||(r->>'key'),r->>'title',r->>'summary',r->>'brand',
      CASE WHEN brand IS NOT NULL THEN r->>'brand' ELSE NULL END,r->>'url','always_open',r->>'provider',r->>'category',r->>'method',
      jsonb_build_object('conditions',r->>'conditions','enrollment_statuses',r->'statuses','audience_scope',CASE WHEN r->>'school' IS NULL THEN 'higher_education' ELSE 'university' END),
      jsonb_build_object('curation',jsonb_build_object('batch','2026-09-23-official-expansion-2','status','verified','reviewed_at','2026-09-23','official_url',r->>'url')),
      CASE WHEN r->>'school' IS NOT NULL THEN jsonb_build_array(school_name) ELSE '[]'::JSONB END,r->>'school',(r->>'end')::TIMESTAMPTZ,
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
      jsonb_build_object('all',jsonb_build_array(jsonb_build_object('field','school_name','op','in','value',to_jsonb(school_aliases),'weight',50),jsonb_build_object('field','enrollment_status','op','in','value',r->'statuses','weight',50))),
      jsonb_build_object('all',jsonb_build_array(jsonb_build_object('field','school_name','op','in','value',to_jsonb(school_aliases)),jsonb_build_object('field','enrollment_status','op','in','value',r->'statuses'))),
      jsonb_build_object('school_name',school_name || ' 구성원 대상','enrollment_status',r->>'conditions'),1);
  END IF;
END LOOP;
END $seed$;

-- A single corporate record is reused by every selected university.
UPDATE programs SET normalized_payload=COALESCE(normalized_payload,'{}'::jsonb) ||
  '{"audience_scope":"higher_education","enrollment_statuses":["enrolled","재학"]}'::jsonb,updated_at=now()
WHERE program_type='corporate_benefit' AND official_url IN (
  'https://unity.com/products/unity-student',
  'https://1password.com/developers/students',
  'https://www.lge.co.kr/benefits/exhibitions/detail-PE00518001',
  'https://www.microsoft.com/ko-kr/education/products/office',
  'https://www.notion.com/help/notion-for-education',
  'https://obsidian.md/help/discounts',
  'https://docs.github.com/en/copilot/how-tos/copilot-on-github/set-up-copilot/enable-copilot/set-up-for-students',
  'https://linear.app/docs/billing-and-plans',
  'https://www.nike.com/kr/membership/student-discount-program',
  'https://www.apple.com/kr-edu/store',
  'https://www.samsungebiz.com/event/galaxycampus/about/?anc=condition',
  'https://help.figma.com/hc/en-us/articles/360041061214-Figma-for-Education',
  'https://sales.jetbrains.com/hc/en-gb/articles/207241195-Do-you-offer-free-educational-licenses-for-students-and-teachers',
  'https://www.adobe.com/kr/education/students/creativecloud.html',
  'https://help.miro.com/hc/en-us/articles/360017730473-Education-plan',
  'https://education.github.com/pack',
  'https://azure.microsoft.com/en-us/free/students/',
  'https://www.autodesk.com/education/home'
);
