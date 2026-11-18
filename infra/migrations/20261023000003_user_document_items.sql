-- User-owned document checklist entries (v1 feedback F6.2b, "직접 추가").
--
-- Only the item name and its status are stored. Document files stay on the
-- device; nothing here holds file contents, numbers or identity data.
-- program_id attaches an item to a programme's checklist (NULL = general).
-- A programme removed from the catalogue keeps the user's item (SET NULL):
-- user data is never deleted by catalogue maintenance. Account deletion
-- removes the user's items with the account (CASCADE), as for every other
-- user table.
--
-- Reversible: DROP TABLE user_document_items;
CREATE TABLE user_document_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    program_id UUID REFERENCES programs(id) ON DELETE SET NULL,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 60),
    status TEXT NOT NULL DEFAULT 'todo' CHECK (status IN ('todo', 'done')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX user_document_items_owner ON user_document_items (user_id, program_id, created_at);

ALTER TABLE user_document_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE user_document_items FORCE ROW LEVEL SECURITY;
CREATE POLICY user_document_items_service ON user_document_items
    FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());
CREATE POLICY user_document_items_own ON user_document_items
    FOR ALL USING (user_id = app_user_id()) WITH CHECK (user_id = app_user_id());
REVOKE ALL ON TABLE user_document_items FROM PUBLIC;
DO $grants$
DECLARE client_role text;
BEGIN
  FOREACH client_role IN ARRAY ARRAY['anon','authenticated'] LOOP
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = client_role) THEN
      EXECUTE format('REVOKE ALL ON user_document_items FROM %I', client_role);
    END IF;
  END LOOP;
END $grants$;
