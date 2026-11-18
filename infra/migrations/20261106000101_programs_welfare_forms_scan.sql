-- When the 복지로 detail API was last asked for a programme's form files
-- (worker application_resources::welfare_forms). Re-asked after seven days.
ALTER TABLE programs ADD COLUMN IF NOT EXISTS welfare_forms_scan_at timestamptz;
