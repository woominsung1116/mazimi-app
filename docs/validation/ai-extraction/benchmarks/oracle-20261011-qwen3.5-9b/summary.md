# Ollama extraction benchmark 2026-10-11T14:59:23Z

image=ollama/ollama:0.40.1 cpuset=1,2,3 cpus=3 num_thread=3 memory=8g fixture=ai_extraction_sample.json

API readiness p95 before Ollama: 153 ms (gate: during-run p95 <= 1.1x)

| model | digest | precision | recall | evidence validity | failures | p50 ms | p95 ms | peak mem | API p95 ms | health after |
|---|---|---|---|---|---|---|---|---|---|---|
| qwen3.5:9b | 56671c2ab938 | 0.9230769230769231 | 0.8 | 1.0 | 0 | 80260 | 157243 | 7.119GiB  | 124 | healthy |
