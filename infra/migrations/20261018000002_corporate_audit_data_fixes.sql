-- Data fixes for the 3 WRONG items and the 1 PAGE_GONE item of the
-- 2026-10-16 corporate audit (design §6 P1 (a)).
--
-- Every fix below is checked twice against the audit's verbatim page text
-- before it applies:
-- 1. In CI, the Rust verbatim validator reads the spec in this file
--    (corporate_auto::evidence tests, `data_fix_spec_passes_the_verbatim_validator`):
--    each anchor is audit page text, every number written into a public field
--    occurs in its anchor, and every age written is what `parse_age` reads
--    from the anchor.
-- 2. Here, at apply time, a fix is skipped (NOTICE, no error) unless the
--    anchor is a substring of the audit quote embedded in the spec and the
--    programme still holds exactly the values the audit saw. A row the
--    audit did not see is never touched.
--
-- Fixes:
-- - 대구대 (911fa814) and 영남대 (25e5b9a6): the page is campus-agnostic, the
--   catalogue label was "…대구캠퍼스". The catalogue entry itself becomes
--   campus-agnostic (name without campus, the old name kept as an alias so
--   saved profiles still resolve); the programmes' school_scope and rule
--   explanation follow it.
-- - KT Y덤 (7d11ca91): min_age 19 → NULL (max 34 stays), summary rewritten
--   from the page's own wording ("만 34세 이하 고객", friend bundle "만 19세~34세").
-- - 공차 (8456d303): HTTP 404 → retired as provider_withdrawn.
--
-- Rollback (do not run automatically): every changed row is copied to
-- corporate_audit_fix_backup first.
--   UPDATE programs p SET school_scope = (b.before->'school_scope'), min_age = (b.before->>'min_age')::int,
--       max_age = (b.before->>'max_age')::int, summary = b.before->>'summary',
--       is_active = (b.before->>'is_active')::boolean, program_status = b.before->>'program_status',
--       retired_at = (b.before->>'retired_at')::timestamptz, retirement_reason = b.before->>'retirement_reason'
--   FROM corporate_audit_fix_backup b WHERE b.program_id = p.id;
--   UPDATE universities u SET name = b.before->>'name',
--       aliases = ARRAY(SELECT jsonb_array_elements_text(b.before->'aliases'))
--   FROM corporate_audit_fix_backup b WHERE b.fix_key = 'catalogue:' || u.id;
--   (eligibility_rules: restore b.before->'explain_json' and ->'rule_json' the same way.)

CREATE TABLE IF NOT EXISTS corporate_audit_fix_backup (
    fix_key     TEXT        NOT NULL,
    program_id  UUID        REFERENCES programs(id) ON DELETE CASCADE,
    before      JSONB       NOT NULL,
    after       JSONB       NOT NULL,
    applied_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (fix_key)
);
ALTER TABLE corporate_audit_fix_backup ENABLE ROW LEVEL SECURITY;
ALTER TABLE corporate_audit_fix_backup FORCE ROW LEVEL SECURITY;
REVOKE ALL ON TABLE corporate_audit_fix_backup FROM PUBLIC;
DO $rls$
BEGIN
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
    REVOKE ALL ON TABLE corporate_audit_fix_backup FROM anon;
  END IF;
  IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'authenticated') THEN
    REVOKE ALL ON TABLE corporate_audit_fix_backup FROM authenticated;
  END IF;
END
$rls$;
DROP POLICY IF EXISTS corporate_audit_fix_backup_all_service ON corporate_audit_fix_backup;
CREATE POLICY corporate_audit_fix_backup_all_service ON corporate_audit_fix_backup
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

DO $fix$
DECLARE
  spec JSONB := $spec$
