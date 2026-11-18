//! A-09 운영자 AI 데이터 큐레이터: structured eligibility facts from public
//! program text, each with an exact evidence span, stored as candidates that
//! only an admin approval can apply. Contract: `docs/mazimi-ai-extraction.md`.
//!
//! Flow: rule normalizer (pipeline) → [`source`] masked text + hash →
//! [`provider`] (self-hosted Ollama, schema-constrained, temperature 0) →
//! [`validate`] (exact span, ranges, canonical codes, textual guards,
//! confidence) → [`job`] cache + per-fact candidates. The model is off by
//! default (`EXTRACTION_AI_ENABLED`) and never sees user data.
pub mod apply;
pub mod auto;
pub mod crosscheck;
pub mod eval;
pub mod facts;
pub mod job;
pub mod provider;
pub mod source;
pub mod validate;

/// Bump when the prompt text or field guide changes.
pub const PROMPT_VERSION: &str = "program-facts-v2";
/// Bump when [`facts`] changes shape.
pub const SCHEMA_VERSION: i32 = 1;
/// Asynchronous analysis model from the technical spec (§5.1): Qwen3.5 9B
/// Q4_K_M, pending the Oracle benchmark (`scripts/ai/benchmark-ollama.sh`).
pub const DEFAULT_MODEL: &str = "qwen3.5:9b-q4_K_M";
pub const DEFAULT_URL: &str = "http://ollama:11434";
