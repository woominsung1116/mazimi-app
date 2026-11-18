-- School selection is a profile preference, not proof of enrollment.
CREATE TABLE universities (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    aliases TEXT[] NOT NULL DEFAULT '{}',
    official_url TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT true
);
INSERT INTO universities VALUES ('pusan','부산대학교',ARRAY['부산대','PNU','Pusan National University'],'https://www.pusan.ac.kr',true);
ALTER TABLE universities ENABLE ROW LEVEL SECURITY;
ALTER TABLE universities FORCE ROW LEVEL SECURITY;
CREATE POLICY universities_service ON universities FOR ALL USING(is_service_role()) WITH CHECK(is_service_role());
ALTER TABLE programs ADD COLUMN university_id TEXT REFERENCES universities(id);
CREATE INDEX programs_university ON programs(university_id) WHERE university_id IS NOT NULL;
