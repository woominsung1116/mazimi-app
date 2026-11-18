-- Published-data fixes from the 2026-10-26 corporate health audit
-- (`.reports/corporate-health-20261026.md`). Additive and idempotent:
-- nothing is deleted, every change is guarded by the value it replaces,
-- every changed row is copied to corporate_audit_fix_backup first, and a
-- fix already applied (its key in the backup) is skipped.
--
-- Every anchor is verbatim text of the official page as fetched on
-- 2026-10-26 (`crates/worker/tests/fixtures/audit_20261026`); CI checks it
-- with the verbatim validator (corporate_auto::evidence tests,
-- `fixes_20261026_pass_the_verbatim_validator`): each anchor is text of
-- its saved page, every number written into a public field occurs in its
-- anchor, and every age written is what `parse_age` reads from it.
--
-- Fixes:
-- - spotify_duplicate: two shown programmes for one offer page. The check
--   of 20261018000015 decides which one stays: references in every user
--   table (a table with a user_id column and a foreign key to programs,
--   enumerated from pg_catalog so the migration role sees every one); the
--   referenced one is kept, with both or neither 0daf688b, as
--   20261018000015 decides.
--   The other is retired as its duplicate the way pipeline.rs
--   retire_as_duplicate does. A user's bookmark, saved state or alert on
--   the retired row moves to the kept one unless the user already has one
--   there; delivery and history rows stay as recorded.
-- - spotify_age: the kept summary and min_age state the page's 18+. Written
--   for 0daf688b; when spotify_duplicate kept the other row, it fixes that
--   one (every guard below still applies to it).
-- - sketch_exclusions: the summary named 창업·사업체 as excluded, which the
--   page does not; it now names the page's own exclusions.
--
-- Rollback (do not run automatically):
--   UPDATE programs p SET summary = b.before->>'summary',
--       min_age = (b.before->>'min_age')::int,
--       is_active = (b.before->>'is_active')::boolean,
--       retired_at = (b.before->>'retired_at')::timestamptz,
--       retirement_reason = b.before->>'retirement_reason',
--       normalized_payload = b.before->'normalized_payload'
--   FROM corporate_audit_fix_backup b
--   WHERE b.program_id = p.id AND b.fix_key LIKE '20261026:%' AND b.before ? 'summary';
--   -- Moved user rows: b.before->'rows' (fix keys '20261026:spotify_duplicate:<table>')
--   -- hold every moved row as it was; set their program column back.

DO $fix$
DECLARE
  spec JSONB := $spec$
[
  {"key": "spotify_duplicate", "kind": "retire_duplicate",
   "programmes": ["0daf688b-691c-44d6-9e5a-f3ea9f481a34", "7af27e95-3a01-41f6-8639-6a9585cec9b9"],
   "title": "Spotify Premium 학생 요금제",
   "official_url": "https://www.spotify.com/kr-ko/student/"},
  {"key": "spotify_age", "kind": "age_and_summary",
   "id": "0daf688b-691c-44d6-9e5a-f3ea9f481a34", "title": "Spotify Premium 학생 요금제",
   "page_file": "32fe43be98.html",
   "expect": {"min_age": null, "max_age": null},
   "set": {"min_age": 18, "max_age": null},
   "anchor": "승인된 대학 교육 기관에 등록된 학생이며 18세 이상인 경우 혜택 제공 대상이며",
   "replacements": [
     {"from": "대상 대학 교육기관의 학생은", "to": "만 18세 이상으로 대상 대학 교육기관에 등록된 학생은",
      "anchor": "승인된 대학 교육 기관에 등록된 학생이며 18세 이상인 경우 혜택 제공 대상이며"}
   ]},
  {"key": "sketch_exclusions", "kind": "summary",
   "id": "1249aeda-db5b-46a5-b89e-3dc1005f9be8", "title": "Sketch Education 무료 워크스페이스",
   "page_file": "b19f82fd11.html",
   "replacements": [
     {"from": "독학과 창업·사업체 용도는 대상이 아닙니다.",
      "to": "독학·홈러닝, 비영리 단체, 교육기관의 마케팅 부서는 대상이 아닙니다.",
      "anchor": "the Educational Plan does not apply to anyone with home-learning or Self-Education, Not-for-Profit organizations, and Marketing departments from Academic Institutions"}
   ]}
]
$spec$;
  f JSONB;
  r JSONB;
  p RECORD;
  new_summary TEXT;
  ref RECORD;
  a UUID;
  b UUID;
  keep UUID;
  other UUID;
  refs_a BIGINT;
  refs_b BIGINT;
  n_a BIGINT;
  n_b BIGINT;
  moved JSONB;
  target UUID;
