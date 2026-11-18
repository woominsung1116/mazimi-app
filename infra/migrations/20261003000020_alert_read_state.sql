-- In-app alert read state: lets the notification centre mark deliveries as
-- read and compute an unread badge count per user. Additive and idempotent;
-- existing deliveries default to unread (read_at IS NULL).
ALTER TABLE alert_deliveries ADD COLUMN IF NOT EXISTS read_at TIMESTAMPTZ NULL;
COMMENT ON COLUMN alert_deliveries.read_at IS
    'When the user marked this in-app delivery as read. NULL = unread.';

-- Partial index for the unread-count query: only in-app-visible, still-unread
-- rows matter for the badge, so the index stays small regardless of history size.
CREATE INDEX IF NOT EXISTS idx_alert_del_user_unread ON alert_deliveries(user_id)
    WHERE in_app_visible AND read_at IS NULL;

-- RLS: alert_deliveries already has RLS enabled (see
-- 20260322000001_rls_policies.sql). The API server writes read_at as the
-- service role, which the existing "alert_deliveries_all_service" FOR ALL
-- policy already covers, so no new policy is needed here.

-- Rollback: dropping the column loses read state and is not an automatic
-- rollback action. Old binaries ignore the extra column safely.
