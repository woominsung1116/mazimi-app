-- Browser redirects carry only a short-lived, one-use code bound to a client secret.
CREATE TABLE oauth_handoffs (
    state_hash TEXT PRIMARY KEY,
    challenge TEXT NOT NULL,
    client TEXT NOT NULL CHECK(client IN ('mobile','web')),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now()+interval '10 minutes',
    callback_used BOOLEAN NOT NULL DEFAULT false,
    exchange_hash TEXT UNIQUE,
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    is_new BOOLEAN NOT NULL DEFAULT false
);
ALTER TABLE oauth_handoffs ENABLE ROW LEVEL SECURITY;
ALTER TABLE oauth_handoffs FORCE ROW LEVEL SECURITY;
CREATE POLICY oauth_handoffs_service ON oauth_handoffs FOR ALL USING(is_service_role()) WITH CHECK(is_service_role());
