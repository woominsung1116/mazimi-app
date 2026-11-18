-- Published-data fixes from the 2026-10-18 real-page ground truth (handoff
-- §18). Idempotent: every change is guarded by the value it replaces.
-- Nothing is retired except a duplicate no user references.
--
-- 1. Nike student discount (5fa3af29): the official page offers a 대학생
--    할인 프로그램 (대학생 인증하기), not a discount for every 학생. The title,
--    summary and conditions say 대학생, and a rule on `education_stage` (the
--    rule engine derives it from `school_type` through its canonical form,
--    so `대학교`, `전문대학`, `대학원` and `university` all count) keeps
--    high-school profiles out. An existing eligibility rule is never
--    overwritten.
-- 2. Spotify Premium 학생 요금제: two programmes for one URL (0daf688b,
--    7af27e95). References are counted in every user table (a table whose
--    rows have an owner, a user_id column) that has a foreign key to
--    programs, enumerated from information_schema, so a new table counts
--    without editing this file. The referenced one is kept and the other
--    retired as its duplicate the way pipeline.rs retire_as_duplicate does
--    (retirement_reason 'duplicate', normalized_payload.duplicate_of, and
--    never over a manual retirement). If both are referenced, if the user
--    tables cannot be read, or if the one kept would not be shown, both
--    stay and a NOTICE reports the counts.
--
-- Rollback (do not run automatically):
--   UPDATE programs SET title = '나이키 학생 10% 할인',
--     summary = '학생 자격 인증 후 대상 상품에 10% 할인을 적용할 수 있습니다. 제외 상품과 쿠폰 사용 조건이 있으며 자격 인증은 30일마다 갱신해야 합니다.',
--     normalized_payload = jsonb_set(normalized_payload, '{conditions}', '"학생 자격 인증, 대상 상품 및 제외 조건 확인"')
--     WHERE id = '5fa3af29-0c99-489b-8ebd-1ef063bf25b4' AND title = '나이키 대학생 10% 할인';
--   DELETE FROM eligibility_rules WHERE created_by = 'migration:20261018000015';
--   -- As pipeline.rs restore_from_duplicate: a row whose own window ended
--   -- (or whose status is closed) comes back as expired, not shown.
--   UPDATE programs p SET
--     is_active = r.open,
--     program_status = CASE WHEN r.open THEN p.program_status ELSE 'closed' END,
--     retirement_reason = CASE WHEN r.open THEN NULL ELSE 'expired' END,
--     retired_at = CASE WHEN r.open THEN NULL ELSE p.retired_at END,
--     normalized_payload = p.normalized_payload - 'duplicate_of', updated_at = now()
--   FROM (SELECT id, program_status <> 'closed'
--                AND (LEAST(benefit_valid_until, deadline_at, application_end_at) IS NULL
--                     OR LEAST(benefit_valid_until, deadline_at, application_end_at) > now()) AS open
--         FROM programs
--         WHERE NOT is_active AND retirement_reason = 'duplicate'
--           AND normalized_payload->>'duplicate_of' IN ('0daf688b-691c-44d6-9e5a-f3ea9f481a34', '7af27e95-3a01-41f6-8639-6a9585cec9b9')) r
--   WHERE p.id = r.id;

DO $nike$
DECLARE
    nike UUID;