[
  {"key": "daegu_school_scope", "kind": "scope_campus_agnostic",
   "audit_id": "911fa814-da34-424d-97ac-ba181bd2f6c3",
   "source_type": "university_official", "source_id": "verified:20260923:expansion2:daegu_office365",
   "title": "대구대 재학생 Office 365",
   "university_id": "daegu-daegu", "old_name": "대구대학교 대구캠퍼스", "new_name": "대구대학교",
   "extra_aliases": ["대구대학교 대구캠퍼스", "대구대학교 경산캠퍼스"],
   "anchor": "캠퍼스 제한 없음",
   "audit_quote": "자퇴/휴학/제적/수료/졸업생",
   "audit_page_says": "페이지는 학부·대학원 재학생 전체 대상(캠퍼스 제한 없음). 저장 school_scope는 '대구대학교 대구캠퍼스'(catalog id daegu-daegu)로, 본교(경산) 재학생이 빠지는 라벨."},
  {"key": "yu_school_scope", "kind": "scope_campus_agnostic",
   "audit_id": "25e5b9a6-3aff-4609-8a64-6b475d41c093",
   "source_type": "university_official", "source_id": "verified:20260923:expansion2:yu_office365",
   "title": "영남대 재학생 Office 365",
   "university_id": "yu-daegu", "old_name": "영남대학교 대구캠퍼스", "new_name": "영남대학교",
   "extra_aliases": ["영남대학교 대구캠퍼스", "영남대학교 경산캠퍼스"],
   "anchor": "캠퍼스 제한 없음",
   "audit_quote": "재학생은 Office 365 서비스를 통해 최상위 버전의 정품 오피스를 설치 사용할 수 있습니다.",
   "audit_page_says": "페이지는 '재학생' 전체 대상(캠퍼스 제한 없음). 저장 school_scope는 '영남대학교 대구캠퍼스'(catalog id yu-daegu)로, 본교(경산) 재학생이 빠지는 라벨."},
  {"key": "kt_ybonus_age", "kind": "age_and_summary",
   "audit_id": "7d11ca91-bc71-4865-b163-db0d57ac2a23",
   "source_type": "corporate_official", "source_id": "https://product.kt.com/benefit/membership/mobile/y-bonus.html",
   "title": "KT Y덤 청년 데이터 혜택",
   "expect": {"min_age": 19, "max_age": 34},
   "set": {"min_age": null, "max_age": 34},
   "anchor": "만 34세 이하 고객 중 아래 요금제 가입 고객",
   "replacements": [
     {"from": "만 19~34세가", "to": "만 34세 이하 고객이", "anchor": "만 34세 이하 고객 중 아래 요금제 가입 고객"},
     {"from": "추가 제공됩니다.", "to": "추가 제공됩니다. 친구 결합 혜택은 만 19세~34세 고객에게 적용됩니다.", "anchor": "만 19세~34세"}
   ],
   "audit_quote": "만 34세 이하 고객 중 아래 요금제 가입 고객",
   "audit_page_says": "페이지 기본 대상은 '만 34세 이하 고객 중 아래 요금제 가입 고객'(하한 없음). '만 19세~34세'는 친구 결합 혜택에만 해당. 저장값 min_age=19·요약 '만 19~34세'가 기본 대상을 좁게 씀."},
  {"key": "gongcha_gone", "kind": "retire_gone",
   "audit_id": "8456d303-f76f-4a9e-ae63-a2a80aed0f46",
   "source_type": "corporate_official", "source_id": "https://www.gong-cha.co.kr/brand/member/membership1.php",
   "title": "공차 멤버십 스탬프 혜택",
   "audit_http": "404", "audit_quote": "", "audit_page_says": "공식 URL이 HTTP 404(공차 오류 페이지)."}
]
$spec$;
  f JSONB;
  r JSONB;
  p RECORD;
  u RECORD;
  new_summary TEXT;
  anchor_ok BOOLEAN;
