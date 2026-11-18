//! The write-understanding model: one learned reading of a masked goal for
//! every write type the agent can propose (architect design, "one local
//! multi-head model"). Deterministic and local; nothing generative decides
//! a write.
//!
//! Features, shared by every head: one multilingual-e5-small pass of
//! `query: <goal>` (standardized, weighted), char 1-4 and jamo 3-6 TF-IDF of
//! the goal, char 1-5 of its last 16 characters, and char 1-4 of its last
//! two words (the final clause: a correction or retraction decides the
//! act). Heads are linear with a softmax temperature fitted on dev:
//! - **act**, per write type: none, request, mention, question, report,
//!   retract;
//! - **value**: alert on/off, save/unsave, checklist done/undo, the status;
//! - **target**: on_screen, saved, selected, none.
//!
//! [`Understanding::writes`] applies the artefact's card bar per type (the
//! lowest write probability with precision >= 0.90 on dev). The policy in
//! `agent::intent` decides the rest (screen, saved count, arguments).
//!
//! Artefact: `understanding.json` from `scripts/ai/understanding/train.py`
//! (in git under `crates/api/assets/understanding/`), sha256-pinned below,
//! next to the routing classifier's encoder files.
use super::{
    char_wb_range, decode_f32, encoder, jamo, read_pinned, shared_encoder, softmax, tfidf,
    ArtefactPaths, LoadError, MODEL_SHA256, TOKENIZER_SHA256,
};
use serde::Deserialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

/// `understanding.json` as committed (retraining changes this pin).
pub const UNDERSTANDING_SHA256: &str =
    "ba2d18cf09617b9e8963082804519b4c7f65033668693894ea50d53636a596b4";
const FORMAT: &str = "mazimi-understanding/4";
/// The write types, in the artefact's order.
pub const TYPES: [&str; 8] = [
    "alert",
    "save",
    "status",
    "checklist",
    "calendar",
    "profile",
    "form",
    "page",
];
/// The acts, in the artefact's order.
pub const ACTS: [&str; 6] = [
    "none", "request", "mention", "question", "report", "retract",
];
/// The targets, in the artefact's order.
pub const TARGETS: [&str; 4] = ["on_screen", "saved", "selected", "none"];

#[derive(Deserialize)]
struct EncoderSpec {
    model_sha256: String,
    tokenizer_sha256: String,
    prefix: String,
}

#[derive(Deserialize)]
struct Features {
    char: [usize; 2],
    jamo: [usize; 2],
    tail: [usize; 2],
    tail_chars: usize,
    last: [usize; 2],
    last_words: usize,
    embedding_weight: f64,
}

#[derive(Deserialize)]
struct HeadSpec {
    kind: String,
    #[serde(rename = "type")]
    write: String,
    labels: Vec<String>,
    temperature: f64,
    coef_f32_b64: String,
    intercept: Vec<f64>,
}

#[derive(Deserialize)]
struct Artefact {
    format: String,
    version: String,
    encoder: EncoderSpec,
    types: Vec<String>,
    acts: Vec<String>,
    targets: Vec<String>,
    writes_for: HashMap<String, Vec<String>>,
    features: Features,
    card_bar: HashMap<String, f64>,
    value_bar: HashMap<String, f64>,
    char_ngrams: Vec<String>,
    char_idf: Vec<f64>,
    jamo_ngrams: Vec<String>,
    jamo_idf: Vec<f64>,
    tail_ngrams: Vec<String>,
    tail_idf: Vec<f64>,
    last_ngrams: Vec<String>,
    last_idf: Vec<f64>,
    emb_mean: Vec<f64>,
    emb_scale: Vec<f64>,
    heads: Vec<HeadSpec>,
}

struct Vocab {
    ngrams: HashMap<String, usize>,
    idf: Vec<f64>,
    range: (usize, usize),
}

impl Vocab {
    fn new(ngrams: Vec<String>, idf: Vec<f64>, range: [usize; 2]) -> Result<Self, LoadError> {
        let n = ngrams.len();
        let map: HashMap<String, usize> = ngrams
            .into_iter()
            .enumerate()
            .map(|(i, g)| (g, i))
            .collect();
        if map.len() != n || idf.len() != n || !(1..=range[1]).contains(&range[0]) {
            return Err(LoadError::Invalid("vocabulary".into()));
        }
        Ok(Self {
            ngrams: map,
            idf,
            range: (range[0], range[1]),
        })
    }

