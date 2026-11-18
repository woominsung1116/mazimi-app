-- no-transaction
-- Index for the ?corporate_category= filter and the chip counts
-- (20261026000301), built without blocking writes to programs. Partial: only
-- rows on the 기업혜택 tab have a browse_category. Its own file because
-- CREATE INDEX CONCURRENTLY cannot run inside a transaction.
--
-- Reversible: DROP INDEX CONCURRENTLY programs_browse_category;
CREATE INDEX CONCURRENTLY IF NOT EXISTS programs_browse_category
    ON programs (browse_category) WHERE browse_category IS NOT NULL;
