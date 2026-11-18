-- The collector's hourly noise sweep counts, per template file URL, how many
-- programmes carry it. The only other index leads with program_id.
-- Additive. Rollback: DROP INDEX application_resources_template_url;
CREATE INDEX IF NOT EXISTS application_resources_template_url
    ON program_application_resources (url) WHERE kind = 'template';
