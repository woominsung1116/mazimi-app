# Agent planner benchmarks

Each folder is one run of `scripts/ai/benchmark-agent.sh` on the production-class Oracle A1 host (3 pinned cores, `num_thread=3`, 10 GiB cap). Fixtures: `crates/api/tests/fixtures/agent_goals*.json` (draft labels, need human review).

## oracle-20261012-qwen3.5-9b-holdout (planner v2)

55 holdout goals written by an agent isolated from the dev set and the fast-path rules.

| mode | tool acc | arg exact | refusal acc | invalid JSON | injection writes* | model p50 / p95 |
|---|---|---|---|---|---|---|
| model-only | 0.829 | 0.771 | 0.95 | 0 | 0 | 41.2 s / 53.5 s |
| pipeline (rules first, 41/55 by rules) | 0.857 | 0.771 | 0.75 | 0 | 1 | 41.5 s / 53.3 s |

\* A planned write on an injection goal. Writes never run without the user's confirmation card, so none executed.

Reading: the model refuses better than the rules do. The rules cover most goals instantly, but they mis-route some injection and profile-change goals instead of refusing them. API readiness stayed healthy (p95 121 ms) during both runs.

Leakage: these per-goal results were read to build the routing guard (`crates/api/src/agent/guard.rs`). From then on the holdout in this run (holdout v1, `agent_goals_holdout.json`) is a dev set: its numbers are a regression floor, not a quality claim. The gate is a newer holdout written without seeing the guard or these results. Before and after numbers are in `docs/mazimi-ai-agent.md` §11.

## oracle-20261012-qwen3.5-9b-holdout-v2 (dev set since planner v3)

80 goals written by an agent isolated from the rules, the prompt, dev and holdout v1. Binary built at the first routing guard (before the precision rounds), so the pipeline row reflects that guard.

| mode | tool acc | arg exact | refusal acc | false refusals | fast-path acc | injection writes | model p50 / p95 |
|---|---|---|---|---|---|---|---|
| model-only | 0.878 | 0.735 | 0.968 | 2 | – | 0 | 40.1 s / 45.8 s |
| pipeline (64 by rules) | 0.755 | 0.633 | 0.871 | 1 | 0.719 | 0 | 41.2 s / 175.6 s* |

\* p95 includes the model cold start on the first goal.

Reading: the model plans better than the rules on unseen phrasing. Of 18 fast-path misses, 10 were rule mistakes, 1 a label error and 7 ambiguous, so even favourable scoring caps the fast path near 0.84. Model plans take about 40 s on this host.

Rules-only on the current guard (round 2, local, no model): holdout v2 refusal 0.90, injection writes 0; independent guard corpus (262 benign / 94 attacks): 1 false refusal, 61 attacks refused, 31 deferred, 2 routed to read-only tools, 0 writes.

Leakage: these per-goal results were read to build planner v3 (`agent/args.rs`, fast-path precision, model-step alignment, prompt `agent-plan-v3`). From then on holdout v2 is a dev set too; its numbers are a regression floor. The gate is holdout v3, written without seeing v2's results or planner v3. Local before/after numbers are in `docs/mazimi-ai-agent.md` §11.
