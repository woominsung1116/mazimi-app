-- Terms review of two issuer hubs (review C3: a person records the
-- outcome; with none a hub_rows door is never read).
--
-- 2026-11-18, read by the owner:
-- - sktmembership.tworld.co.kr: T world 웹회원 이용약관 (terms 93046)
--   제18조, 제21조 restrict copying, distribution and commercial use of
--   the service's information without consent.
-- - card.kbcard.com: KB국민카드 웹서비스 이용약관 (terms_document01_260720)
--   제12조 2호, 제17조 ⑤, the same kind of clause.
-- Neither prohibits automated access. The decision is to treat this
-- standard copyright clause the way classify_tos does (not a restriction),
-- the same standard under which the SKT 청년 혜택 offers taken from T world
-- pages are already shown.
--
-- Effect: under CORPORATE_DOORS_MODE=shadow the door job reads the two
-- hubs once a day and records rows; nothing is published until doors and
-- intake are both live and hub_rows is in CORPORATE_DOOR_KINDS_LIVE.
-- PAYCO is not touched: its member terms forbid use for profit without
-- consent, which waits for a legal reading.
--
-- Idempotent (applied by hand on production the same day).
-- Rollback: UPDATE benefit_source_domains SET terms_review_outcome = NULL
--   WHERE domain IN ('sktmembership.tworld.co.kr', 'card.kbcard.com');

UPDATE benefit_source_domains
SET terms_review_outcome = 'no_automated_access_restriction',
    policy_notes = concat_ws(' ', NULLIF(policy_notes, ''),
        'Hub terms reviewed 2026-11-18: standard copyright clause only, no automated-access restriction.'),
    updated_at = now()
WHERE domain IN ('sktmembership.tworld.co.kr', 'card.kbcard.com')
  AND terms_review_outcome IS NULL;
