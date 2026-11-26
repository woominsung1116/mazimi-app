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

## oracle-20261118-planner-q17-v2-holdout-v3 (gate, production planner)

128 holdout v3 goals (the gate set). Model: the production fine-tuned planner `mazimi-planner-q17-v2` (digest `8a7e88aaa0c6`), served by the running `mazimi-planner-1` container (Ollama 0.40.3, cpuset 2,3). `agent-eval` built from commit `75a7d29` and run in the `mazimi-api` image on the `mazimi_planner` network (cpuset 0,1): `--planner-prompt tuned --num-thread 2 --timeout-seconds 12`, pipeline with `--classifier` (classifier `e5s-char-jamo-lr-v3`, threshold 0.7). Not `scripts/ai/benchmark-agent.sh`: that script starts its own Ollama for qwen3.5 models.

| mode | tool acc | arg exact | refusal acc | false refusals | fast-path acc | invalid JSON | writes w/o confirm | injection writes (non-bypass) | model p50 / p95 |
|---|---|---|---|---|---|---|---|---|---|
| pipeline (82 fast path, 17 classifier, 29 model) | 0.889 | 0.838 | 0.897 | 0 | 0.915 | 0 | 0 | 1 (0) | 2.5 s / 7.7 s |
| model-only | 0.889 | 0.828 | 0.897 | 1 | – | 0 | 0 | 1 (0) | 2.0 s / 3.3 s |

Gates: arg exact (>= 0.80), invalid JSON, writes without confirmation and p95 latency (<= 12 s) pass. Tool accuracy (>= 0.90) and refusal accuracy (>= 0.95) fail. The injection write is a skip-confirmation goal planned as a confirmation card (routing v4 decision), so `injection_writes_non_bypass` is 0. Weakest categories: deadlines (pipeline tool 0.43), injection refusal 0.75, multi 0.75.

The per-goal texts were not read, so holdout v3 stays a gate set.

These numbers score the planner's output, not the run. `agent-eval` then loaded no understanding model, while production loads it by default and lets the intent layer replace the planner's writes with its own cards, so the run's cards were not measured. See the next section.

## 2026-11-20 replay and mock runs (no model calls, local WSL)

Not a server run. The 11-18 model-only replies were turned into a replay file (`{masked goal \u0001 has_program \u0001 selected: reply}`) and scored with `agent-eval --provider replay`; `--provider mock` stands in for a perfect planner. Replaying the 11-18 model-only replies reproduces its numbers exactly (tool 0.889, args 0.828, refusal 0.897). Artefacts: classifier `e5s-char-jamo-lr-v3`, understanding `e5s-char-jamo-understand-v4`, e5 encoder at the pinned revision. Card accuracy: among goals whose label asks for a write, the share whose cards after `observe::expand` equal the expected writes, direction included (alert on/off, save/unsave, checklist done/undone, status).

holdout v3, pipeline, 11-18 replies:

| run | tool acc | arg exact | card acc | unasked cards |
|---|---|---|---|---|
| lexical rules (the 11-18 eval condition) | 0.899 | 0.848 | 0.500 | 2 |
| understanding model (production before the fix) | 0.899 | 0.838 | 0.559 | 2 |
| understanding model + rule agreement (production after `49c1fa6`) | 0.899 | 0.838 | 0.618 | 2 |
| same, `--saved 3` | 0.899 | 0.838 | 0.676 | 3 |

Before the fix, card accuracy by route was fast path 1.0, classifier 0.50, model 0.19. The pipeline tool 0.899 is one goal above the 11-18 pipeline record (0.889): replay answers model-routed goals with the model-only replies.

Intent layer with a perfect planner (`--provider mock --saved 3`, dev sets):

| set | lexical rules | understanding model | + rule agreement |
|---|---|---|---|
| dev | 0.78 | 0.68 | 0.84 |
| holdout v1 | 0.36 | 0.50 | 0.64 |
| holdout v2 | 0.73 | 0.73 | 0.87 |

Card precision on the hand-labelled `agent_card_requests.json` (1,034 rows, all on screen, saved 3, `acceptance_probe`): rows with an unasked card 34 before and after the rule agreement.

Tried and not kept (chosen on the dev sets and the probe, not on holdout v3):

- Value bars scaled 0.75 to 0.25: no wrong-direction card on the dev sets, but only one more card each on holdout v1 and v2 and no change on the probe.
- Alert and save with no allowed target sent to the programme on screen: one more card on dev and on the probe, and one more probe row with an unasked card (35).

Leakage: holdout v3 aggregates were read for several variants (per category, per route, miss counts by write type and act, and the category and tool of unasked cards). No per-goal text was read and no variant was chosen on v3, so it stays the gate set.

## 2026-11-26 dev card accuracy after the journey relabel (no model calls, local WSL)

Six dev 준비 journey rows (prepare-1, prepare-2, prepare-3, dev2-prepare-1, dev2-prepare-2, dev3-journey-1) were relabelled to check_eligibility + get_preparation: since round 12 the journey's alert is the answer's button, not a card. They now ask for no write, so dev has 44 write goals instead of 50. Same conditions as the 11-20 mock table (`--provider mock --saved 3`, same artefacts); the understanding-only column was run with `intent.rs` from before `agree_with_rules`. Holdout v1 and v2 reproduce the 11-20 numbers; holdout v1 keeps its frozen `ho-024` label.

| set | lexical rules | understanding model | + rule agreement | unasked cards (lexical / understanding) |
|---|---|---|---|---|
| dev | 0.89 | 0.77 | 0.95 | 0 / 1 |

The relabelled rows were misses in every condition, so only the denominator changed (39, 34 and 42 hits). The unasked card is `open_official_application` on `dev4-official-1`, a check-the-official-page goal, and appears only with the understanding model. With and without `--classifier` the card numbers are the same.
