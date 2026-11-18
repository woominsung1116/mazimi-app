//! BM25-style ranking of one programme's passages for a question.
//!
//! Terms are the question's content words after particle stripping, each
//! with its synonym forms, matched as substrings of the whitespace-free
//! passage (Korean compounds: `휴학` in `휴학생`, `서류` in `제출서류`).
//! Character bigrams of the specific terms add a small partial-match signal.
//! A passage in a section of the question's topic (rule classifier) gets a
//! boost, so `서류는?` answers from 제출서류 even with weak lexical overlap.
//!
//! Answer or not-found:
//! - a question with a specific term (not topic vocabulary such as 서류,
//!   마감, 조건) is answered only by passages containing one, and the best
//!   one must cover at least [`MIN_COVERAGE`] of the question's weight;
//! - a question of topic vocabulary only is answered by matching passages
//!   or passages of its topic;
//! - otherwise nothing is found and the caller says so.
use super::passages::{Highlight, Passage};
use super::text::{self, Term};
use crate::assistant_provider::Topic;

const K1: f64 = 1.2;
const B: f64 = 0.75;
const SPECIFIC_WEIGHT: f64 = 1.0;
const GENERIC_WEIGHT: f64 = 0.6;
const BIGRAM_WEIGHT: f64 = 0.3;
const TOPIC_FACTOR: f64 = 1.3;
const TOPIC_BONUS: f64 = 1.0;
/// Share of the question's idf-weighted terms the best passage must contain.
pub const MIN_COVERAGE: f64 = 0.3;
/// Passages below this share of the best score are not shown.
const RELATIVE_CUTOFF: f64 = 0.5;
pub const TOP_K: usize = 3;

#[derive(Debug, Clone)]
pub struct Ranked {
    pub index: usize,
    pub score: f64,
    pub highlights: Vec<Highlight>,
}

#[derive(Debug, Clone)]
pub struct Retrieval {
    pub ranked: Vec<Ranked>,
    pub not_found: bool,
}

struct Doc {
    compact: String,
    map: Vec<usize>,
    bigrams: std::collections::HashSet<String>,
}

fn term_matches(doc: &Doc, term: &Term) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = term
        .forms
        .iter()
        .flat_map(|form| text::occurrences(&doc.compact, form))
        .collect();
    spans.sort_unstable();
    spans.dedup();
    spans
}

fn highlights(doc: &Doc, spans: &mut [(usize, usize)]) -> Vec<Highlight> {
    spans.sort_unstable();
    let mut out: Vec<Highlight> = Vec::new();
    for &(start, end) in spans.iter() {
        let (Some(&from), Some(&to)) = (doc.map.get(start), doc.map.get(end - 1)) else {
            continue;
        };
        let span = Highlight {
            start: from,
            end: to + 1,
        };
        match out.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => out.push(span),
        }
    }
    out
}

