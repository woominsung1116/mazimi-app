//! Grounded Q&A ("근거 검색형 Q&A"): answer a question about one programme by
//! quoting its stored official text, never by generating facts.
//!
//! `question → topics (rule classifier, previous topic for follow-ups) →
//! passages (on-the-fly index) → BM25 + topic boost → ≤3 quotes with
//! highlights | not found`, plus a verdict from the shared structural
//! eligibility when the question is about eligibility. An optional
//! generated summary is a separate, streamed step
//! (`routes::assistant::answer_stream`) checked by [`validate`].
pub mod intent;
pub mod passages;
pub mod retrieval;
pub(crate) mod text;
pub mod validate;
pub mod verdict;

use crate::assistant_provider::{is_out_of_scope, redact_question, rule_topics, Topic};
use mazimi_core::models::{CheckEvidence, Program};
use mazimi_core::structural::StructuralProfile;
use passages::{Passage, PassageSection};
use serde::Serialize;
use verdict::{Overall, Verdict};

#[derive(Debug, Clone, Serialize)]
pub struct Answer {
    /// Topics used for ranking; `unsupported` alone when out of scope.
    pub topics: Vec<Topic>,
    /// Send back as `previous_topic` so a follow-up without its own topic
    /// ("그럼 휴학생은?") keeps this one. No conversation is stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_topic: Option<Topic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    pub passages: Vec<Passage>,
    pub not_found: bool,
    /// Out of scope (state changes, hypotheticals, calculations): no search.
    pub unsupported: bool,
    pub notice: &'static str,
}

pub const NOT_FOUND_NOTICE: &str =
    "공고에서 이 질문에 대한 내용을 찾지 못했어요. 공식 공고에서 확인해 주세요.";
const FOUND_NOTICE: &str =
    "공고 원문 문장을 그대로 보여 드려요. 문장을 바꿔 쓰거나 새 정보를 만들지 않아요.";
const UNSUPPORTED_NOTICE: &str =
    "이 질문은 공고 문장으로 답할 수 없는 요청이에요. 프로필·알림·신청 상태는 바꾸지 않았어요.";

/// Topics of a question. A question with no topic cue of its own keeps the
/// previous question's topic; out-of-scope requests never do.
pub fn topics_for(question: &str, previous: Option<Topic>) -> (Vec<Topic>, bool) {
    if is_out_of_scope(question) {
        return (vec![Topic::Unsupported], true);
    }
    let topics = rule_topics(question);
    match previous {
        Some(previous) if topics == [Topic::Unsupported] && previous != Topic::Unsupported => {
            (vec![previous], false)
        }
        _ => (topics, false),
    }
}

pub struct Inputs<'a> {
    pub question: &'a str,
    pub previous_topic: Option<Topic>,
    pub program: &'a Program,
    pub program_university_id: Option<&'a str>,
    pub facts: StructuralProfile<'a>,
    pub overall: Overall<'a>,
    pub evidence: &'a [CheckEvidence],
}

pub fn answer(inputs: &Inputs) -> Answer {
    let (topics, unsupported) = topics_for(inputs.question, inputs.previous_topic);
    if unsupported {
        return Answer {
            topics,
            follow_up_topic: None,
            verdict: None,
            passages: Vec::new(),
            not_found: false,
            unsupported: true,
            notice: UNSUPPORTED_NOTICE,
        };
    }
    // Contact details and ID numbers never become search terms.
    let question = redact_question(inputs.question);
    let index = passages::build(inputs.program, inputs.evidence);
    let retrieval = retrieval::retrieve(&question, &topics, &index);
    let quoted: Vec<Passage> = retrieval
        .ranked
        .iter()
        .map(|ranked| {
            let mut passage = index[ranked.index].clone();
            passage.highlights = ranked.highlights.clone();
            passage
        })
        .collect();
    let verdict = verdict::verdict(
        &question,
        &topics,
        inputs.program,
        inputs.program_university_id,
        &inputs.facts,
        &inputs.overall,
    )
    .map(|mut v| {
        v.passage_id = quoted
            .iter()
            .find(|p| p.section == PassageSection::Conditions)
            .map(|p| p.id.clone());
        v
    });
    Answer {
        follow_up_topic: topics.iter().copied().find(|t| *t != Topic::Unsupported),
        topics,
        verdict,
        not_found: retrieval.not_found,
        unsupported: false,
        notice: if retrieval.not_found {
            NOT_FOUND_NOTICE
        } else {
            FOUND_NOTICE
        },
        passages: quoted,
    }
}

#[cfg(test)]
mod eval;
