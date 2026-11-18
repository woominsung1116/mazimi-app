-- Catalogue gaps from the 2026-10-12 recommendation audit, checked against the
-- operators' own pages on 2026-10-12. Only programmes no source feed carries
-- are added; the others were found in the feeds:
--   문화누리카드        gov_benefits ("문화누리카드 발급", "통합문화이용권(문화누리카드)")
--   청년전용 버팀목     manual row in review_required; gov/national "버팀목전세자금대출"
--   청년도약계좌        closed to new members since 2025-12 (금융위원회 2026-06-15,
--                       https://www.fsc.go.kr/no010101/87106); its youth_center row is
--                       already closed and its successor 청년미래적금 is in the catalogue
--   기후동행카드        youth_center row exists, but the 30-day pass ended on 2026-09-30
--                       (https://news.seoul.go.kr/traffic/archives/517575); its
--                       successor 기후동행패스 is added below
-- Facts are the operators' text only; amounts the operator page does not state
-- are left out.
-- 동백패스 has no youth band: it is open to every adult from 만 19세 (만 13~18세
-- use the separate 청소년 동백패스), checked against busan.go.kr and press
-- coverage on 2026-10-14. `youth_relevant` is the curator's call that it
-- belongs in a young Busan resident's default list (audit recall probe P1),
-- not an eligibility fact; no max_age is set. 기후동행패스 reads as youth from
-- its own 만 19~39세 청년 tier. Provenance only: this grants no fetch or submission authority.
INSERT INTO programs (
    program_type, source_type, source_id, title, summary, provider_name, official_url,
    program_status, provider_kind, benefit_category, application_method,
    min_age, max_age, regions, normalized_payload, raw_payload,
    is_active, last_synced_at
)
SELECT r.program_type, 'government_official', r.source_id, r.title, r.summary, r.provider,
       r.url, 'always_open', 'municipality', r.program_type, r.method,
       r.min_age, NULL, r.regions, r.normalized, r.raw, true, now()
FROM (VALUES
  (
    'benefit',
    'verified:20261012:gap:dongbaek-pass',
    '동백패스 (부산 대중교통 요금 환급)',
    '만 19세 이상이 동백전 교통카드로 부산 대중교통을 이용하면 한 달 이용액 중 45,000원을 넘는 금액을 최대 45,000원까지 다음 달 동백전 정책지원금으로 돌려받습니다. K-패스와 함께 가입하면 더 큰 환급이 적용됩니다.',
    '부산광역시 대중교통과',
    'https://www.busan.go.kr/depart/dongbaegpass001',
    '동백전 앱이나 부산은행 지점에서 동백전 교통카드를 발급받은 뒤 동백전 앱에서 동백패스에 가입합니다. 연중 가입할 수 있습니다.',
    19,
    -- The page states no residence requirement; the refund applies only to
    -- 부산 public transport, so the programme is listed for 부산.
    ARRAY['busan']::text[],
    '{"conditions":"만 19세 이상, 동백전 교통카드로 부산 대중교통 이용, 동백패스 가입 필요. 부산 거주 요건 없음.","scope_note":"usage_area_not_residence","youth_relevant":true}'::jsonb,
    '{"curation":{"batch":"2026-10-12-audit-gaps","status":"verified","reviewed_at":"2026-10-12","official_url":"https://www.busan.go.kr/depart/dongbaegpass001","corroboration":"https://www.busan.go.kr/nbnews/1721108"}}'::jsonb
  ),
  (
    'benefit',
    'verified:20261012:gap:seoul-climate-pass',
    '기후동행패스 (서울 모두의카드)',
    '주민등록상 주소지가 서울인 만 19세 이상 시민이 모두의카드(K-패스)로 대중교통을 이용하면 월 교통비 일부를 환급받습니다. 만 19~39세 청년은 월 교통비의 30%를 환급받습니다. 기존 기후동행카드 30일권을 대신합니다.',
    '서울특별시',
    'https://news.seoul.go.kr/traffic/archives/518138',
    '카드사에서 모두의카드를 발급받아 K-패스 누리집에 등록합니다. 등록한 다음 달부터 혜택이 적용됩니다.',
    19,
    ARRAY['seoul']::text[],
    '{"conditions":"주민등록상 주소지가 서울특별시인 만 19세 이상. 청년(만 19~39세) 월 교통비 30% 환급."}'::jsonb,
    '{"curation":{"batch":"2026-10-12-audit-gaps","status":"verified","reviewed_at":"2026-10-12","official_url":"https://news.seoul.go.kr/traffic/archives/518138","replaces":"https://news.seoul.go.kr/traffic/archives/517575"}}'::jsonb
  )
) AS r(program_type, source_id, title, summary, provider, url, method, min_age, regions, normalized, raw)
ON CONFLICT (source_type, source_id) DO NOTHING;