pub fn retrieve(question: &str, topics: &[Topic], passages: &[Passage]) -> Retrieval {
    let terms = text::query_terms(question);
    let docs: Vec<Doc> = passages
        .iter()
        .map(|p| {
            let (compact, map) = text::compact(&p.text);
            let bigrams = text::bigrams(&compact).into_iter().collect();
            Doc {
                compact,
                map,
                bigrams,
            }
        })
        .collect();
    if docs.is_empty() {
        return Retrieval {
            ranked: Vec::new(),
            not_found: true,
        };
    }
    let n = docs.len() as f64;
    let average = docs
        .iter()
        .map(|d| d.compact.chars().count() as f64)
        .sum::<f64>()
        / n;
    // matches[t][d]: spans of term t in doc d.
    let matches: Vec<Vec<Vec<(usize, usize)>>> = terms
        .iter()
        .map(|term| docs.iter().map(|doc| term_matches(doc, term)).collect())
        .collect();
    let idf: Vec<f64> = matches
        .iter()
        .map(|per_doc| {
            // An absent term weighs like the rarest present one, so one
            // unknown word does not outweigh every matched term.
            let df = (per_doc.iter().filter(|spans| !spans.is_empty()).count() as f64).max(1.0);
            (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
        })
        .collect();
    let weight = |term: &Term| {
        if term.specific {
            SPECIFIC_WEIGHT
        } else {
            GENERIC_WEIGHT
        }
    };
    let has_specific = terms.iter().any(|t| t.specific);
    // Coverage counts the terms that decide found / not found.
    let counted = |term: &Term| term.specific || !has_specific;
    let total_weight: f64 = terms
        .iter()
        .zip(&idf)
        .filter(|(term, _)| counted(term))
        .map(|(term, idf)| weight(term) * idf)
        .sum();
    let query_bigrams: Vec<String> = terms
        .iter()
        .filter(|t| t.specific)
        .flat_map(|t| text::bigrams(&t.surface))
        .collect();
    let wanted = |passage: &Passage| {
        passage
            .section
            .topic()
            .is_some_and(|topic| topics.contains(&topic))
    };

    let mut candidates: Vec<(Ranked, f64)> = Vec::new();
    for (d, doc) in docs.iter().enumerate() {
        let length = doc.compact.chars().count() as f64;
        let mut score = 0.0;
        let mut covered = 0.0;
        let mut specific_hit = false;
        let mut any_hit = false;
        let mut spans = Vec::new();
        for (t, term) in terms.iter().enumerate() {
            let found = &matches[t][d];
            if found.is_empty() {
                continue;
            }
            any_hit = true;
            specific_hit |= term.specific;
            let tf = found.len() as f64;
            score += weight(term) * idf[t] * tf * (K1 + 1.0)
                / (tf + K1 * (1.0 - B + B * length / average));
            if counted(term) {
                covered += weight(term) * idf[t];
            }
            spans.extend_from_slice(found);
        }
        if !query_bigrams.is_empty() {
            let hits = query_bigrams
                .iter()
                .filter(|b| doc.bigrams.contains(*b))
                .count();
            score += BIGRAM_WEIGHT * hits as f64 / query_bigrams.len() as f64;
        }
        let topical = wanted(&passages[d]);
        let eligible = if has_specific {
            specific_hit
        } else if !terms.is_empty() {
            any_hit || topical
        } else {
            topical
        };
        if !eligible {
            continue;
        }
        if topical {
            score = score * TOPIC_FACTOR + TOPIC_BONUS;
        }
        let coverage = if total_weight > 0.0 {
            covered / total_weight
        } else {
            0.0
        };
        candidates.push((
            Ranked {
                index: d,
                score,
                highlights: highlights(doc, &mut spans),
            },
            coverage,
        ));
    }
    candidates.sort_by(|a, b| {
        b.0.score
            .partial_cmp(&a.0.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.index.cmp(&b.0.index))
    });
    let best_coverage = candidates.iter().map(|(_, c)| *c).fold(0.0, f64::max);
    if candidates.is_empty() || (has_specific && best_coverage < MIN_COVERAGE) {
        return Retrieval {
            ranked: Vec::new(),
            not_found: true,
        };
    }
    let best = candidates[0].0.score;
    let ranked = candidates
        .into_iter()
        .map(|(ranked, _)| ranked)
        .filter(|r| r.score >= best * RELATIVE_CUTOFF)
        .take(TOP_K)
        .collect();
    Retrieval {
        ranked,
        not_found: false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::passages::{self, tests::program};
    use super::*;
    use serde_json::json;

    fn sample() -> Vec<Passage> {
        passages::build(
            &program(json!({
                "summary": "부산 거주 만 19~34세 청년에게 월 최대 20만원 월세를 지원합니다.",
                "application_method": "부산청년플랫폼 누리집에서 온라인으로 신청합니다.",
                "submission_documents": "신분증, 주민등록 등본, 임대차계약서 사본",
                "raw_payload": {"sprtTrgtCn": "부산 거주 무주택 청년\n대학 휴학생 및 졸업생 포함"},
                "application_end_at": "2026-12-31T14:59:59Z"
            })),
            &[],
        )
    }

    fn top(question: &str, topics: &[Topic]) -> Vec<String> {
        let passages = sample();
        let retrieval = retrieve(question, topics, &passages);
        retrieval
            .ranked
            .iter()
            .map(|r| passages[r.index].id.clone())
            .collect()
    }

    #[test]
    fn specific_terms_must_appear_and_compounds_match() {
        assert_eq!(
            top("휴학생도 받을 수 있어?", &[Topic::Eligibility]),
            ["raw.sprtTrgtCn.1"]
        );
        assert_eq!(
            top("주민등록등본 필요해?", &[Topic::Documents])[0],
            "documents.0"
        );
        let passages = sample();
        let none = retrieve("외국인도 신청할 수 있어?", &[Topic::Eligibility], &passages);
        assert!(none.not_found && none.ranked.is_empty());
    }

    #[test]
    fn topic_vocabulary_answers_from_its_section() {
        assert_eq!(top("그럼 서류는?", &[Topic::Documents]), ["documents.0"]);
        assert_eq!(top("언제 마감해?", &[Topic::Deadline])[0], "record.window");
        assert_eq!(top("어떻게 신청해?", &[Topic::Steps])[0], "method.0");
        assert!(retrieve("이거 뭐야?", &[Topic::Unsupported], &sample()).not_found);
    }

    #[test]
    fn highlights_point_at_original_text() {
        let passages = sample();
        let retrieval = retrieve("주민등록등본 필요해?", &[Topic::Documents], &passages);
        let best = &retrieval.ranked[0];
        let text: Vec<char> = passages[best.index].text.chars().collect();
        let marked: String = text[best.highlights[0].start..best.highlights[0].end]
            .iter()
            .collect();
        assert_eq!(marked, "주민등록 등본");
    }
}
