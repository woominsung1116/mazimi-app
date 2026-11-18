-- Compile the existing reviewed eligibility rules into explicit hard filters.
--
-- `rule_json` remains the weighted ranking/explanation tree. A program is
-- excluded only when `hard_filter_json` evaluates to `fail`; `unknown` remains
-- visible with a request for the missing profile fields. New rules may keep the
-- hard filter NULL while they are being drafted, but publication code treats a
-- malformed non-NULL hard filter as fail-closed.

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'eligibility_rules'::regclass
          AND conname = 'eligibility_rules_rule_json_object_check'
    ) THEN
        ALTER TABLE eligibility_rules
            ADD CONSTRAINT eligibility_rules_rule_json_object_check
            CHECK (jsonb_typeof(rule_json) = 'object');
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conrelid = 'eligibility_rules'::regclass
          AND conname = 'eligibility_rules_hard_filter_object_check'
    ) THEN
        ALTER TABLE eligibility_rules
            ADD CONSTRAINT eligibility_rules_hard_filter_object_check
            CHECK (
                hard_filter_json IS NULL
                OR jsonb_typeof(hard_filter_json) = 'object'
            );
    END IF;
END
$$;

UPDATE eligibility_rules
SET hard_filter_json = rule_json,
    compiled_at = COALESCE(compiled_at, now()),
    updated_at = now()
WHERE hard_filter_json IS NULL;

COMMENT ON COLUMN eligibility_rules.hard_filter_json IS
    'Reviewed mandatory conditions. fail excludes; unknown stays visible with missing checks.';

