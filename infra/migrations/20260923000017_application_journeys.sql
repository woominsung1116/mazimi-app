ALTER TABLE application_preparations ADD COLUMN selected_channel TEXT;
ALTER TABLE application_preparations ADD CONSTRAINT preparation_channel CHECK (selected_channel IS NULL OR selected_channel IN ('online','mobile','visit','postal','email','phone','fax','not_required','unknown'));

-- A handoff is a saved preparation, not an executed third-party submission.
CREATE TABLE application_journeys (
    id UUID NOT NULL DEFAULT gen_random_uuid() UNIQUE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    program_id UUID NOT NULL REFERENCES programs(id) ON DELETE RESTRICT,
    plan_revision TEXT NOT NULL,
    channel TEXT NOT NULL CHECK(channel IN ('online','mobile','visit','postal','email','phone','fax','unknown')),
    outcome TEXT NOT NULL DEFAULT 'checking' CHECK(outcome IN ('checking','unknown','reported_applied')),
    snapshot JSONB NOT NULL,
    handoff_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    reported_at TIMESTAMPTZ,
    receipt_reference TEXT CHECK(char_length(receipt_reference)<=120),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(user_id,program_id)
);
ALTER TABLE application_journeys ENABLE ROW LEVEL SECURITY;
ALTER TABLE application_journeys FORCE ROW LEVEL SECURITY;
CREATE POLICY application_journeys_service ON application_journeys FOR ALL USING(is_service_role()) WITH CHECK(is_service_role());
REVOKE ALL ON TABLE application_journeys FROM PUBLIC;
DO $$ DECLARE role_name text; BEGIN
  FOREACH role_name IN ARRAY ARRAY['anon','authenticated'] LOOP
    IF EXISTS(SELECT 1 FROM pg_roles WHERE rolname=role_name) THEN
      EXECUTE format('REVOKE ALL ON TABLE application_journeys FROM %I',role_name);
    END IF;
  END LOOP;
END $$;