BEGIN
    SELECT id INTO nike FROM programs
    WHERE id = '5fa3af29-0c99-489b-8ebd-1ef063bf25b4'
       OR (source_type = 'corporate_official' AND source_id = 'verified:20260923:nike')
    ORDER BY (id = '5fa3af29-0c99-489b-8ebd-1ef063bf25b4') DESC
    LIMIT 1;
    IF nike IS NULL THEN
        RAISE NOTICE 'nike: no programme, nothing to fix';
        RETURN;
    END IF;
    UPDATE programs SET title = '나이키 대학생 10% 할인', updated_at = now()
    WHERE id = nike AND title = '나이키 학생 10% 할인';
    UPDATE programs
    SET summary = '대학생 자격 인증 후 대상 상품에 10% 할인을 적용할 수 있습니다. 제외 상품과 쿠폰 사용 조건이 있으며 자격 인증은 30일마다 갱신해야 합니다.',
        updated_at = now()
    WHERE id = nike
      AND summary = '학생 자격 인증 후 대상 상품에 10% 할인을 적용할 수 있습니다. 제외 상품과 쿠폰 사용 조건이 있으며 자격 인증은 30일마다 갱신해야 합니다.';
    UPDATE programs
    SET normalized_payload = jsonb_set(normalized_payload, '{conditions}', '"대학생 자격 인증, 대상 상품 및 제외 조건 확인"'),
        updated_at = now()
    WHERE id = nike
      AND normalized_payload->>'conditions' = '학생 자격 인증, 대상 상품 및 제외 조건 확인';
    IF EXISTS (SELECT 1 FROM eligibility_rules WHERE program_id = nike) THEN
        RAISE NOTICE 'nike: an eligibility rule exists already, left as it is';
    ELSE
        INSERT INTO eligibility_rules (program_id, rule_json, hard_filter_json, explain_json, version, created_by, compiled_at)
        VALUES (
            nike,
            '{"all": [{"field": "education_stage", "op": "in", "value": ["univ_enrolled", "univ_expected", "postgrad"], "weight": 100}]}'::jsonb,
            '{"all": [{"field": "education_stage", "op": "in", "value": ["univ_enrolled", "univ_expected", "postgrad"]}]}'::jsonb,
            '{"education_stage": "대학(원) 재학생 대상 (공식 페이지: 대학생 할인 프로그램)"}'::jsonb,
            1,
            'migration:20261018000015',
            now()
        );
    END IF;
END
$nike$;

DO $spotify$
DECLARE
    a UUID := '0daf688b-691c-44d6-9e5a-f3ea9f481a34';
    b UUID := '7af27e95-3a01-41f6-8639-6a9585cec9b9';
    refs_a BIGINT := 0;
    refs_b BIGINT := 0;
    n_a BIGINT;
    n_b BIGINT;
    tables_read INT := 0;
    core_read INT := 0;
    ref RECORD;
    keep UUID;
    other UUID;
    retired INT;
    hidden_tables INT;