BEGIN
  FOR f IN SELECT * FROM jsonb_array_elements(spec) LOOP
    IF EXISTS (SELECT 1 FROM corporate_audit_fix_backup WHERE fix_key = '20261026:' || (f->>'key')) THEN
      CONTINUE;
    END IF;

    IF f->>'kind' = 'retire_duplicate' THEN
      a := (f->'programmes'->>0)::uuid;
      b := (f->'programmes'->>1)::uuid;
      IF (SELECT count(*) FROM programs
          WHERE id IN (a, b) AND is_active AND title = f->>'title' AND official_url = f->>'official_url') < 2 THEN
        RAISE NOTICE 'fix %: the two programmes are not both shown on this page, nothing to do', f->>'key';
        CONTINUE;
      END IF;
      refs_a := 0;
      refs_b := 0;
      -- Every user-owned column with a foreign key to programs(id), from
      -- pg_catalog (visible to the migration role whatever it owns).
      FOR ref IN
        SELECT c.relname::text AS table_name, att.attname::text AS column_name
        FROM pg_catalog.pg_constraint k
        JOIN pg_catalog.pg_class c ON c.oid = k.conrelid
        JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace AND n.nspname = 'public'
        JOIN pg_catalog.pg_attribute att ON att.attrelid = k.conrelid AND att.attnum = ANY (k.conkey)
        WHERE k.contype = 'f' AND k.confrelid = 'public.programs'::regclass
          AND c.relname <> 'programs'
          AND EXISTS (SELECT 1 FROM pg_catalog.pg_attribute u
                      WHERE u.attrelid = c.oid AND u.attname = 'user_id' AND NOT u.attisdropped)
        ORDER BY 1, 2
      LOOP
        EXECUTE format('SELECT count(*) FILTER (WHERE %1$I = $1), count(*) FILTER (WHERE %1$I = $2) FROM public.%2$I',
                       ref.column_name, ref.table_name)
          INTO n_a, n_b USING a, b;
        refs_a := refs_a + n_a;
        refs_b := refs_b + n_b;
      END LOOP;
      RAISE NOTICE 'fix %: user references % = %, % = %', f->>'key', a, refs_a, b, refs_b;
      -- The referenced one stays; with both or neither, a (20261018000015).
      IF refs_b > 0 AND refs_a = 0 THEN
        keep := b; other := a;
      ELSE
        keep := a; other := b;
      END IF;
      -- Users' bookmarks, saved states and alerts follow the kept row,
      -- unless the user already has one there (the row stays as it was;
      -- each table's only uniqueness is (user_id, program_id)).
      FOR ref IN SELECT * FROM (VALUES ('user_bookmarks'), ('user_program_states'), ('alert_subscriptions')) t(table_name)
      LOOP
        IF to_regclass('public.' || ref.table_name) IS NULL THEN
          CONTINUE;
        END IF;
        EXECUTE format(
          'WITH movable AS (
             SELECT t.ctid FROM public.%1$I t
             WHERE t.program_id = $1
               AND NOT EXISTS (SELECT 1 FROM public.%1$I k WHERE k.program_id = $2 AND k.user_id = t.user_id)),
           backed AS (SELECT jsonb_agg(to_jsonb(t)) AS rows FROM public.%1$I t WHERE t.ctid IN (SELECT ctid FROM movable)),
           done AS (UPDATE public.%1$I t SET program_id = $2 WHERE t.ctid IN (SELECT ctid FROM movable) RETURNING 1)
           SELECT (SELECT rows FROM backed) FROM (SELECT count(*) FROM done) d',
          ref.table_name)
          INTO moved USING other, keep;
        IF moved IS NOT NULL THEN
          INSERT INTO corporate_audit_fix_backup (fix_key, program_id, before, after)
          VALUES ('20261026:' || (f->>'key') || ':' || ref.table_name, other,
                  jsonb_build_object('rows', moved), jsonb_build_object('program_id', keep))
          ON CONFLICT (fix_key) DO NOTHING;
          RAISE NOTICE 'fix %: moved % row(s) of % to %', f->>'key', jsonb_array_length(moved), ref.table_name, keep;
        END IF;
      END LOOP;
      INSERT INTO corporate_audit_fix_backup (fix_key, program_id, before, after)
      SELECT '20261026:' || (f->>'key'), p2.id,
             jsonb_build_object('summary', p2.summary, 'min_age', p2.min_age, 'is_active', p2.is_active,
                                'retired_at', p2.retired_at, 'retirement_reason', p2.retirement_reason,
                                'normalized_payload', p2.normalized_payload),
             jsonb_build_object('is_active', false, 'retirement_reason', 'duplicate', 'duplicate_of', keep)
      FROM programs p2 WHERE p2.id = other;
      -- The guard of pipeline.rs retire_as_duplicate: never over a manual
      -- (or any other) retirement.
      UPDATE programs
      SET is_active = false,
          retired_at = now(),
          retirement_reason = 'duplicate',
          normalized_payload = COALESCE(normalized_payload, '{}'::jsonb) || jsonb_build_object('duplicate_of', keep::text),
          updated_at = now()
      WHERE id = other AND is_active;

    ELSE
      -- A target this migration retired as a duplicate: the fix lands on
      -- the row kept in its place (its backup's duplicate_of).
      target := COALESCE(
        (SELECT (d.after->>'duplicate_of')::uuid FROM corporate_audit_fix_backup d
         WHERE d.fix_key LIKE '20261026:%' AND d.program_id = (f->>'id')::uuid AND d.after ? 'duplicate_of'
         LIMIT 1),
        (f->>'id')::uuid);
      IF target <> (f->>'id')::uuid THEN
        RAISE NOTICE 'fix %: % was retired as a duplicate of %, fixing the kept row', f->>'key', f->>'id', target;
      END IF;
      SELECT * INTO p FROM programs WHERE id = target AND title = f->>'title';
      IF NOT FOUND THEN
        RAISE NOTICE 'fix %: programme not found, skipped', f->>'key';
        CONTINUE;
      END IF;
      IF f->>'kind' = 'age_and_summary'
         AND (p.min_age IS DISTINCT FROM (f->'expect'->>'min_age')::int
              OR p.max_age IS DISTINCT FROM (f->'expect'->>'max_age')::int) THEN
        RAISE NOTICE 'fix %: ages differ from what the audit saw, skipped', f->>'key';
        CONTINUE;
      END IF;
      new_summary := p.summary;
      FOR r IN SELECT * FROM jsonb_array_elements(f->'replacements') LOOP
        IF (length(new_summary) - length(replace(new_summary, r->>'from', ''))) / greatest(length(r->>'from'), 1) <> 1 THEN
          RAISE NOTICE 'fix %: % is not in the summary exactly once, skipped', f->>'key', r->>'from';
          new_summary := NULL;
          EXIT;
        END IF;
        new_summary := replace(new_summary, r->>'from', r->>'to');
      END LOOP;
      IF new_summary IS NULL THEN
        CONTINUE;
      END IF;
      INSERT INTO corporate_audit_fix_backup (fix_key, program_id, before, after)
      VALUES ('20261026:' || (f->>'key'), p.id,
              jsonb_build_object('summary', p.summary, 'min_age', p.min_age, 'max_age', p.max_age),
              jsonb_build_object('summary', new_summary,
                                 'min_age', COALESCE(f->'set'->'min_age', to_jsonb(p.min_age)),
                                 'max_age', COALESCE(f->'set'->'max_age', to_jsonb(p.max_age))));
      UPDATE programs
      SET summary = new_summary,
          min_age = CASE WHEN f->>'kind' = 'age_and_summary' THEN (f->'set'->>'min_age')::int ELSE min_age END,
          max_age = CASE WHEN f->>'kind' = 'age_and_summary' THEN (f->'set'->>'max_age')::int ELSE max_age END,
          updated_at = now()
      WHERE id = p.id AND summary = p.summary;
    END IF;
  END LOOP;
END
$fix$;