    fn features(&self, text: &str, offset: usize) -> Vec<(usize, f64)> {
        tfidf(
            char_wb_range(text, self.range.0, self.range.1),
            &self.ngrams,
            &self.idf,
            offset,
        )
    }
}

struct Head {
    labels: Vec<String>,
    temperature: f64,
    /// `[labels, width]`, row-major.
    coef: Vec<f32>,
    intercept: Vec<f64>,
}

/// The model's reading of one goal.
#[derive(Debug, Clone, PartialEq)]
pub struct Understood {
    /// Per write type (in [`TYPES`] order): act probabilities in [`ACTS`] order.
    pub acts: Vec<Vec<f64>>,
    /// Per write type with a value head: label → probability.
    pub values: HashMap<String, Vec<(String, f64)>>,
    /// Target probabilities in [`TARGETS`] order.
    pub target: Vec<f64>,
}

/// The paths the model needs: its artefact and the shared encoder.
#[derive(Debug, Clone)]
pub struct UnderstandingPaths {
    pub model: PathBuf,
    pub encoder: ArtefactPaths,
}

impl UnderstandingPaths {
    /// `understanding.json` next to the encoder files (the API image).
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            model: dir.join("understanding.json"),
            encoder: ArtefactPaths::in_dir(dir),
        }
    }
}

pub struct Understanding {
    version: String,
    char: Vocab,
    jamo: Vocab,
    tail: Vocab,
    tail_chars: usize,
    last: Vocab,
    last_words: usize,
    emb_mean: Vec<f64>,
    emb_scale: Vec<f64>,
    emb_weight: f64,
    heads: HashMap<(String, String), Head>,
    writes_for: HashMap<String, Vec<usize>>,
    card_bar: HashMap<String, f64>,
    value_bar: HashMap<String, f64>,
    encoder: Arc<encoder::Encoder>,
    pool: rayon::ThreadPool,
    threads: usize,
}

impl Understanding {
    /// Load and verify the model and the (shared) encoder. Slow the first
    /// time the encoder loads: call off the async runtime.
    pub fn load(paths: &UnderstandingPaths, threads: usize) -> Result<Self, LoadError> {
        Self::load_pinned(paths, threads, UNDERSTANDING_SHA256)
    }

