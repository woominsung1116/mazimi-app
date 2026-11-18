-- P2 intake re-adopts a held evidence_auto source whose every fact's quote
-- is gone from its page (`held_source`, corp-expand 2026-10-31): the intake
-- offer row records that kind next to `new_offer` and `human_source`.
-- Widens a CHECK only; no row changes.
--
-- Rollback: DELETE FROM corporate_intake_offers WHERE kind = 'held_source';
-- then restore the constraint with ('new_offer', 'human_source').

ALTER TABLE corporate_intake_offers DROP CONSTRAINT corporate_intake_offers_kind_check;
ALTER TABLE corporate_intake_offers ADD CONSTRAINT corporate_intake_offers_kind_check
    CHECK (kind IN ('new_offer', 'human_source', 'held_source'));
