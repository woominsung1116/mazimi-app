-- Agent routing v4: when the model planner cannot plan (busy, budget,
-- outage, invalid output) the rules' answer stands in. Those runs are
-- recorded as `fast_path_fallback`, so metrics separate them from
-- confident fast-path answers.
--
-- Widens a CHECK only; no data changes. Rollback (do not run automatically):
-- revert the API first, then
--   UPDATE assistant_runs SET planner = 'fast_path' WHERE planner = 'fast_path_fallback';
--   ALTER TABLE assistant_runs DROP CONSTRAINT assistant_runs_planner_check;
--   ALTER TABLE assistant_runs ADD CONSTRAINT assistant_runs_planner_check
--     CHECK (planner IN ('fast_path', 'model', 'refused', 'fallback'));
ALTER TABLE assistant_runs DROP CONSTRAINT assistant_runs_planner_check;
ALTER TABLE assistant_runs ADD CONSTRAINT assistant_runs_planner_check
    CHECK (planner IN ('fast_path', 'fast_path_fallback', 'model', 'refused', 'fallback'));
