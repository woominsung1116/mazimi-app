-- Checkbox groups for 서류 작성 도우미 templates (crates/core form_templates
-- `Template.groups`): which checkboxes answer one question and whether one
-- (single) or several (multi) may be ticked. Additive with an empty default,
-- so every existing template reads back byte-identical (an empty list is
-- never serialized). The table keeps its RLS policy (service role only).
-- Rollback: revert the API first, then
--   ALTER TABLE application_form_templates DROP COLUMN groups;
ALTER TABLE application_form_templates
    ADD COLUMN groups JSONB NOT NULL DEFAULT '[]'::jsonb
        CHECK (jsonb_typeof(groups) = 'array' AND jsonb_array_length(groups) <= 100);
