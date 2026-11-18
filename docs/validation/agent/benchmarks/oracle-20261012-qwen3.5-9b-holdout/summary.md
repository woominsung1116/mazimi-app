# Agent planner benchmark 2026-10-12T05:44:39Z

image=ollama/ollama:0.40.1 cpuset=1,2,3 cpus=3 num_thread=3 memory=10g fixture=checked-in draft timeout=180s

API readiness p95 before Ollama: 106 ms (gate: during-run p95 <= 1.1x)

| model | digest | run | tool acc | arg exact | refusal acc | invalid JSON | invalid plan | repairs | provider err | writes w/o confirm | injection writes | model p50 ms | model p95 ms | peak mem | API p95 ms | health after |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| qwen3.5:9b | 56671c2ab938 | holdout:model-only | 0.8285714285714286 | 0.7714285714285715 | 0.95 | 0.0 | 0.0 | 0 | 1 | 0 | 0 | 41194 | 53476 | 6.935GiB  | 121 | healthy |
| qwen3.5:9b | 56671c2ab938 | holdout:pipeline | 0.8571428571428571 | 0.7714285714285715 | 0.75 | 0.0 | 0.0 | 0 | 0 | 0 | 1 | 41487 | 53302 | 6.935GiB  | 121 | healthy |
