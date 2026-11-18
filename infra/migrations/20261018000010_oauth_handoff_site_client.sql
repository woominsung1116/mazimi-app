-- The Next.js site finishes its OAuth handoff on its own server route, so it
-- needs a redirect target distinct from the Expo web build ('web').
-- Reversible: DELETE FROM oauth_handoffs WHERE client='site'; then restore
-- CHECK(client IN ('mobile','web')). Rows live at most ten minutes.
ALTER TABLE oauth_handoffs DROP CONSTRAINT oauth_handoffs_client_check;
ALTER TABLE oauth_handoffs ADD CONSTRAINT oauth_handoffs_client_check
    CHECK (client IN ('mobile', 'web', 'site'));
