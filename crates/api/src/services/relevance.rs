//! Semantic relevance reranker for benefit search results
//! (`ASSISTANT_SEARCH_RERANK_ENABLED`, default off; needs the intent
//! classifier's e5 encoder).
//!
//! The rules filter a search by field, region, type and keywords, and a row
//! can sit in the right field but on another topic: "청년 적금" listed
//! student-loan and debt rows (finance), "공연·전시 관람" a wartime-abductee
//! row (전시) and a venue rental, "교통비" a traffic-safety class. The
//! reranker scores each candidate by the cosine of the e5 embeddings of the
//! goal's topic words (`query: `) and the programme's title and summary
//! (`passage: `), and then,
//! within each of the rules' topic tiers:
//! - drops a row under [`FLOOR`] when the search has keywords and no
//!   deadline window, so a search with nothing relevant says so instead of
//!   listing off-topic rows (a field-only or closing-soon search is never
//!   emptied by the reranker: its query text is too thin, or a hard
//!   condition already chose the rows);
//! - orders the rest by score, the ones more than [`DELTA`] below the best
//!   of their tier after the others.
//!
//! Thresholds were calibrated on `scripts/ai/relevance/relevance_calib.json`
//! (40 hand-written queries, catalogue rows labelled by per-query patterns;
//! no holdout goal), scored with the query text production embeds. Programme vectors are cached per programme and content
//! hash: a background job embeds the catalogue at start and hourly
//! ([`Relevance::embed_catalogue`], only changed rows again). A search pays
//! one query embedding (off the async runtime, [`Relevance::query_vector`])
//! and dot products; a candidate without a fresh vector is not scored and
//! keeps the rules' order after the scored rows of its tier.
use crate::agent::classifier::IntentClassifier;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

/// Cosine under which a candidate is dropped (only when the search has
/// keywords and no deadline window: see [`Relevance::rerank_with`]).
/// Calibrated with production query text (keywords + field labels): under
/// it lie 7% of candidates and 2% of the relevant ones.
pub const FLOOR: f32 = 0.78;
/// A candidate more than this below its tier's best score is listed after
/// the rest of the tier.
pub const DELTA: f32 = 0.05;
/// Added per topic word the title contains. 0: on the production query
/// text it lowered P@3 (0.825 → 0.808).
pub const LEXICAL: f32 = 0.0;
/// Summary characters embedded with the title.
const SUMMARY_CHARS: usize = 200;

static GLOBAL: OnceLock<Arc<Relevance>> = OnceLock::new();

/// The process's reranker, when the flag is on and the encoder loaded.
pub fn global() -> Option<Arc<Relevance>> {
    GLOBAL.get().cloned()
}

/// Install the reranker (once; later calls are ignored).
pub fn install(relevance: Arc<Relevance>) {
    let _ = GLOBAL.set(relevance);
}

/// `ASSISTANT_SEARCH_RERANK_ENABLED`: opt-in (default **off**), under the
/// AI kill switch. It re-ranks rows the words admitted and never admits
/// one; it was not measured to hold its floor on the 10-31 QA goals.
pub fn enabled_by_env() -> bool {
    enabled_from(
        crate::assistant_provider::models_allowed(),
        std::env::var("ASSISTANT_SEARCH_RERANK_ENABLED")
            .ok()
            .as_deref(),
    )
}

/// [`enabled_by_env`] over its inputs: on only for an explicit true value.
fn enabled_from(models_allowed: bool, flag: Option<&str>) -> bool {
    models_allowed
        && flag.is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            )
        })
}

/// A programme's content hash and its unit vector.
type CachedVector = ([u8; 32], Arc<[f32]>);

pub struct Relevance {
    classifier: Arc<IntentClassifier>,
    /// Programme id -> (content hash, unit vector).
    cache: RwLock<HashMap<Uuid, CachedVector>>,
}

/// One candidate as the reranker sees it.
pub struct Candidate<'a> {
    pub id: Uuid,
    pub title: &'a str,
    pub summary: Option<&'a str>,
    /// The rules' topic tier (lower first); reranking stays within a tier.
    pub tier: u8,
}

/// What a rerank did.
#[derive(Debug, Clone, PartialEq)]
pub struct Reranked {
    /// Indices into the candidates, in the new order (dropped ones left out).
    pub order: Vec<usize>,
    pub dropped: usize,
    pub demoted: usize,
    /// Best score, for logs and the eval.
    pub best: Option<f32>,
}

/// The programme text embedded: title, then the summary's start.
pub fn passage(title: &str, summary: Option<&str>) -> String {
    let summary: String = summary.unwrap_or("").chars().take(SUMMARY_CHARS).collect();
    format!("{title}. {summary}").trim().to_owned()
}

