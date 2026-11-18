-- Bind authorization state and one-use handoffs to the initiating provider.
ALTER TABLE oauth_handoffs ADD COLUMN provider TEXT NOT NULL DEFAULT 'kakao'
    CHECK (provider IN ('kakao','google'));
-- A short-lived server-side PKCE verifier, never returned to a browser/client.
ALTER TABLE oauth_handoffs ADD COLUMN provider_verifier TEXT;