    pub fn load_pinned(
        paths: &UnderstandingPaths,
        threads: usize,
        pin: &str,
    ) -> Result<Self, LoadError> {
        let invalid = |what: &str| LoadError::Invalid(what.to_owned());
        let raw = read_pinned(&paths.model, pin, "understanding.json")?;
        let art: Artefact =
            serde_json::from_slice(&raw).map_err(|e| LoadError::Invalid(e.to_string()))?;
        if art.format != FORMAT
            || art.encoder.model_sha256 != MODEL_SHA256
            || art.encoder.tokenizer_sha256 != TOKENIZER_SHA256
            || art.encoder.prefix != "query: "
            || art.types != TYPES
            || art.acts != ACTS
            || art.targets != TARGETS
        {
            return Err(invalid("format, encoder pins or label sets"));
        }
        let char = Vocab::new(art.char_ngrams, art.char_idf, art.features.char)?;
        let jamo = Vocab::new(art.jamo_ngrams, art.jamo_idf, art.features.jamo)?;
        let tail = Vocab::new(art.tail_ngrams, art.tail_idf, art.features.tail)?;
        let last = Vocab::new(art.last_ngrams, art.last_idf, art.features.last)?;
        if art.features.last_words == 0 {
            return Err(invalid("last_words"));
        }
        let width =
            char.idf.len() + jamo.idf.len() + tail.idf.len() + last.idf.len() + encoder::HIDDEN;
        if art.emb_mean.len() != encoder::HIDDEN
            || art.emb_scale.len() != encoder::HIDDEN
            || art.emb_scale.iter().any(|s| *s == 0.0 || !s.is_finite())
        {
            return Err(invalid("embedding scaler"));
        }
        let mut heads = HashMap::new();
        for h in art.heads {
            let coef = decode_f32(&h.coef_f32_b64).ok_or_else(|| invalid("coefficients"))?;
            if h.labels.len() < 2
                || coef.len() != h.labels.len() * width
                || h.intercept.len() != h.labels.len()
                || !(h.temperature > 0.0 && h.temperature.is_finite())
            {
                return Err(invalid("head shape"));
            }
            heads.insert(
                (h.kind, h.write),
                Head {
                    labels: h.labels,
                    temperature: h.temperature,
                    coef,
                    intercept: h.intercept,
                },
            );
        }
        if TYPES
            .iter()
            .any(|t| !heads.contains_key(&("act".into(), (*t).into())))
            || !heads.contains_key(&("target".into(), String::new()))
        {
            return Err(invalid("missing head"));
        }
        let writes_for = TYPES
            .iter()
            .map(|t| {
                let acts = art.writes_for.get(*t).cloned().unwrap_or_default();
                let idx = acts
                    .iter()
                    .filter_map(|a| ACTS.iter().position(|x| x == a))
                    .collect();
                ((*t).to_owned(), idx)
            })
            .collect();
        let encoder = shared_encoder(&paths.encoder)?;
        let threads = threads.clamp(1, 16);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("understanding-{i}"))
            .build()
            .map_err(|e| LoadError::Invalid(e.to_string()))?;
        Ok(Self {
            version: art.version,
            char,
            jamo,
            tail,
            tail_chars: art.features.tail_chars,
            last,
            last_words: art.features.last_words,
            emb_mean: art.emb_mean,
            emb_scale: art.emb_scale,
            emb_weight: art.features.embedding_weight,
            heads,
            writes_for,
            card_bar: art.card_bar,
            value_bar: art.value_bar,
            encoder,
            pool,
            threads,
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// The card bar of a write type (1.0 when the artefact has none).
    pub fn card_bar(&self, write: &str) -> f64 {
        self.card_bar.get(write).copied().unwrap_or(1.0)
    }

    /// The value bar of a polarity type (alert, save, checklist): the
    /// lowest margin between its two directions at which the value is right
    /// on dev (0.0 for a type without one).
    pub fn value_bar(&self, write: &str) -> f64 {
        self.value_bar.get(write).copied().unwrap_or(0.0)
    }

    /// The margin between the two most likely values of a type (1.0 for a
    /// type without a value head: nothing to confuse).
    pub fn value_margin(understood: &Understood, write: &str) -> f64 {
        let Some(values) = understood.values.get(write) else {
            return 1.0;
        };
        let mut p: Vec<f64> = values.iter().map(|(_, v)| *v).collect();
        p.sort_by(|a, b| b.total_cmp(a));
        p[0] - p.get(1).copied().unwrap_or(0.0)
    }

    /// Read one masked goal (blocking, tens of milliseconds).
    pub fn read(&self, goal: &str) -> Result<Understood, String> {
        let embedding = self
            .pool
            .install(|| self.encoder.embed(goal, self.threads))?;
        let lower = goal.to_lowercase();
        let chars: Vec<char> = goal.chars().collect();
        let tail: String = chars[chars.len().saturating_sub(self.tail_chars)..]
            .iter()
            .collect();
        let mut sparse = self.char.features(&lower, 0);
        let mut offset = self.char.idf.len();
        sparse.extend(self.jamo.features(&jamo(goal), offset));
        offset += self.jamo.idf.len();
        sparse.extend(self.tail.features(&tail.to_lowercase(), offset));
        offset += self.tail.idf.len();
        sparse.extend(
            self.last
                .features(&last_words(&lower, self.last_words), offset),
        );
        offset += self.last.idf.len();
        let width = offset + encoder::HIDDEN;
        let dense: Vec<f64> = embedding
            .iter()
            .enumerate()
            .map(|(j, e)| (f64::from(*e) - self.emb_mean[j]) / self.emb_scale[j] * self.emb_weight)
            .collect();
        let score = |head: &Head| -> Vec<f64> {
            let logits = head
                .intercept
                .iter()
                .enumerate()
                .map(|(k, bias)| {
                    let row = &head.coef[k * width..(k + 1) * width];
                    let s: f64 = sparse.iter().map(|(i, v)| f64::from(row[*i]) * v).sum();
                    let d: f64 = dense
                        .iter()
                        .enumerate()
                        .map(|(j, z)| f64::from(row[offset + j]) * z)
                        .sum();
                    (bias + s + d) / head.temperature
                })
                .collect();
            softmax(logits)
        };
        let acts = TYPES
            .iter()
            .map(|t| score(&self.heads[&("act".to_owned(), (*t).to_owned())]))
            .collect();
        let mut values = HashMap::new();
        for t in TYPES {
            if let Some(head) = self.heads.get(&("value".to_owned(), t.to_owned())) {
                let p = score(head);
                values.insert(t.to_owned(), head.labels.iter().cloned().zip(p).collect());
            }
        }
        let target = score(&self.heads[&("target".to_owned(), String::new())]);
        Ok(Understood {
            acts,
            values,
            target,
        })
    }

    /// The probability that the goal asks for a write of this type: the
    /// mass of the acts that produce it (a request; for status and
    /// checklist also a report).
    pub fn write_mass(&self, understood: &Understood, write: &str) -> f64 {
        let Some(i) = TYPES.iter().position(|t| *t == write) else {
            return 0.0;
        };
        self.writes_for
            .get(write)
            .map_or(0.0, |idx| idx.iter().map(|a| understood.acts[i][*a]).sum())
    }

    /// Whether the goal asks for a card of this type: [`Self::write_mass`]
    /// at or above the type's card bar.
    pub fn writes(&self, understood: &Understood, write: &str) -> bool {
        self.write_mass(understood, write) >= self.card_bar(write)
    }

    /// Encoder token ids (parity tests).
    pub fn token_ids(&self, goal: &str) -> Result<Vec<u32>, String> {
        self.encoder.ids(goal)
    }
}

/// The last `n` whitespace-separated words, joined by one space (the
/// trainer's `" ".join(goal.split()[-n:])`).
fn last_words(text: &str, n: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    words[words.len().saturating_sub(n)..].join(" ")
}

#[cfg(test)]
mod tests {
    /// Writes `scripts/ai/understanding/data/guard_barred.json`: the row
    /// goals `guard::bars_writes` bars. The model never reads them (the
    /// policy bars them first), so the trainer leaves them out of the card
    /// bar's calibration and the report. Run after the rows change:
    /// `UNDERSTANDING_EXPORT_BARRED=1 cargo test -p api --lib export_guard_barred -- --ignored`.
    #[test]
    #[ignore = "writes the trainer's guard list"]
    fn export_guard_barred() {
        if std::env::var("UNDERSTANDING_EXPORT_BARRED").is_err() {
            return;
        }
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/ai/understanding/data"
        );
        let rows: serde_json::Value =
            serde_json::from_slice(&std::fs::read(format!("{dir}/intent_rows.json")).unwrap())
                .unwrap();
        let mut barred: Vec<String> = rows["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["goal"].as_str())
            .filter(|g| crate::agent::guard::bars_writes(g))
            .map(str::to_owned)
            .collect();
        barred.sort();
        barred.dedup();
        let out = serde_json::json!({"source": "guard::bars_writes", "goals": barred});
        std::fs::write(
            format!("{dir}/guard_barred.json"),
            serde_json::to_string_pretty(&out).unwrap() + "\n",
        )
        .unwrap();
    }

    #[test]
    fn the_value_margin_is_the_gap_between_the_two_directions() {
        let understood = super::Understood {
            acts: Vec::new(),
            values: [
                (
                    "save".to_owned(),
                    vec![("save".to_owned(), 0.35), ("unsave".to_owned(), 0.65)],
                ),
                (
                    "alert".to_owned(),
                    vec![("on".to_owned(), 0.97), ("off".to_owned(), 0.03)],
                ),
            ]
            .into_iter()
            .collect(),
            target: Vec::new(),
        };
        let m = |t| super::Understanding::value_margin(&understood, t);
        assert!((m("save") - 0.30).abs() < 1e-9);
        assert!((m("alert") - 0.94).abs() < 1e-9);
        // No value head: nothing to confuse.
        assert_eq!(m("calendar"), 1.0);
    }

    #[test]
    fn last_words_is_the_final_clause() {
        assert_eq!(
            super::last_words("  이거   알림 꺼줘\t아니  켜줘 ", 2),
            "아니 켜줘"
        );
        assert_eq!(super::last_words("알림", 2), "알림");
        assert_eq!(super::last_words("", 2), "");
    }

    use super::*;

    fn committed(dir: &Path) -> UnderstandingPaths {
        UnderstandingPaths {
            model: PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/understanding/understanding.json"
            )),
            encoder: ArtefactPaths::in_dir(dir),
        }
    }

