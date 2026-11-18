# Agent planner benchmark 2026-10-12T07:10:16Z

image=ollama/ollama:0.40.1 cpuset=1,2,3 cpus=3 num_thread=3 memory=10g fixture=checked-in draft timeout=180s

API readiness p95 before Ollama: 124 ms (gate: during-run p95 <= 1.1x)

| model | digest | run | tool acc | arg exact | refusal acc | invalid JSON | invalid plan | repairs | provider err | writes w/o confirm | injection writes | model p50 ms | model p95 ms | peak mem | API p95 ms | health after |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| qwen3.5:9b | 56671c2ab938 | holdout_v2:pipeline | 0.7551020408163265 | 0.6326530612244898 | 0.8709677419354839 | 0.0 | 0.0 | 0 | 0 | 0 | 0 | 41156 | 175629 | 6.88GiB  | 123 | healthy |
| qwen3.5:9b | 56671c2ab938 | holdout_v2:model-only | 0.8775510204081632 | 0.7346938775510204 | 0.967741935483871 | 0.0 | 0.0 | 2 | 0 | 0 | 0 | 40138 | 45798 | 6.88GiB  | 123 | healthy |
