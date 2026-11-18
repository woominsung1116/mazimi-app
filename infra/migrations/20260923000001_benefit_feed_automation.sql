-- Publisher-authorized structured feeds extend the existing program/brand API.
-- URLs are attributes, never identities. Missing items in a delta do not expire.
CREATE TABLE benefit_feeds (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    domain_id UUID NOT NULL REFERENCES benefit_source_domains(id),
    brand_id UUID NOT NULL REFERENCES brands(id),
    authority_reference TEXT NOT NULL CHECK (char_length(authority_reference) BETWEEN 10 AND 2000),
    authorized_until TIMESTAMPTZ NOT NULL,
    token_hash TEXT NOT NULL CHECK (length(token_hash) = 64),
    is_active BOOLEAN NOT NULL DEFAULT true,
    last_sequence BIGINT NOT NULL DEFAULT 0,
    last_received_at TIMESTAMPTZ,
    created_by UUID NOT NULL REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE benefit_feed_batches (
    feed_id UUID NOT NULL REFERENCES benefit_feeds(id),
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    payload_hash TEXT NOT NULL,
    item_count INT NOT NULL CHECK (item_count BETWEEN 1 AND 100),
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (feed_id, sequence)
);
CREATE TABLE benefit_feed_items (
    feed_id UUID NOT NULL REFERENCES benefit_feeds(id),
    external_id TEXT NOT NULL CHECK (char_length(external_id) BETWEEN 1 AND 200),
    program_id UUID NOT NULL UNIQUE REFERENCES programs(id),
    last_sequence BIGINT NOT NULL,
    publisher_enabled BOOLEAN NOT NULL,
    -- An operator can suspend an offer without the next publisher batch undoing it.
    publication_suspended BOOLEAN NOT NULL DEFAULT false,
    valid_until TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (feed_id, external_id)
);
CREATE INDEX benefit_feed_items_expiry ON benefit_feed_items(valid_until);
ALTER TABLE benefit_feeds ENABLE ROW LEVEL SECURITY;
ALTER TABLE benefit_feeds FORCE ROW LEVEL SECURITY;
CREATE POLICY benefit_feeds_service ON benefit_feeds FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());
ALTER TABLE benefit_feed_batches ENABLE ROW LEVEL SECURITY;
ALTER TABLE benefit_feed_batches FORCE ROW LEVEL SECURITY;
CREATE POLICY benefit_feed_batches_service ON benefit_feed_batches FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());
ALTER TABLE benefit_feed_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE benefit_feed_items FORCE ROW LEVEL SECURITY;
CREATE POLICY benefit_feed_items_service ON benefit_feed_items FOR ALL USING (is_service_role()) WITH CHECK (is_service_role());

-- Runs independently of incoming batches, including when a publisher disappears.
CREATE FUNCTION expire_benefit_feeds() RETURNS BIGINT LANGUAGE plpgsql AS $$
DECLARE changed BIGINT;
BEGIN
    UPDATE programs p SET is_active = false,
        program_status = CASE WHEN i.valid_until <= now() THEN 'closed' ELSE 'review_required' END,
        source_health = CASE WHEN i.valid_until <= now() THEN 'retired' ELSE 'review_due' END,
        updated_at = now()
    FROM benefit_feed_items i JOIN benefit_feeds f ON f.id = i.feed_id
    WHERE p.id = i.program_id AND p.is_active = true
      AND (i.valid_until <= now() OR f.authorized_until <= now() OR NOT f.is_active
           OR i.publication_suspended OR NOT i.publisher_enabled);
    GET DIAGNOSTICS changed = ROW_COUNT;
    RETURN changed;
END $$;
REVOKE ALL ON FUNCTION expire_benefit_feeds() FROM PUBLIC;