    #[test]
    fn committed_artefact_matches_its_pin() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/understanding/understanding.json"
        ))
        .unwrap();
        assert_eq!(super::super::sha256_hex(&bytes), UNDERSTANDING_SHA256);
    }

    #[test]
    fn a_missing_or_tampered_artefact_loads_no_model() {
        let dir = std::env::temp_dir().join(format!("understanding-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = UnderstandingPaths::in_dir(&dir);
        assert!(matches!(
            Understanding::load(&paths, 1),
            Err(LoadError::Missing(_))
        ));
        std::fs::write(&paths.model, b"{}").unwrap();
        assert!(matches!(
            Understanding::load(&paths, 1),
            Err(LoadError::Mismatch(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Probabilities match the training script's export
    /// (`tests/fixtures/understanding_parity.json`).
    #[test]
    #[ignore = "needs INTENT_CLASSIFIER_MODEL_DIR with the e5 files"]
    fn parity_with_the_python_reference() {
        let Ok(dir) = std::env::var("INTENT_CLASSIFIER_MODEL_DIR") else {
            return;
        };
        let model = Understanding::load(&committed(Path::new(&dir)), 2).unwrap();
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/understanding_parity.json"
        ))
        .unwrap();
        assert_eq!(fixture["artefact_sha256"], UNDERSTANDING_SHA256);
        let mut worst = 0.0f64;
        for case in fixture["cases"].as_array().unwrap() {
            let goal = case["goal"].as_str().unwrap();
            let ids: Vec<u32> = serde_json::from_value(case["ids"].clone()).unwrap();
            assert_eq!(model.token_ids(goal).unwrap(), ids, "{goal}");
            let got = model.read(goal).unwrap();
            for (key, want) in case["heads"].as_object().unwrap() {
                let want: Vec<f64> = serde_json::from_value(want.clone()).unwrap();
                let (kind, write) = key.split_once(':').unwrap();
                let have: Vec<f64> = match kind {
                    "act" => got.acts[TYPES.iter().position(|t| *t == write).unwrap()].clone(),
                    "value" => got.values[write].iter().map(|(_, p)| *p).collect(),
                    _ => got.target.clone(),
                };
                for (a, b) in have.iter().zip(&want) {
                    worst = worst.max((a - b).abs());
                }
            }
        }
        eprintln!("understanding parity: max |Δp| = {worst:.2e}");
        assert!(worst < 1e-5, "max |Δp| {worst}");
    }

    /// Latency per goal (release build), for the 30 ms container budget.
    #[test]
    #[ignore = "needs INTENT_CLASSIFIER_MODEL_DIR with the e5 files"]
    fn latency() {
        let Ok(dir) = std::env::var("INTENT_CLASSIFIER_MODEL_DIR") else {
            return;
        };
        let threads = std::env::var("UNDERSTANDING_THREADS")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(2);
        let started = std::time::Instant::now();
        let model = Understanding::load(&committed(Path::new(&dir)), threads).unwrap();
        let load = started.elapsed();
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/understanding_parity.json"
        ))
        .unwrap();
        let goals: Vec<String> = fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["goal"].as_str().unwrap().to_owned())
            .collect();
        let mut times = Vec::new();
        for _ in 0..5 {
            for goal in &goals {
                let t = std::time::Instant::now();
                model.read(goal).unwrap();
                times.push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        times.sort_by(f64::total_cmp);
        let q = |p: f64| times[((times.len() as f64 * p).ceil() as usize).saturating_sub(1)];
        eprintln!(
            "understanding threads={threads} load={}ms n={} p50={:.2}ms p95={:.2}ms max={:.2}ms",
            load.as_millis(),
            times.len(),
            q(0.5),
            q(0.95),
            times.last().unwrap()
        );
    }
}
