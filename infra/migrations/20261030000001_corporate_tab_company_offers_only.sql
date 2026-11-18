-- 기업혜택 means company offers only (rule version 2 of 20261023000002).
--
-- Version 1 filed two other kinds of row under the 기업혜택 provider tab:
--   - a school's own offer (program_type campus_benefit): 부산대 Microsoft 365
--     and the like were listed under 기업혜택 for every viewer, whatever
--     their school. Such an offer is for one school's students, so it is
--     listed only by the school-scoped 대학 tab (GET /api/v1/campus/programs,
--     mazimi_core::categories::school_offer_sql) and on no provider tab;
--   - a bank's deposit (program_type financial_product from a company,
--     provider_kind corporate: the FSS feed). It is a financial product,
--     listed under the 금융 field tab like every other one, not a student
--     offer. A government financial product stays under 정책 as before.
-- Both now have no provider tab: provider_category NULL and
-- provider_categories '{}' (category_version still marks the row computed).
-- A company offer filed as a scholarship stays under both 장학금 and 기업혜택.
--
-- The functions keep their signatures (CREATE OR REPLACE) and stay IMMUTABLE,
-- SECURITY INVOKER plain SQL. The version bump makes the worker's bounded
-- refresh (mazimi_worker::categories) recompute every stored row; the rows
-- this rule moves are recomputed here at once (campus offers and company
-- financial products: tens of rows, no table rewrite). Their 기업혜택 chip
-- (browse_category) is cleared by the browse-category sweep, whose tab
-- predicate reads provider_categories.
--
-- No new table, so no new RLS policy.
--
-- Reversible: recreate program_provider_category and
-- program_provider_categories as in 20261023000002 and return 1 from
-- program_category_version(); the worker recomputes every row.

-- No provider tab: a school's own offer, or a company's financial product.
CREATE FUNCTION program_has_no_provider_tab(p_program_type text, p_provider_kind text)
RETURNS boolean LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT COALESCE(p_program_type = 'campus_benefit', false)
        OR (COALESCE(p_program_type = 'financial_product', false)
            AND COALESCE(p_provider_kind = 'corporate', false))
$$;

CREATE OR REPLACE FUNCTION program_provider_category(
    p_program_type text, p_provider_kind text, p_benefit_category text, p_title text
) RETURNS text LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT CASE
        WHEN public.program_has_no_provider_tab(p_program_type, p_provider_kind)
            THEN NULL
        WHEN public.program_is_scholarship(p_program_type, p_benefit_category, p_title)
            THEN 'scholarship'
        WHEN p_program_type = 'corporate_benefit'
          OR COALESCE(p_provider_kind = 'corporate', false)
            THEN 'corporate'
        ELSE 'policy'
    END
$$;

CREATE OR REPLACE FUNCTION program_provider_categories(
    p_program_type text, p_provider_kind text, p_benefit_category text, p_title text
) RETURNS text[] LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$
    SELECT CASE
        WHEN public.program_has_no_provider_tab(p_program_type, p_provider_kind)
            THEN ARRAY[]::text[]
        WHEN public.program_is_scholarship(p_program_type, p_benefit_category, p_title)
         AND (p_program_type = 'corporate_benefit'
              OR COALESCE(p_provider_kind = 'corporate', false))
            THEN ARRAY['scholarship', 'corporate']
        ELSE ARRAY[public.program_provider_category(
            p_program_type, p_provider_kind, p_benefit_category, p_title)]
    END
$$;

CREATE OR REPLACE FUNCTION program_category_version() RETURNS integer
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$ SELECT 2 $$;

-- The rows this version moves, now (the worker's sweep does the rest). All
-- three columns, as the trigger and the worker write them: a row stamped
-- version 2 is never revisited, so one not computed before (field_category
-- NULL) must get its field here too.
UPDATE programs SET
    provider_category = program_provider_category(program_type, provider_kind, benefit_category, title),
    provider_categories = program_provider_categories(program_type, provider_kind, benefit_category, title),
    field_category = program_field_category(program_type, source_type, provider_kind,
        benefit_category, title, summary, raw_payload),
    category_version = program_category_version()
WHERE program_has_no_provider_tab(program_type, provider_kind)
  AND provider_categories IS DISTINCT FROM ARRAY[]::text[];