BEGIN
  FOR f IN SELECT * FROM jsonb_array_elements(spec) LOOP
    IF EXISTS (SELECT 1 FROM corporate_audit_fix_backup WHERE fix_key = f->>'key') THEN
      CONTINUE;
    END IF;
    SELECT * INTO p FROM programs
    WHERE source_type = f->>'source_type' AND source_id = f->>'source_id' AND title = f->>'title'
      AND (id = (f->>'audit_id')::uuid OR NOT EXISTS (SELECT 1 FROM programs WHERE id = (f->>'audit_id')::uuid));
    IF NOT FOUND THEN
      RAISE NOTICE 'audit fix %: programme not found, skipped', f->>'key';
      CONTINUE;
    END IF;
    anchor_ok := f->>'kind' = 'retire_gone'
      OR position(f->>'anchor' IN f->>'audit_quote') > 0
      OR position(f->>'anchor' IN f->>'audit_page_says') > 0;
    IF NOT anchor_ok THEN
      RAISE NOTICE 'audit fix %: anchor is not audit page text, skipped', f->>'key';
      CONTINUE;
    END IF;

    IF f->>'kind' = 'scope_campus_agnostic' THEN
      SELECT * INTO u FROM universities WHERE id = f->>'university_id';
      IF NOT FOUND OR p.university_id IS DISTINCT FROM f->>'university_id'
         OR p.school_scope IS DISTINCT FROM jsonb_build_array(f->>'old_name')
         OR (u.name <> f->>'old_name' AND u.name <> f->>'new_name') THEN
        RAISE NOTICE 'audit fix %: values differ from what the audit saw, skipped', f->>'key';
        CONTINUE;
      END IF;
      IF u.name = f->>'old_name' THEN
        INSERT INTO corporate_audit_fix_backup (fix_key, program_id, before, after)
        VALUES ('catalogue:' || u.id, NULL, jsonb_build_object('name', u.name, 'aliases', to_jsonb(u.aliases)),
                jsonb_build_object('name', f->>'new_name'))
        ON CONFLICT (fix_key) DO NOTHING;
        UPDATE universities
        SET name = f->>'new_name',
            aliases = ARRAY(SELECT DISTINCT a FROM unnest(aliases || ARRAY(SELECT jsonb_array_elements_text(f->'extra_aliases'))) a
                            WHERE a <> f->>'new_name' ORDER BY a)
        WHERE id = u.id;
      END IF;
      INSERT INTO corporate_audit_fix_backup (fix_key, program_id, before, after)
      SELECT f->>'key', p.id,
             jsonb_build_object('school_scope', p.school_scope,
                                'explain_json', (SELECT explain_json FROM eligibility_rules WHERE program_id = p.id ORDER BY version DESC LIMIT 1),
                                'rule_json', (SELECT rule_json FROM eligibility_rules WHERE program_id = p.id ORDER BY version DESC LIMIT 1)),
             jsonb_build_object('school_scope', jsonb_build_array(f->>'new_name'));
      UPDATE programs SET school_scope = jsonb_build_array(f->>'new_name'), updated_at = now() WHERE id = p.id;
      UPDATE eligibility_rules er
      SET explain_json = jsonb_set(er.explain_json, '{school_name}', to_jsonb((f->>'new_name') || ' 구성원 대상')),
          rule_json = (
            SELECT jsonb_build_object('all', jsonb_agg(
              CASE WHEN cond->>'field' = 'school_name'
                   THEN jsonb_set(cond, '{value}', (SELECT to_jsonb(array_agg(DISTINCT v ORDER BY v)) FROM (
                          SELECT jsonb_array_elements_text(cond->'value') v
                          UNION SELECT jsonb_array_elements_text(f->'extra_aliases')
                          UNION SELECT f->>'new_name') vals))
                   ELSE cond END ORDER BY ord))
            FROM jsonb_array_elements(er.rule_json->'all') WITH ORDINALITY AS t(cond, ord)),
          hard_filter_json = (
            SELECT jsonb_build_object('all', jsonb_agg(
              CASE WHEN cond->>'field' = 'school_name'
                   THEN jsonb_set(cond, '{value}', (SELECT to_jsonb(array_agg(DISTINCT v ORDER BY v)) FROM (
                          SELECT jsonb_array_elements_text(cond->'value') v
                          UNION SELECT jsonb_array_elements_text(f->'extra_aliases')
                          UNION SELECT f->>'new_name') vals))
                   ELSE cond END ORDER BY ord))
            FROM jsonb_array_elements(er.hard_filter_json->'all') WITH ORDINALITY AS t(cond, ord))
      WHERE er.program_id = p.id AND er.rule_json ? 'all' AND er.hard_filter_json ? 'all';

    ELSIF f->>'kind' = 'age_and_summary' THEN
      IF p.min_age IS DISTINCT FROM (f->'expect'->>'min_age')::int
         OR p.max_age IS DISTINCT FROM (f->'expect'->>'max_age')::int THEN
        RAISE NOTICE 'audit fix %: ages differ from what the audit saw, skipped', f->>'key';
        CONTINUE;
      END IF;
      new_summary := p.summary;
      FOR r IN SELECT * FROM jsonb_array_elements(f->'replacements') LOOP
        IF (length(new_summary) - length(replace(new_summary, r->>'from', ''))) / greatest(length(r->>'from'), 1) <> 1
           OR (position(r->>'anchor' IN f->>'audit_quote') = 0 AND position(r->>'anchor' IN f->>'audit_page_says') = 0) THEN
          RAISE NOTICE 'audit fix %: replacement % is not unique or not anchored, skipped', f->>'key', r->>'from';
          new_summary := NULL;
          EXIT;
        END IF;
        new_summary := replace(new_summary, r->>'from', r->>'to');
      END LOOP;
      IF new_summary IS NULL THEN
        CONTINUE;
      END IF;
      INSERT INTO corporate_audit_fix_backup (fix_key, program_id, before, after)
      VALUES (f->>'key', p.id,
              jsonb_build_object('min_age', p.min_age, 'max_age', p.max_age, 'summary', p.summary),
              jsonb_build_object('min_age', f->'set'->'min_age', 'max_age', f->'set'->'max_age', 'summary', new_summary));
      UPDATE programs
      SET min_age = (f->'set'->>'min_age')::int, max_age = (f->'set'->>'max_age')::int,
          summary = new_summary, updated_at = now()
      WHERE id = p.id;

    ELSIF f->>'kind' = 'retire_gone' THEN
      IF f->>'audit_http' NOT IN ('404', '410') OR p.retirement_reason IS NOT NULL THEN
        RAISE NOTICE 'audit fix %: not a gone page or already retired, skipped', f->>'key';
        CONTINUE;
      END IF;
      INSERT INTO corporate_audit_fix_backup (fix_key, program_id, before, after)
      VALUES (f->>'key', p.id,
              jsonb_build_object('is_active', p.is_active, 'program_status', p.program_status,
                                 'retired_at', p.retired_at, 'retirement_reason', p.retirement_reason),
              jsonb_build_object('is_active', false, 'program_status', 'closed', 'retirement_reason', 'provider_withdrawn'));
      UPDATE programs
      SET is_active = false, program_status = 'closed', retired_at = now(),
          retirement_reason = 'provider_withdrawn', updated_at = now()
      WHERE id = p.id;
      UPDATE program_source_monitors
      SET verification_state = 'retired', last_error = 'official page returned HTTP 404 (audit 2026-10-16)', updated_at = now()
      WHERE program_id = p.id;
      UPDATE program_source_holds
      SET resolved_at = now(), resolution = 'retired',
          resolution_note = 'Retired: the official page returned HTTP 404 in the 2026-10-16 audit'
      WHERE program_id = p.id AND resolved_at IS NULL;
      UPDATE programs SET source_health = 'retired', updated_at = now()
      WHERE id = p.id AND NOT EXISTS (
          SELECT 1 FROM program_source_monitors WHERE program_id = p.id AND verification_state <> 'retired');
    END IF;
  END LOOP;
END
$fix$;
