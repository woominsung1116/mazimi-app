-- A resumable preparation workspace for every program category. No credentials,
-- identity numbers or third-party application payloads are stored here.
CREATE TABLE application_preparations (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    program_id UUID NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
    plan_revision TEXT NOT NULL,
    completed_tasks TEXT[] NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, program_id),
    CHECK (cardinality(completed_tasks) <= 100)
);
ALTER TABLE application_preparations ENABLE ROW LEVEL SECURITY;
ALTER TABLE application_preparations FORCE ROW LEVEL SECURITY;
CREATE POLICY application_preparations_service ON application_preparations FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());