fn content_hash(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Order `scores` (`None` = could not score: kept, after the scored rows of
/// its tier) by the rules above; `may_drop` allows the floor. Pure, for tests.
pub fn order(tiers: &[u8], scores: &[Option<f32>], may_drop: bool) -> Reranked {
    let best = scores
        .iter()
        .flatten()
        .copied()
        .fold(None, |m: Option<f32>, s| Some(m.map_or(s, |m| m.max(s))));
    // Demotion is relative to the best row of the same tier.
    let mut tier_best: HashMap<u8, f32> = HashMap::new();
    for (tier, score) in tiers.iter().zip(scores) {
        if let Some(s) = score.filter(|s| !may_drop || *s >= FLOOR) {
            let e = tier_best.entry(*tier).or_insert(s);
            *e = e.max(s);
        }
    }
    let mut kept: Vec<(u8, u8, f32, usize)> = Vec::new();
    let (mut dropped, mut demoted) = (0, 0);
    for (i, (tier, score)) in tiers.iter().zip(scores).enumerate() {
        match score {
            Some(s) if may_drop && *s < FLOOR => dropped += 1,
            Some(s) => {
                let low = tier_best.get(tier).is_some_and(|b| *s < b - DELTA);
                demoted += usize::from(low);
                kept.push((*tier, u8::from(low), -*s, i));
            }
            None => kept.push((*tier, 2, 0.0, i)),
        }
    }
    kept.sort_by(|a, b| {
        (a.0, a.1)
            .cmp(&(b.0, b.1))
            .then(a.2.total_cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });
    Reranked {
        order: kept.into_iter().map(|k| k.3).collect(),
        dropped,
        demoted,
        best,
    }
}

impl Relevance {
    pub fn new(classifier: Arc<IntentClassifier>) -> Self {
        Self {
            classifier,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Programmes in the cache.
    pub fn cached(&self) -> usize {
        self.cache.read().map(|c| c.len()).unwrap_or(0)
    }

    /// The vector of one programme: cached when its text is unchanged,
    /// embedded (and cached) otherwise. Blocking.
    fn vector(&self, id: Uuid, text: &str) -> Option<Arc<[f32]>> {
        let hash = content_hash(text);
        if let Some((h, v)) = self.cache.read().ok()?.get(&id) {
            if *h == hash {
                return Some(v.clone());
            }
        }
        let v: Arc<[f32]> = self.classifier.embed("passage: ", text).ok()?.into();
        if let Ok(mut cache) = self.cache.write() {
            cache.insert(id, (hash, v.clone()));
        }
        Some(v)
    }

    /// Embed every programme of `rows` whose text changed (one batch of the
    /// job). Blocking; returns how many were embedded. Eviction of
    /// programmes no longer in the catalogue is [`Self::keep_only`], once
    /// per pass over the whole catalogue: a batch is not the catalogue.
    pub fn embed_catalogue(&self, rows: &[(Uuid, String, Option<String>)]) -> usize {
        let mut embedded = 0;
        for (id, title, summary) in rows {
            let text = passage(title, summary.as_deref());
            let fresh = self
                .cache
                .read()
                .ok()
                .and_then(|c| c.get(id).map(|(h, _)| *h == content_hash(&text)))
                .unwrap_or(false);
            if !fresh && self.vector(*id, &text).is_some() {
                embedded += 1;
            }
        }
        embedded
    }

    /// Drop the vectors of programmes no longer in the catalogue (`live`:
    /// every programme id of one catalogue read). Architecture review: the
    /// eviction ran per 32-row batch, so each batch evicted the one before
    /// and the cache never held more than one batch.
    pub fn keep_only(&self, live: &std::collections::HashSet<Uuid>) {
        if let Ok(mut cache) = self.cache.write() {
            cache.retain(|id, _| live.contains(id));
        }
    }

    /// The query vector of a search's topic words. Blocking: call it off
    /// the async runtime. `None` with no words or an encoder failure.
    pub fn query_vector(&self, query_words: &[String]) -> Option<Vec<f32>> {
        let query = query_words.join(" ");
        if query.trim().is_empty() {
            return None;
        }
        self.classifier.embed("query: ", &query).ok()
    }

    /// The cached vector of a programme whose text is unchanged.
    fn cached_vector(&self, id: Uuid, text: &str) -> Option<Arc<[f32]>> {
        let cache = self.cache.read().ok()?;
        let (hash, v) = cache.get(&id)?;
        (*hash == content_hash(text)).then(|| v.clone())
    }

    /// Rerank every candidate against a query vector, from cached programme
    /// vectors only (no embedding: dot products, non-blocking). `may_drop`:
    /// the search has keywords and no deadline window.
    pub fn rerank_with(
        &self,
        query: &[f32],
        query_words: &[String],
        candidates: &[Candidate<'_>],
        may_drop: bool,
    ) -> Reranked {
        let scores: Vec<Option<f32>> = candidates
            .iter()
            .map(|c| {
                let v = self.cached_vector(c.id, &passage(c.title, c.summary))?;
                let bonus = query_words
                    .iter()
                    .filter(|w| !w.is_empty() && c.title.contains(w.as_str()))
                    .count();
                Some(dot(query, &v) + LEXICAL * bonus as f32)
            })
            .collect();
        let tiers: Vec<u8> = candidates.iter().map(|c| c.tier).collect();
        order(&tiers, &scores, may_drop)
    }
}

/// With `ASSISTANT_SEARCH_RERANK_ENABLED`: once the intent classifier has
/// loaded (its encoder is the reranker's), install the reranker and start
/// the catalogue job. A classifier that never loads leaves search as it is.
pub fn spawn_when_ready(pool: sqlx::PgPool, agent: Arc<crate::agent::AgentRuntime>) {
    if !enabled_by_env() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        // The classifier loads in the background (a few seconds).
        for _ in 0..360 {
            if let Some(classifier) = agent.classifier() {
                let relevance = Arc::new(Relevance::new(classifier));
                install(relevance.clone());
                tracing::info!("relevance reranker on");
                spawn_catalogue_job(pool, relevance);
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
        tracing::warn!("relevance reranker off: the intent classifier did not load");
    });
}

fn relevance_cached(r: &Relevance) -> usize {
    r.cached()
}

/// Programmes embedded per blocking call of the catalogue job.
pub const CATALOGUE_BATCH: usize = 32;
/// The pause between catalogue batches: live classification takes the
/// shared encoder pool meanwhile.
pub const CATALOGUE_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

/// The background batch job: embed the catalogue now and every hour (only
/// rows whose title or summary changed are embedded again).
pub fn spawn_catalogue_job(pool: sqlx::PgPool, relevance: Arc<Relevance>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let rows: Vec<(Uuid, String, Option<String>)> =
                match sqlx::query_as("SELECT id, title, summary FROM programs")
                    .fetch_all(&pool)
                    .await
                {
                    Ok(rows) => rows,
                    Err(error) => {
                        tracing::warn!(%error, "relevance: catalogue read failed");
                        continue;
                    }
                };
            let started = std::time::Instant::now();
            let total = rows.len();
            let live: std::collections::HashSet<Uuid> = rows.iter().map(|r| r.0).collect();
            // Small batches with a pause between them (security review F6):
            // the job shares the classifier's encoder pool with live goal
            // classification, which runs between batches.
            let mut embedded = 0;
            for batch in rows.chunks(CATALOGUE_BATCH) {
                let batch = batch.to_vec();
                let job = relevance.clone();
                embedded += tokio::task::spawn_blocking(move || job.embed_catalogue(&batch))
                    .await
                    .unwrap_or(0);
                tokio::time::sleep(CATALOGUE_PAUSE).await;
            }
            relevance.keep_only(&live);
            let cache_ref = relevance.clone();
            tracing::info!(
                total,
                embedded,
                cached = relevance_cached(&cache_ref),
                ms = started.elapsed().as_millis() as u64,
                "relevance: catalogue embedded"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PR #20 review H1: unset (or empty) is off; only an explicit true
    /// turns the reranker on, and the AI kill switch wins.
    #[test]
    fn the_reranker_is_opt_in() {
        assert!(!enabled_from(true, None));
        assert!(!enabled_from(true, Some("")));
        assert!(!enabled_from(true, Some("false")));
        assert!(enabled_from(true, Some("true")));
        assert!(enabled_from(true, Some(" ON ")));
        assert!(!enabled_from(false, Some("true")));
    }

    #[test]
    fn floor_drops_delta_demotes_tiers_hold() {
        // tier 0: 0.90 (best), 0.83 (demoted: > DELTA below), 0.75 (dropped,
        // under FLOOR);
        // tier 1: 0.95 stays after every tier-0 row; unscored row kept last
        // of its tier.
        let tiers = [0, 0, 0, 1, 0];
        let scores = [Some(0.83), Some(0.90), Some(0.75), Some(0.95), None];
        let r = order(&tiers, &scores, true);
        assert_eq!(r.order, [1, 0, 4, 3]);
        assert_eq!(
            (r.dropped, r.demoted),
            (1, 1),
            "relative to its own tier's best"
        );
        assert_eq!(r.best, Some(0.95));
    }

    #[test]
    fn nothing_relevant_lists_nothing() {
        let r = order(&[0, 0], &[Some(0.70), Some(0.75)], true);
        assert!(r.order.is_empty());
        assert_eq!(r.dropped, 2);
        // A field-only or closing-soon search keeps its rows, reordered.
        let kept = order(&[0, 0], &[Some(0.70), Some(0.75)], false);
        assert_eq!((kept.order, kept.dropped), (vec![1, 0], 0));
    }

    #[test]
    fn passage_is_title_and_summary_start() {
        let long = "가".repeat(500);
        let p = passage("청년 적금", Some(&long));
        assert!(p.starts_with("청년 적금. "));
        assert_eq!(
            p.chars().count(),
            "청년 적금. ".chars().count() + SUMMARY_CHARS
        );
        assert_eq!(passage("제목", None), "제목.");
    }
}