BEGIN
    IF (SELECT count(*) FROM programs WHERE id IN (a, b)) < 2 THEN
        RAISE NOTICE 'spotify: the two programmes are not both present, nothing to do';
        RETURN;
    END IF;
    -- Every column with a foreign key to programs(id) in a user table.
    FOR ref IN
        SELECT DISTINCT kcu.table_name::text AS table_name, kcu.column_name::text AS column_name
        FROM information_schema.table_constraints tc
        JOIN information_schema.key_column_usage kcu
          ON kcu.constraint_schema = tc.constraint_schema AND kcu.constraint_name = tc.constraint_name
        JOIN information_schema.constraint_column_usage ccu
          ON ccu.constraint_schema = tc.constraint_schema AND ccu.constraint_name = tc.constraint_name
        WHERE tc.constraint_type = 'FOREIGN KEY'
          AND tc.table_schema = 'public'
          AND ccu.table_schema = 'public' AND ccu.table_name = 'programs' AND ccu.column_name = 'id'
          AND tc.table_name <> 'programs'
          -- A user's row: it has an owner (user_id). Tables that only name
          -- a reviewer or an operator (holds, duplicate candidates, AI
          -- overrides) are the catalogue's, not users'.
          AND EXISTS (SELECT 1 FROM information_schema.columns c
                      WHERE c.table_schema = 'public' AND c.table_name = tc.table_name
                        AND c.column_name = 'user_id')
        ORDER BY 1, 2
    LOOP
        EXECUTE format('SELECT count(*) FILTER (WHERE %1$I = $1), count(*) FILTER (WHERE %1$I = $2) FROM public.%2$I',
                       ref.column_name, ref.table_name)
            INTO n_a, n_b USING a, b;
        refs_a := refs_a + n_a;
        refs_b := refs_b + n_b;
        tables_read := tables_read + 1;
        IF ref.table_name IN ('user_bookmarks', 'user_program_states') THEN
            core_read := core_read + 1;
        END IF;
        IF n_a > 0 OR n_b > 0 THEN
            RAISE NOTICE 'spotify: %.% references % = %, % = %', ref.table_name, ref.column_name, a, n_a, b, n_b;
        END IF;
    END LOOP;
    -- Fail safe: the role cannot see the user tables' constraints, or a
    -- table with a program_id column is hidden from it (its foreign keys
    -- missing from information_schema, or no SELECT on it).
    IF core_read < 2 THEN
        RAISE NOTICE 'spotify: user tables not readable (% read), both kept', tables_read;
        RETURN;
    END IF;
    SELECT count(*) INTO hidden_tables
    FROM pg_catalog.pg_class c
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p')
      AND EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
                  WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
                    AND a.attname LIKE '%program_id%')
      AND (
          NOT has_table_privilege(c.oid, 'SELECT')
          OR (SELECT count(*) FROM pg_catalog.pg_constraint k
              WHERE k.conrelid = c.oid AND k.contype = 'f'
                AND k.confrelid = 'public.programs'::regclass)
             <> (SELECT count(DISTINCT tc.constraint_name)
                 FROM information_schema.table_constraints tc
                 JOIN information_schema.constraint_column_usage ccu
                   ON ccu.constraint_schema = tc.constraint_schema AND ccu.constraint_name = tc.constraint_name
                 WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_schema = 'public'
                   AND tc.table_name = c.relname
                   AND ccu.table_schema = 'public' AND ccu.table_name = 'programs')
      );
    IF hidden_tables > 0 THEN
        RAISE NOTICE 'spotify: % table(s) with a program_id column not fully visible to this role, both kept', hidden_tables;
        RETURN;
    END IF;
    RAISE NOTICE 'spotify: references over % user columns: % = %, % = %', tables_read, a, refs_a, b, refs_b;
    IF refs_a > 0 AND refs_b > 0 THEN
        RAISE NOTICE 'spotify: both are referenced, both kept';
        RETURN;
    END IF;
    -- The referenced one; with none, the one shown.
    IF refs_b > 0 OR (refs_a = 0 AND NOT (SELECT is_active FROM programs WHERE id = a)
                      AND (SELECT is_active FROM programs WHERE id = b)) THEN
        keep := b; other := a;
    ELSE
        keep := a; other := b;
    END IF;
    IF NOT (SELECT is_active FROM programs WHERE id = keep)
       AND (SELECT is_active FROM programs WHERE id = other) THEN
        RAISE NOTICE 'spotify: the referenced programme % is not shown while % is; both kept', keep, other;
        RETURN;
    END IF;
    -- The guard of pipeline.rs retire_as_duplicate: an active row, or one
    -- retired as expired, removed at source or a duplicate of another; a
    -- manual (or any other) retirement is never overwritten.
    UPDATE programs
    SET is_active = false,
        retired_at = CASE WHEN is_active OR retired_at IS NULL THEN now() ELSE retired_at END,
        retirement_reason = 'duplicate',
        normalized_payload = COALESCE(normalized_payload, '{}'::jsonb) || jsonb_build_object('duplicate_of', keep::text),
        updated_at = now()
    WHERE id = other
      AND (is_active OR retirement_reason IN ('expired', 'source_removed', 'duplicate'))
      AND (is_active OR retirement_reason <> 'duplicate'
           OR normalized_payload->>'duplicate_of' IS DISTINCT FROM keep::text);
    GET DIAGNOSTICS retired = ROW_COUNT;
    IF retired > 0 THEN
        RAISE NOTICE 'spotify: kept %, % retired as its duplicate', keep, other;
    ELSE
        RAISE NOTICE 'spotify: kept %, % left as it is (already its duplicate, or retired otherwise)', keep, other;
    END IF;
END
$spotify$;
