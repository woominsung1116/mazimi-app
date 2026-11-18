# Ollama extraction benchmark 2026-10-08T06:55:26Z

image=ollama/ollama:0.40.1 cpuset=1,2,3 cpus=3 num_thread=3 memory=8g fixture=ai_extraction_sample.json

API readiness p95 before Ollama: 117 ms (gate: during-run p95 <= 1.1x)

| model | digest | precision | recall | evidence validity | failures | p50 ms | p95 ms | peak mem | API p95 ms | health after |
|---|---|---|---|---|---|---|---|---|---|---|
| qwen3.5:4b | d8b0f5e9760c | 0.9285714285714286 | 0.43333333333333335 | 0.8157894736842105 | 0 | 66423 | 98845 | 4.959GiB  | 128 | healthy |
