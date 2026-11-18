//! Publishing a fresh-extraction proposal (live mode only).
//!
//! A proposal reaches this step only after every gate a brand-new candidate
//! passes: precheck and campus scope, opt-out, robots, ToS, the
//! `direct:direct_html` lane and verbatim quotes of at most 100 characters
//! (see [`super::reextract`]). Publishing adds the checks that decide whether
//! the hidden offer may be shown again **as it is displayed**:
//!
//! 1. verification mode is `live` (`CORPORATE_AUTO_VERIFY_MODE`); in shadow
//!    nothing here runs and no programme row is written;
//! 2. the whole-page ending and date scan ran on this fetch and is clean
//!    (no scan available = refused);
//! 3. exactly one programme is tied to the source (any non-retired monitor
//!    in any verification mode, or a live programme linking the source URL),
//!    it is `corporate_benefit` with an `evidence_auto` monitor, not retired
//!    and not under a publisher opt-out;
//! 4. no side-table row the detail page shows ([`DETAIL_TABLE_CLASSES`]
//!    `side`: eligibility rules, required documents, application resources,
//!    AI evidence citations, condition reviews), else `unverified_side_data`;
//! 5. every public field of the programme ([`PROGRAM_COLUMN_CLASSES`]) is
//!    stated by this page: values and audience in title and summary
//!    (value tokens normalised, `1만 원` = `10,000원`), free text verbatim,
//!    end dates equal to the page's, amounts and ages stated by the quotes;
//!    regions, schools and tags empty. Anything else refuses.
//!
//! A value that differs is not rewritten here: the proposal stays open and
//! the offer stays hidden.
//!
//! On publish, in a savepoint of the attempt's transaction that is rolled
//! back unless the programme row really changed: the programme's old facts are
//! retired (kept, never deleted; their quotes go to the new facts'
//! `anchors.previous_quotes`, so the audit seed never re-inserts them), the
//! new quotes become its facts, and the programme is restored exactly as the
//! evidence job restores a confirmed one (monitor current, candidate
//! approved, health refreshed, evidence holds released to the state
//! captured when they were placed, source events resolved). The evidence
//! job then checks the new facts on its next pass like any other.

use anyhow::Result;
use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;
use uuid::Uuid;

use super::bootstrap::Offer;
use super::evidence::{compact, read_quote, PageEvidence, EVIDENCE_MAX_AGE_DAYS};
use super::values::value_signature;
use chrono::{DateTime, Utc};
use sqlx::Acquire;

/// Every table the public programme detail reads (`get_program`,
/// `load_program_detail` and its loaders in `crates/api`, and the
/// `current_programs` view), and why it does or does not block publishing.
/// A test parses those sources, so a new join fails the build until it is
/// classified here.
///
/// - `programme`: the programme row itself (checked field by field,
///   [`PROGRAM_COLUMN_CLASSES`]);
/// - `identity`: brand name and logo next to the offer (who, not what);
/// - `visibility`: feed authorisation for `corporate_feed` programmes only;
///   it decides whether the row is listed, shows nothing;
/// - `side`: facts shown next to the programme that this step cannot
///   re-check; any row refuses with `unverified_side_data`.
///
/// `program_condition_reviews` is not joined by the detail route itself; it
/// reaches users through the condition overrides and is listed so a review
/// also refuses.
pub const DETAIL_TABLE_CLASSES: &[(&str, &str)] = &[
    ("current_programs", "programme"),
    ("programs", "programme"),
    ("brands", "identity"),
    ("program_brands", "identity"),
    ("benefit_feed_items", "visibility"),
    ("benefit_feeds", "visibility"),
    ("benefit_source_domains", "visibility"),
    ("eligibility_rules", "side"),
    ("program_documents", "side"),
    ("program_application_resources", "side"),
    ("program_ai_extraction_candidates", "side"),
    ("program_ai_extraction_cache", "side"),
    ("program_condition_reviews", "side"),
];

/// The row predicate of a side table for programme `$1`.
fn side_predicate(table: &str) -> Option<&'static str> {
    Some(match table {
        "eligibility_rules" => "SELECT 1 FROM eligibility_rules WHERE program_id = $1",
        "program_documents" => "SELECT 1 FROM program_documents WHERE program_id = $1",
        "program_application_resources" => {
            "SELECT 1 FROM program_application_resources WHERE program_id = $1"
        }
        "program_ai_extraction_candidates" => {
            "SELECT 1 FROM program_ai_extraction_candidates WHERE program_id = $1"
        }
        "program_ai_extraction_cache" => {
            "SELECT 1 FROM program_ai_extraction_cache WHERE program_id = $1"
        }
        "program_condition_reviews" => {
            "SELECT 1 FROM program_condition_reviews r JOIN programs p
             ON p.source_type = r.source_type AND p.source_id = r.source_id WHERE p.id = $1"
        }
        _ => return None,
    })
}

/// Side tables holding any row for the programme, in [`DETAIL_TABLE_CLASSES`]
/// order.
async fn side_rows(tx: &mut Transaction<'_, Postgres>, program_id: Uuid) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for (table, class) in DETAIL_TABLE_CLASSES {
        if *class != "side" {
            continue;
        }
        let predicate = side_predicate(table)
            .ok_or_else(|| anyhow::anyhow!("side table {table} has no row predicate"))?;
        let exists: bool = sqlx::query_scalar(&format!("SELECT EXISTS ({predicate})"))
            .bind(program_id)
            .fetch_one(&mut **tx)
            .await?;
        if exists {
            found.push(table.to_string());
        }
    }
    Ok(found)
}

/// The `resilience_24h.hidden_by_reason` name of a `resilience` hide reason
/// (`hidden_` + [`super::resilience::reason_key`], as health names it), so
/// fresh-extraction counts line up with the evidence job's. The reason
/// itself is recorded verbatim.
pub fn hidden_metric(reason: &str) -> Option<String> {
    super::resilience::reason_key(reason).map(|key| format!("hidden_{key}"))
}

/// Days within which the evidence job may adopt the new facts' baseline.
pub const ADOPT_WITHIN_DAYS: i64 = 1;

/// Result of the whole-page ending and date scan on the page just read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanVerdict {
    Clean,
    /// The page says the offer ended (marker or past end date); the reason
    /// names what was found.
    Ended(String),
}

/// The whole-page ending and date scan: the page's HTML and every quote a
/// proposal would rest on.
pub type EndingScan = fn(&str, &[&str]) -> ScanVerdict;

/// The production scan: `resilience::page_blocked` on the whole page for
/// every quote (the evidence job's own scan, never a copy; contract §16).
pub const PAGE_ENDING_SCAN: Option<EndingScan> = Some(page_blocked_scan);

/// [`super::resilience::page_blocked`] for each quote on one page view.
pub fn page_blocked_scan(html: &str, quotes: &[&str]) -> ScanVerdict {
    let view = super::resilience::PageView::new(html, &super::evidence::visible_text(html));
    for quote in quotes {
        if let Some(reason) = super::resilience::page_blocked(&view, quote) {
            return ScanVerdict::Ended(reason.to_string());
        }
    }
    ScanVerdict::Clean
}

/// The normalised unit-bearing values stated in `text`, read by the shared
/// value parser (`values::value_signature` / `canonical_value`: `1만 원`,
/// `1만원` and `10,000원` read `10000원`).
pub fn value_tokens(text: &str) -> BTreeSet<String> {
    // On compacted text, as the evidence job reads its windows (`10 %` is
    // `10%`; a gap between two digits stays a separator).
    value_signature(&compact(text))
        .split(',')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Gate 4: every displayed value is stated by the new quotes.
pub fn displayed_values_evidenced(displayed: &str, quotes: &[&str]) -> Result<(), String> {
    let shown = value_tokens(displayed);
    if shown.is_empty() {
        return Err("no_displayed_value".into());
    }
    let stated: BTreeSet<String> = quotes.iter().flat_map(|q| value_tokens(q)).collect();
    let missing: Vec<&String> = shown.difference(&stated).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "values_differ:{}",
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ))
    }
}

/// How often a value is given (`매월`, `가입 시 1회`).
static VALUE_FREQUENCY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"매월|매달|월\s*\d|매년|연\s*\d|매주|매일|1\s*회|최초|첫\s*(?:달|결제|구매|이용|가입)?|가입\s*시|(?i:\bmonthly\b|\bper\s+month\b|\bonce\b|\byearly\b|\bannual)")
        .expect("valid frequency pattern")
});
/// Claims without a number (`첫 달 무료`, `평생`): shown only when the page
/// states the same phrase.
static VALUE_CLAIM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"무료|첫\s*달|평생|무제한|1\s*\+\s*1|증정|무상|(?i:\bfree\b|\bunlimited\b|\blifetime\b)",
    )
    .expect("valid claim pattern")
});
/// A dated day (`10월 1일`): never a frequency.
static DATED_DAY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{1,2}\s*월\s*\d{1,2}\s*일").expect("valid dated-day pattern"));
/// The value's own clause says it is not given (`… 대상에서 제외됩니다`).
static VALUE_NEGATED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)제외|불가|대상이\s*아닙|아닙니다|아님|안\s*됩|안\s*돼|미적용|미제공|하지\s*않|되지\s*않|받을\s*수\s*없|받으실\s*수\s*없|였습니다|이었습니다|까지였|변경|종료|마감|중단|만료|없음|없습니다|해당되지|not\s+eligible|excluded|no\s+longer")
        .expect("valid negation pattern")
});
/// A threshold or cap anywhere in the sentence (`3만원 이상 구매 시`,
/// `최대`): the numbers are not the offer's plain value.
static VALUE_CONDITIONED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"이상|이하|미만|초과|최대|최소|(?i:\bup\s+to\b|\bminimum\b|\bat\s+least\b)")
        .expect("valid conditioned pattern")
});
/// Clause breaks inside a quote or a title: a value's kind and frequency
/// are read in its own clause.
static VALUE_CLAUSE_BREAK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[,;·/+()|&]|\s및\s|\s그리고\s|\s또는\s|\band\b|(?:과|와|하고|되고|되며|이며|으며|지만|는데|으나|이나|되어)\s")
        .expect("valid clause break")
});

/// How often, normalised: `월 1만원`, `매월`, `매달` are monthly; `1회`,
/// `최초`, `가입 시` are once. A dated day (`10월 1일`) is none.
fn frequency_of(text: &str) -> BTreeSet<&'static str> {
    let undated = DATED_DAY.replace_all(text, " # ");
    VALUE_FREQUENCY
        .find_iter(&undated)
        .map(|m| {
            let word: String = m
                .as_str()
                .chars()
                .filter(|c| !c.is_whitespace() && !c.is_ascii_digit())
                .collect::<String>()
                .to_lowercase();
            match word.as_str() {
                "매월" | "매달" | "월" | "monthly" | "permonth" => "monthly",
                "매년" | "연" | "yearly" | "annual" => "yearly",
                "매주" => "weekly",
                "매일" => "daily",
                // `첫`, `첫 달`, `첫 결제`, `최초`, `1회`, `가입 시`.
                _ => "once",
            }
        })
        .collect()
}

/// The clauses of `text` that state `value` (a thousands comma is no
/// break: `10,000원`).
fn value_clauses(text: &str, value: &str) -> Vec<String> {
    let joined = DIGIT_COMMA.replace_all(text, "$1$2");
    VALUE_CLAUSE_BREAK
        .split(&joined)
        .filter(|clause| value_tokens(clause).contains(value))
        .map(str::to_string)
        .collect()
}
static DIGIT_COMMA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d),(\d)").expect("valid digit-comma pattern"));

/// Each claim of `displayed`: its whole phrase compacted (the claim word
/// and what follows it in its clause: `첫달무료`, `평생10%할인`) and the
/// word right before it, particles taken off (`대학생`).
fn claim_phrases(displayed: &str) -> Vec<(String, Option<String>, Vec<&'static str>)> {
    let mut phrases = Vec::new();
    for clause in VALUE_CLAUSE_BREAK.split(displayed) {
        if let Some(found) = VALUE_CLAIM.find(clause) {
            phrases.push((
                compact(&clause[found.start()..]).to_lowercase(),
                word_before(&clause[..found.start()]),
                super::bootstrap::audience_codes(clause),
            ));
        }
    }
    phrases
}

fn word_before(text: &str) -> Option<String> {
    let word = text.split_whitespace().last()?;
    let word = word.trim_end_matches(['은', '는', '이', '가', '을', '를', '의', '도', '에', '게']);
    (!word.is_empty()).then(|| word.to_lowercase())
}

/// A quote states the claim: a clause holding the whole phrase, not negated
/// or ended, whose word before the claim is the displayed one, an audience
/// or nothing (`배송비 무료` is no `대학생 무료`).
fn claim_stated(
    phrase: &str,
    before: Option<&str>,
    audience: &[&'static str],
    quotes: &[&str],
) -> bool {
    quotes.iter().any(|quote| {
        VALUE_CLAUSE_BREAK.split(quote).any(|clause| {
            let compacted = compact(clause).to_lowercase();
            let Some(at) = compacted.find(phrase) else {
                return false;
            };
            if VALUE_NEGATED.is_match(clause) {
                return false;
            }
            // Every other word of the claim's clause is from the allowed
            // set or the claim itself.
            if !clause
                .split_whitespace()
                .all(|w| allowed_clause_word(w) || VALUE_CLAIM.is_match(w))
            {
                return false;
            }
            let claim_start = VALUE_CLAIM.find(clause).map_or(0, |m| m.start());
            // The audience named in the claim's own clause is the shown
            // one (`대학생 무료` is no `청년 무료`).
            let clause_audience = super::bootstrap::audience_codes(clause);
            if !clause_audience.is_empty() && clause_audience != audience {
                return false;
            }
            match word_before(&clause[..claim_start]) {
                None => true,
                Some(word) => {
                    before.is_some_and(|b| b == word)
                        || !super::bootstrap::audience_codes(&word).is_empty()
                        || at == 0
                }
            }
        })
    })
}

/// One value as written (`10%`, `5,000원`, `1만 원`, `20 퍼센트`).
static VALUE_FORM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d[\d,.]*\s*(?:만\s*(?:\d[\d,.]*\s*천?)?|천)?\s*(?:%|퍼센트|원|포인트|P\b)")
        .expect("valid value form")
});
/// The kind of benefit right after a value.
static KIND_AFTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(할인|적립|캐시백|페이백|환급|지원|off\b|discount|cashback)")
        .expect("valid kind-after pattern")
});
/// The words a value's clause may hold besides its audience, frequency,
/// value and kind: how one gets it and that one gets it. Anything else (a
/// product, a threshold, a cap, a negation, a past tense, a range word)
/// refuses.
const VALUE_CLAUSE_WORDS: &[&str] = &[
    "학생증",
    "제시",
    "시",
    "인증",
    "학생인증",
    "재학",
    "증명",
    "확인",
    "학교",
    "이메일",
    "전메뉴",
    "메뉴",
    "모든",
    "전체",
    "상품",
    "결제",
    "금액",
    "혜택",
    "쿠폰",
    "제공",
    "제공해",
    "제공합니다",
    "제공됩니다",
    "드립니다",
    "드려요",
    "드림",
    "받을",
    "받으실",
    "수",
    "있습니다",
    "있어요",
    "적용",
    "적용됩니다",
    "됩니다",
    "돼요",
    "가능",
    "가능합니다",
    "해",
    "해드립니다",
    "지급",
    "지급됩니다",
    "이용",
    "구독",
    "멤버십",
    "가입",
    "하면",
    "하시면",
    "인증하면",
    "인증하시면",
    "즉시",
    "특별",
    "대상",
    "students",
    "student",
    "get",
    "on",
    "all",
    "every",
    "plan",
    "plans",
    "the",
    "a",
    "of",
    "for",
    "with",
    "verified",
    "price",
    "pricing",
];

/// A word of a value's clause, punctuation and a trailing particle off.
fn clause_word(word: &str) -> String {
    let word = word.trim_matches(|c: char| !c.is_alphanumeric());
    let word = [
        "에게", "으로", "은", "는", "이", "가", "을", "를", "의", "도", "로", "만",
    ]
    .iter()
    .find_map(|p| word.strip_suffix(p).filter(|w| !w.is_empty()))
    .unwrap_or(word);
    word.to_lowercase()
}

fn allowed_clause_word(word: &str) -> bool {
    let bare = word
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    [bare, clause_word(word)].iter().any(|form| {
        form.is_empty()
            || VALUE_CLAUSE_WORDS.contains(&form.as_str())
            || !super::bootstrap::audience_codes(form).is_empty()
            || frequency_word(form)
    })
}

/// A whole word that is a frequency (`매월`, `월`, `1회`, `최초`, `가입`).
fn frequency_word(word: &str) -> bool {
    VALUE_FREQUENCY
        .find(word)
        .is_some_and(|m| m.start() == 0 && m.end() == word.len())
        || matches!(word, "월" | "첫" | "달" | "첫달")
}

/// The narrow positive grammar of a value's own clause: `[audience]
/// [frequency] [how] value kind [benefit words]`, every other word from
/// the allowed set; returns the kind right after the value. A clause that
/// does not fit (a threshold `3,000원 이상`, a cap `최대`, a product, a
/// negation, a past tense, a change, a range) states no value to publish.
fn clause_kind(clause: &str, value: &str) -> Option<String> {
    // `전 메뉴` is the one fixed phrase with `전`; alone, `전` (before) is
    // no allowed word.
    let clause =
        squash_words(&DIGIT_COMMA.replace_all(clause, "$1$2")).replace("전 메뉴", "전메뉴");
    VALUE_FORM.find_iter(&clause).find_map(|found| {
        if !value_tokens(found.as_str()).contains(value) {
            return None;
        }
        let after = &clause[found.end()..];
        let kind = KIND_AFTER.captures(after)?;
        let kind_end = kind.get(0)?.end();
        let words_ok = clause[..found.start()]
            .split_whitespace()
            .chain(after[kind_end..].split_whitespace())
            .all(allowed_clause_word);
        words_ok.then(|| kind[1].to_lowercase())
    })
}

fn squash_words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Gate 4: every displayed value is bound on the page the way it is shown.
/// A quote states the value in a clause of its own that names the shown
/// kind of benefit (할인 is not 적립) and frequency (매월 is not 가입 시 1회,
/// a date is no frequency) and does not negate it (`… 대상에서 제외`); the
/// quote names the shown audience and no narrower one (or, naming none,
/// the page's audience covers it). Every claim without a number is stated
/// as the same phrase (`첫 달 무료` is not `첫 달 10% 할인, 배송비 무료`).
pub fn displayed_values_bound(
    displayed: &str,
    quotes: &[&str],
    page_audience: &[&str],
) -> Result<(), String> {
    for (phrase, before, audience) in claim_phrases(displayed) {
        if !claim_stated(&phrase, before.as_deref(), &audience, quotes) {
            return Err(format!("claim_unstated:{phrase}"));
        }
    }
    let audience: BTreeSet<&str> = super::bootstrap::audience_codes(displayed)
        .into_iter()
        .collect();
    for value in value_tokens(displayed) {
        let shown = value_clauses(displayed, &value);
        // The kind right after the value where it is shown; none, refused.
        let Some(kind) = shown.iter().find_map(|c| {
            let c = squash_words(&DIGIT_COMMA.replace_all(c, "$1$2"));
            VALUE_FORM
                .find_iter(&c)
                .filter(|m| value_tokens(m.as_str()).contains(&value))
                .find_map(|m| {
                    KIND_AFTER
                        .captures(&c[m.end()..])
                        .map(|k| k[1].to_lowercase())
                })
        }) else {
            return Err(format!("value_unbound:{value}"));
        };
        let frequency: BTreeSet<&str> = shown.iter().flat_map(|c| frequency_of(c)).collect();
        let bound = quotes.iter().any(|quote| {
            let quote_audience: BTreeSet<&str> = super::bootstrap::audience_codes(quote)
                .into_iter()
                .collect();
            // Every displayed audience is named, and no narrower one the
            // display leaves out (`대학원생 전용` shown as `학생`); the
            // generic `학생` of a heading narrows nothing.
            let audience_ok = if quote_audience.is_empty() {
                audience.iter().all(|code| page_audience.contains(code))
            } else {
                audience.is_subset(&quote_audience)
                    && quote_audience
                        .iter()
                        .all(|code| *code == "student" || audience.contains(code))
            };
            audience_ok
                && value_clauses(quote, &value).iter().any(|clause| {
                    // A range (`10%~20%`) states no single value; the value
                    // binds only through the positive grammar: the same
                    // kind right after it and the same frequency.
                    !super::resilience::value_range_b(clause)
                        && !VALUE_NEGATED.is_match(quote)
                        && !VALUE_CONDITIONED.is_match(quote)
                        && clause_kind(clause, &value).as_deref() == Some(kind.as_str())
                        && frequency == frequency_of(clause)
                })
        });
        if !bound {
            return Err(format!("value_unbound:{value}"));
        }
    }
    Ok(())
}

/// Same key as the evidence job's (`{program_id}:{compact(quote)}`), so the
/// seed and relocation see a re-published quote as the same fact.
fn quote_key(program_id: Uuid, quote: &str) -> String {
    hex(&Sha256::digest(
        format!("{program_id}:{}", compact(quote)).as_bytes(),
    ))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the publish step decided, stored in the proposal's `gates.publish`.
#[derive(Debug, Clone, PartialEq)]
pub enum Published {
    Restored {
        program_id: Uuid,
        facts: usize,
    },
    Refused(String),
    /// Side-table rows shown on the detail page that cannot be re-checked.
    SideData(Vec<String>),
}

impl Published {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Restored { program_id, facts } => {
                json!({"outcome": "restored", "program_id": program_id, "facts": facts})
            }
            Self::Refused(reason) => json!({"outcome": "refused", "reason": reason}),
            Self::SideData(tables) => {
                json!({"outcome": "refused", "reason": "unverified_side_data",
                                             "side_tables": tables})
            }
        }
    }
}

/// How each column the API serializes (`PROGRAM_COLUMNS` in
/// `api/src/routes/programs.rs`) is verified before a programme is shown
/// again. A test reads that constant, so a new public column fails the
/// build until it is classified here.
///
/// - `identity`: who offers it and bookkeeping (not an offer fact);
/// - `state`: set by the restore itself;
/// - `internal`: never serialized (`raw_payload`, `age_basis`, which only
///   the public-data normalizers write), or serialized only through
///   `benefit_conditions` (`normalized_payload.conditions`, checked as text);
/// - `link`: must point at the page's own host;
/// - `values`: unit-bearing values and audience words must be stated;
/// - `text`: empty, or verbatim on the page;
/// - `start` / `end`: dates; a start date cannot be read from the page, an end
///   date must equal the page's end date (KST);
/// - `amount`: the amount must be stated;
/// - `age`: must equal the age the quotes state;
/// - `scope`: regions, schools and tags cannot be read from the page and
///   must be empty.
pub const PROGRAM_COLUMN_CLASSES: &[(&str, &str)] = &[
    ("id", "identity"),
    ("program_type", "identity"),
    ("source_type", "identity"),
    ("source_id", "identity"),
    ("provider_name", "identity"),
    ("company_name", "identity"),
    ("company_logo_url", "identity"),
    ("benefit_category", "identity"),
    ("provider_kind", "identity"),
    ("created_at", "identity"),
    ("updated_at", "identity"),
    ("last_synced_at", "identity"),
    ("program_status", "state"),
    ("is_active", "state"),
    ("raw_payload", "internal"),
    ("normalized_payload", "internal"),
    ("age_basis", "internal"),
    ("official_url", "link"),
    ("title", "values"),
    ("summary", "values"),
    ("application_method", "text"),
    ("submission_documents", "text"),
    ("screening_method", "text"),
    ("eligibility_condition", "text"),
    ("application_steps", "text"),
    ("application_start_at", "start"),
    ("application_end_at", "end"),
    ("deadline_at", "end"),
    ("benefit_valid_until", "end"),
    ("benefit_amount_monthly", "amount"),
    ("benefit_amount_semester", "amount"),
    ("benefit_amount_once", "amount"),
    ("min_age", "age"),
    ("max_age", "age"),
    ("regions", "scope"),
    ("region_districts", "scope"),
    ("region_scope", "scope"),
    ("school_scope", "scope"),
    ("tags", "scope"),
];

/// The public face of one programme, every column of
/// [`PROGRAM_COLUMN_CLASSES`] that can carry an offer fact, plus the detail
/// rows the API shows next to it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PublicProgramme {
    pub program_type: String,
    pub title: String,
    pub summary: Option<String>,
    pub official_url: Option<String>,
    pub application_method: Option<String>,
    pub eligibility_condition: Option<String>,
    pub submission_documents: Option<String>,
    pub screening_method: Option<String>,
    pub application_steps: Option<Value>,
    pub benefit_conditions: Option<String>,
    pub application_start_at: Option<DateTime<Utc>>,
    pub application_end_at: Option<DateTime<Utc>>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub benefit_valid_until: Option<DateTime<Utc>>,
    pub benefit_amount_monthly: Option<i32>,
    pub benefit_amount_semester: Option<i32>,
    pub benefit_amount_once: Option<i32>,
    pub min_age: Option<i32>,
    pub max_age: Option<i32>,
    pub regions: Option<Vec<String>>,
    pub region_districts: Option<Vec<String>>,
    pub region_scope: Option<Value>,
    pub school_scope: Option<Value>,
    pub tags: Option<Value>,
}

fn json_empty(value: &Option<Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.is_empty(),
        Some(Value::Object(map)) => map.is_empty(),
        Some(Value::String(text)) => text.trim().is_empty(),
        _ => false,
    }
}

fn json_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|v| json_strings(v, out)),
        Value::Object(map) => map.values().for_each(|v| json_strings(v, out)),
        _ => {}
    }
}

fn host_of(url: &str) -> Option<String> {
    let host = reqwest::Url::parse(url)
        .ok()?
        .host_str()?
        .to_ascii_lowercase();
    Some(host.trim_start_matches("www.").to_string())
}

/// Gate 4: every public field of the programme is stated by this page.
/// `Err` names the first field that is not. Pure.
pub fn verify_public_fields(
    programme: &PublicProgramme,
    offer: &Offer,
    quotes: &[&str],
    page_text: &str,
    page_url: &str,
) -> Result<(), String> {
    verify_fields(programme, offer, quotes, "", page_text, page_url, false)
}

/// Gate 4 for the verbatim display (§25): the shown title carries no value
/// or claim and the shown summary is the page's own sentences; every other
/// public field is checked as in [`verify_public_fields`].
pub fn verify_verbatim_fields(
    programme: &PublicProgramme,
    offer: &Offer,
    quotes: &[&str],
    page_html: &str,
    page_text: &str,
    page_url: &str,
) -> Result<(), String> {
    verify_fields(
        programme, offer, quotes, page_html, page_text, page_url, true,
    )
}

/// The sentences shown as the summary: the offer's own facts (the audience
/// cue alone only when there is no fact).
pub fn summary_quotes(offer: &Offer) -> Vec<&str> {
    let facts: Vec<&str> = offer.facts.iter().map(|f| f.quote.as_str()).collect();
    if facts.is_empty() {
        offer
            .verification
            .iter()
            .map(|f| f.quote.as_str())
            .collect()
    } else {
        facts
    }
}

/// The part of a stored condition the page must show (verify finding F5):
/// under brand scope with an issuer the condition is `{template} · {issuer
/// line}`; the template is server text, re-derived from the issuer's
/// holder key and compared, and only the issuer line is the page's. Any
/// other condition is the page's whole.
fn condition_page_part(text: &str, offer: &Offer) -> Result<String, ()> {
    let (Some(_), Some(issuer)) = (&offer.scope_brand, &offer.issuer) else {
        return Ok(text.to_string());
    };
    let holder = mazimi_core::holder::Holder::parse(&issuer.holder).ok_or(())?;
    let template = holder.field.condition_template(&issuer.name);
    let Some(rest) = text.strip_prefix(template.as_str()) else {
        return Err(());
    };
    match rest.strip_prefix(" · ") {
        Some(line) => Ok(line.to_string()),
        None if rest.is_empty() => Ok(String::new()),
        None => Err(()),
    }
}

fn verify_fields(
    programme: &PublicProgramme,
    offer: &Offer,
    quotes: &[&str],
    page_html: &str,
    page_text: &str,
    page_url: &str,
    verbatim: bool,
) -> Result<(), String> {
    let refuse = |field: &str| Err(format!("unverified_field:{field}"));
    for (field, empty) in [
        (
            "regions",
            programme.regions.as_ref().is_none_or(Vec::is_empty),
        ),
        (
            "region_districts",
            programme
                .region_districts
                .as_ref()
                .is_none_or(Vec::is_empty),
        ),
        ("region_scope", json_empty(&programme.region_scope)),
        ("school_scope", json_empty(&programme.school_scope)),
        ("tags", json_empty(&programme.tags)),
    ] {
        if !empty {
            return refuse(field);
        }
    }
    if let Some(url) = programme.official_url.as_deref() {
        if host_of(url).is_none() || host_of(url) != host_of(page_url) {
            return refuse("official_url");
        }
    }
    let displayed = format!(
        "{} {}",
        programme.title,
        programme.summary.as_deref().unwrap_or("")
    );
    if verbatim {
        let page = PageEvidence::from_visible_text(page_text);
        // The title is `{brand} {audience} {value}` from the facts; the brand
        // may carry digits (1Password, 11번가), nothing else may.
        let Ok(tail) = offer_title_tail(offer) else {
            return refuse("title");
        };
        let Some(brand) = programme.title.strip_suffix(tail.as_str()).map(str::trim) else {
            return refuse("title");
        };
        if VALUE_CLAIM.is_match(brand) || brand.contains(['%', '원']) {
            return refuse("title");
        }
        let Some(summary) = programme.summary.as_deref() else {
            return refuse("summary");
        };
        if !quotes.iter().all(|q| page.contains(q)) {
            return refuse("summary");
        }
        if let Err(reason) =
            display_invariant(summary, offer, page_html, page_text, &summary_quotes(offer))
        {
            return Err(format!("unverified_field:summary:{reason}"));
        }
        let aged = programme.min_age.is_some() || programme.max_age.is_some();
        if audience_narrowed(summary, &offer.audience, aged) {
            return Err("unverified_field:summary:audience_narrowed".into());
        }
    } else {
        displayed_values_evidenced(&displayed, quotes)?;
    }
    let displayed = if verbatim {
        displayed
            .split_once(NOTES_LABEL)
            .map_or(displayed.clone(), |(shown, _)| shown.to_string())
    } else {
        displayed
    };
    let audience: BTreeSet<&str> = offer.audience.iter().copied().collect();
    if super::bootstrap::audience_codes(&displayed)
        .iter()
        .any(|code| !audience.contains(code))
    {
        return refuse("audience");
    }
    if !verbatim {
        displayed_values_bound(&displayed, quotes, &offer.audience)?;
    }
    let page = PageEvidence::from_visible_text(page_text);
    let mut steps = Vec::new();
    if let Some(value) = &programme.application_steps {
        json_strings(value, &mut steps);
    }
    for (field, texts) in [
        (
            "application_method",
            programme
                .application_method
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
        ),
        (
            "eligibility_condition",
            programme
                .eligibility_condition
                .iter()
                .map(|text| condition_page_part(text, offer))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|()| "unverified_field:eligibility_condition".to_string())?,
        ),
        (
            "submission_documents",
            programme.submission_documents.iter().cloned().collect(),
        ),
        (
            "screening_method",
            programme.screening_method.iter().cloned().collect(),
        ),
        ("application_steps", steps),
        (
            "benefit_conditions",
            programme.benefit_conditions.iter().cloned().collect(),
        ),
    ] {
        if texts
            .iter()
            .any(|text| !text.trim().is_empty() && !page.contains(text))
        {
            return refuse(field);
        }
    }
    if programme.application_start_at.is_some() {
        return refuse("application_start_at");
    }
    let kst = chrono::FixedOffset::east_opt(9 * 3600).expect("valid offset");
    let mut any_end = false;
    for (field, at) in [
        ("application_end_at", programme.application_end_at),
        ("deadline_at", programme.deadline_at),
        ("benefit_valid_until", programme.benefit_valid_until),
    ] {
        if let Some(at) = at {
            any_end = true;
            if offer.end_date != Some(at.with_timezone(&kst).date_naive()) {
                return refuse(field);
            }
        }
    }
    if offer.end_date.is_some() && !any_end {
        return refuse("end_date_not_shown");
    }
    let stated: BTreeSet<String> = quotes.iter().flat_map(|q| value_tokens(q)).collect();
    for (field, amount) in [
        ("benefit_amount_monthly", programme.benefit_amount_monthly),
        ("benefit_amount_semester", programme.benefit_amount_semester),
        ("benefit_amount_once", programme.benefit_amount_once),
    ] {
        if let Some(amount) = amount {
            if !stated.contains(&format!("{amount}원")) {
                return refuse(field);
            }
        }
    }
    let shown_age = (programme.min_age, programme.max_age);
    let read_age = offer.age.unwrap_or((None, None));
    if shown_age != read_age {
        return refuse("age");
    }
    Ok(())
}

/// The shown benefit text (§25): the page's own sentences, whitespace
/// normalised, in order, each once.
pub fn verbatim_summary(quotes: &[&str]) -> String {
    let normal: Vec<String> = quotes.iter().map(|q| squash_words(q)).collect();
    let mut kept: Vec<&String> = Vec::new();
    for (index, quote) in normal.iter().enumerate() {
        // A sentence held whole by a longer one is shown once, as the
        // longer one.
        let held = normal.iter().enumerate().any(|(other, longer)| {
            other != index && longer.len() > quote.len() && longer.contains(quote.as_str())
        });
        if !quote.is_empty() && !held && !kept.contains(&quote) {
            kept.push(quote);
        }
    }
    kept.iter()
        .map(|q| q.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Site furniture and markup that must never be shown.
static DISPLAY_FURNITURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bmenu\b|\bsign\s*(?:in|up)\b|\blog\s*in\b|\bskip\s+to\b|\[\[|\]\]|\{\{|\}\}|[<>|]|https?://")
        .expect("valid furniture pattern")
});
/// A question is no statement of an offer.
static DISPLAY_QUESTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\?|？|나요|인가요|까요|\b(?:do|does|can|how)\s+(?:you|i|we)\b|^\s*q\s*[.:)]")
        .expect("valid question pattern")
});

/// True when `text` asks a question ([`DISPLAY_QUESTION`]): a FAQ heading
/// such as `How long can I use it?` is not a statement of the offer.
pub(crate) fn is_question(text: &str) -> bool {
    DISPLAY_QUESTION.is_match(text)
}

/// Opens the original text of the offer's facts in a shown summary (§28):
/// `{Korean summary} 원문: {fact sentences} 유의사항(원문): {conditions}`.
pub const ORIGINAL_LABEL: &str = "원문:";
/// Opens the offer's conditions, word for word, in a shown summary.
pub const NOTES_LABEL: &str = "유의사항(원문):";

/// The Korean word for whom the offer is for.
pub fn korean_audience(audience: &[&str]) -> &'static str {
    if audience.contains(&"youth") {
        "청년"
    } else if audience.contains(&"university_student") {
        "대학생"
    } else if audience.contains(&"graduate") {
        "대학원생"
    } else if audience.contains(&"teen") {
        "청소년"
    } else {
        "학생"
    }
}

/// `amount` with thousands separators when `quote` states that amount as a
/// discount (`10,000원 할인`, `3000 원 할인`), else `None`.
fn won_discount(quote: &str, amount: u64) -> Option<String> {
    static WON_DISCOUNT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\d{1,3}(?:,\d{3})+|\d{3,8})\s*원\s*할인").expect("valid won-discount pattern")
    });
    WON_DISCOUNT
        .captures_iter(quote)
        .any(|c| c[1].replace(',', "").parse::<u64>().ok() == Some(amount))
        .then(|| {
            let digits = amount.to_string();
            let mut out = String::new();
            for (i, d) in digits.chars().enumerate() {
                if i > 0 && (digits.len() - i).is_multiple_of(3) {
                    out.push(',');
                }
                out.push(d);
            }
            out
        })
}

/// The offer's value as a title part and a sentence, from its first fact
/// by kind: a percentage, a free period, free use, a won discount. `Err`
/// when no fact carries a value the template can state.
fn korean_value(offer: &Offer) -> Result<(String, String), &'static str> {
    let fact = |kind: &str| offer.facts.iter().find(|f| f.kind == kind);
    if let Some(n) = fact("discount_percent").and_then(|f| f.value.as_u64()) {
        return Ok((
            format!("{n}% 할인"),
            format!("{n}% 할인 혜택을 받을 수 있습니다."),
        ));
    }
    let term = fact("free_months")
        .and_then(|f| f.value.as_u64())
        .map(|n| super::values::Term::Months(n as u32))
        .or_else(|| {
            fact("free_days")
                .and_then(|f| f.value.as_u64())
                .map(|n| super::values::Term::Days(n as u32))
        })
        .or_else(|| fact("free_enrolled").map(|_| super::values::Term::Enrolled));
    if let Some(term) = term {
        let period = term.korean();
        // A trial is shown as one, with its term (rule 4).
        return Ok(if offer.trial {
            (
                format!("{period} 무료 체험"),
                format!("{period} 동안 무료로 체험할 수 있습니다."),
            )
        } else {
            (
                format!("{period} 무료"),
                format!("{period} 동안 무료로 이용할 수 있습니다."),
            )
        });
    }
    // A won amount is a discount only when its own quote says `N원 할인`;
    // any other amount (a price, a cashback, a support sum) has no template.
    if let Some(won) = offer
        .facts
        .iter()
        .filter(|f| f.kind == "krw")
        .find_map(|f| won_discount(&f.quote, f.value.as_u64()?))
    {
        return Ok((
            format!("{won}원 할인"),
            format!("{won}원 할인 혜택을 받을 수 있습니다."),
        ));
    }
    // A bare free claim never outranks a stated value.
    if fact("free_access").is_some() {
        return Ok(("무료".into(), "무료로 이용할 수 있습니다.".into()));
    }
    Err("template_value_missing")
}

/// The title part after the brand: `{audience} {value}` (`학생 1년 무료`).
pub fn korean_title_tail(offer: &Offer) -> Result<String, &'static str> {
    let (value, _) = korean_value(offer)?;
    Ok(format!("{} {value}", korean_audience(&offer.audience)))
}

/// The value part of an offer's title: [`korean_title_tail`] for a
/// student or youth offer; for one read under brand scope the discount
/// fact's own sentence as the page writes it (code review H-6: "신규 가입
/// 후 첫 구매 시 10% 할인" keeps its condition), without a leading brand
/// name, at most [`super::doors::rows::MAX_LINE_CHARS`] characters.
pub fn offer_title_tail(offer: &Offer) -> Result<String, &'static str> {
    let Some(scope_brand) = offer.scope_brand.as_deref() else {
        return korean_title_tail(offer);
    };
    let fact = offer
        .facts
        .iter()
        .find(|f| matches!(f.kind, "discount_percent" | "krw"))
        .ok_or("no_displayed_value")?;
    let sentence = fact
        .quote
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let sentence = sentence
        .strip_prefix(scope_brand)
        .map_or(sentence.as_str(), str::trim_start)
        .to_string();
    if sentence.is_empty() || sentence.chars().count() > super::doors::rows::MAX_LINE_CHARS {
        return Err("no_displayed_value");
    }
    Ok(sentence)
}

/// The shown title of any offer: [`korean_title`] for a student or youth
/// offer; for one read under brand scope `{issuer} · {brand} {sentence}`
/// (the issuer pays, review H3/H6) or `{brand} {sentence}`, never
/// starting with the redeem brand when an issuer pays.
pub fn offer_title(brand: &str, offer: &Offer) -> Result<String, &'static str> {
    let Some(scope_brand) = offer.scope_brand.as_deref() else {
        return korean_title(brand, offer);
    };
    let tail = offer_title_tail(offer)?;
    let brand = squash_words(scope_brand);
    Ok(match &offer.issuer {
        Some(issuer) => format!("{} · {brand} {tail}", squash_words(&issuer.name)),
        None => format!("{brand} {tail}"),
    })
}

/// The shown title (§28): `{brand} {audience} {value}`, composed from the
/// facts (`Polypane 학생 1년 무료`).
pub fn korean_title(brand: &str, offer: &Offer) -> Result<String, &'static str> {
    let tail = korean_title_tail(offer)?;
    let brand = squash_words(brand);
    Ok(if brand.is_empty() {
        tail
    } else {
        format!("{brand} {tail}")
    })
}

/// The Korean summary (§28), composed from the facts only: whom it is
/// for, the value, the age band, the end date and the verification.
pub fn korean_summary(offer: &Offer) -> Result<String, &'static str> {
    let (_, sentence) = korean_value(offer)?;
    // Brand scope names no audience of its own: the holder an issuer
    // requires (its template, never page text), or no one.
    let mut out = match (&offer.scope_brand, &offer.issuer) {
        (None, _) => format!("{}은 {sentence}", korean_audience(&offer.audience)),
        (Some(_), Some(issuer)) => match mazimi_core::holder::Holder::parse(&issuer.holder) {
            Some(holder) => format!(
                "{}은 {sentence}",
                holder.field.condition_template(&issuer.name)
            ),
            None => return Err("issuer_without_holder_key"),
        },
        (Some(_), None) => sentence,
    };
    match offer.age {
        Some((Some(min), Some(max))) => {
            out.push_str(&format!(" 대상 나이는 만 {min}~{max}세입니다."))
        }
        Some((None, Some(max))) => out.push_str(&format!(" 대상 나이는 만 {max}세 이하입니다.")),
        Some((Some(min), None)) => out.push_str(&format!(" 대상 나이는 만 {min}세 이상입니다.")),
        _ => {}
    }
    if let Some(end) = offer.end_date {
        out.push_str(&format!(
            " {}년 {}월 {}일까지입니다.",
            end.format("%Y"),
            end.format("%-m"),
            end.format("%-d")
        ));
    }
    if let Some(method) = offer.verification.as_ref().and_then(|v| v.value.as_str()) {
        let name = match method {
            "sheerid" => "SheerID",
            "unidays" => "UNiDAYS",
            "student_beans" => "Student Beans",
            "github_student" => "GitHub Student Developer Pack",
            "school_email" => "학교 이메일",
            "student_id" => "학생증",
            _ => "",
        };
        if !name.is_empty() {
            out.push_str(&format!(" 자격 확인: {name}."));
        }
    }
    Ok(out)
}

/// The shown text's checks (§28), on the live page. Content, never layout:
///
/// - the claims (before [`NOTES_LABEL`]) are sentences of the page, name
///   whom the offer is for (one audience), hold every fact's value, and are
///   no question and no site furniture;
/// - every carried condition is a sentence of the page, no furniture;
/// - **no condition of the offer's section is left out**: every restricting
///   clause of [`offer_section`] is in the shown text word for word, a
///   footnote mark in the claims has its footnote shown, and a restriction
///   that cannot be shown word for word (a collapsed panel, alt text, a
///   tooltip) refuses the offer.
pub fn display_invariant(
    summary: &str,
    offer: &Offer,
    page_html: &str,
    page_text: &str,
    facts: &[&str],
) -> Result<(), &'static str> {
    let summary = tidy_spacing(summary);
    let Some((korean, original)) = summary.split_once(ORIGINAL_LABEL) else {
        return Err("no_original_text");
    };
    // The Korean part is exactly the template of the facts.
    if korean.trim() != korean_summary(offer)? {
        return Err("not_the_template");
    }
    let (claims, notes) = match original.split_once(NOTES_LABEL) {
        Some((claims, notes)) => (claims.trim().to_string(), notes.trim().to_string()),
        None => (original.trim().to_string(), String::new()),
    };
    if claims.is_empty() {
        return Err("no_claims");
    }
    let page = PageEvidence::from_visible_text(page_text);
    let section = offer_section(page_html);
    // Whole statements only: the claims and the conditions are each a run
    // of whole sentences of the offer's section (never a window cut out of
    // one), and every one of them is on the live page.
    let whole: BTreeMap<String, String> = section
        .lines
        .iter()
        .flat_map(|line| sentences(line))
        .map(|sentence| (compact_lower(&sentence), sentence))
        .collect();
    for part in [&claims, &notes] {
        let Some(pieces) = whole_sentences(part, &whole) else {
            return Err("not_a_sentence");
        };
        if pieces.iter().any(|piece| !page.contains(piece)) {
            return Err("not_on_page");
        }
    }
    if is_question(&claims) {
        return Err("question");
    }
    if DISPLAY_FURNITURE.is_match(&claims) || DISPLAY_FURNITURE.is_match(&notes) {
        return Err("furniture");
    }
    // The claims name whom the offer is for (a sentence about another
    // programme, `교실에 기술을 통합하는 방법`, does not). An offer read under
    // brand scope is the door brand's for whoever the page says: its claims
    // are its facts' own discount sentences instead.
    if offer.scope_brand.is_none() && super::bootstrap::audience_codes(&claims).is_empty() {
        return Err("no_audience");
    }
    let audiences: BTreeSet<&str> = super::bootstrap::audience_codes(&claims)
        .into_iter()
        .filter(|code| *code != "student")
        .collect();
    if audiences.len() > 1
        && !audiences.is_subset(&BTreeSet::from(["university_student", "graduate"]))
    {
        return Err("audiences");
    }
    let shown_values = value_tokens(&claims);
    if !facts
        .iter()
        .flat_map(|f| value_tokens(f))
        .all(|v| shown_values.contains(&v))
    {
        return Err("facts_missing");
    }
    if !section.uncarried.is_empty() {
        return Err("condition_not_carried");
    }
    let shown = compact_lower(&summary);
    if restrictions(&section.lines)
        .iter()
        .any(|clause| !shown.contains(&compact_lower(clause)))
    {
        return Err("condition_missing");
    }
    for marker in footnote_marks(&claims, &section) {
        let noted = section
            .lines
            .iter()
            .filter(|line| line.trim_start().starts_with(marker.as_str()))
            .any(|line| shown.contains(&compact_lower(line)));
        if !noted {
            return Err("footnote_unresolved");
        }
    }
    Ok(())
}

/// `text` as a run of whole sentences of `whole` (compact, lowercased key
/// to the sentence), or `None`. An empty text is an empty run.
fn whole_sentences(text: &str, whole: &BTreeMap<String, String>) -> Option<Vec<String>> {
    let target: Vec<char> = compact_lower(text).chars().collect();
    let mut lengths: Vec<usize> = whole.keys().map(|k| k.chars().count()).collect();
    lengths.sort_unstable();
    lengths.dedup();
    // best[i]: the sentence ending at i and where it started.
    let mut best: Vec<Option<(usize, String)>> = vec![None; target.len() + 1];
    let mut reached = vec![false; target.len() + 1];
    reached[0] = true;
    for start in 0..target.len() {
        if !reached[start] {
            continue;
        }
        for &len in &lengths {
            let end = start + len;
            if len == 0 || end > target.len() || reached[end] {
                continue;
            }
            let key: String = target[start..end].iter().collect();
            if let Some(sentence) = whole.get(&key) {
                reached[end] = true;
                best[end] = Some((start, sentence.clone()));
            }
        }
    }
    if !reached[target.len()] {
        return None;
    }
    let mut out = Vec::new();
    let mut at = target.len();
    while at > 0 {
        let (start, sentence) = best[at].clone()?;
        out.push(sentence);
        at = start;
    }
    out.reverse();
    Some(out)
}

fn compact_lower(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Sentences of a text: split after `.`, `!`, `?` or `。` followed by a
/// space or the end; a text with none is one sentence.
pub fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        if matches!(c, '.' | '!' | '?' | '。' | '？')
            && chars.peek().is_none_or(|next| next.is_whitespace())
        {
            let piece = current.trim().to_string();
            if !piece.is_empty() {
                out.push(piece);
            }
            current.clear();
        }
    }
    let piece = current.trim().to_string();
    if !piece.is_empty() {
        out.push(piece);
    }
    out
}

/// A phrase that limits an offer to one audience (`대학원생에게만`,
/// `신입생 한정`, `students only`).
static AUDIENCE_LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(대학원생|대학생|재학생|신입생|졸업생|졸업\s*예정자|휴학생|청소년|청년|학생|교직원|교사|교육자|\d\s*학년|graduate students?|undergraduates?|freshmen|students?|teachers?)\s*(?:에게만|에\s*한(?:해|하여|합니다|함)|만\s*(?:해당|적용|가능|이용|신청|대상)|한정|전용|대상에\s*한|\s+only)")
        .expect("valid audience-limit pattern")
});
/// An age bound in the shown text (`만 24세 이하`, `19~24세`).
static AGE_LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\d{1,2}\s*세\s*(?:이하|미만|이상|초과|까지|부터)|\d{1,2}\s*[~\-–]\s*\d{1,2}\s*세|aged?\s+\d{1,2}|under\s+\d{1,2}")
        .expect("valid age-limit pattern")
});

/// Whether the shown text limits the offer to an audience narrower than or
/// other than the one it is listed for (`※ 아래 혜택은 대학원생에게만
/// 적용됩니다` on an offer listed for 대학생), or to an age the programme
/// does not carry. A listing that contradicts its own text is refused.
pub fn audience_narrowed(summary: &str, listed: &[&'static str], has_age: bool) -> bool {
    if AGE_LIMIT.is_match(summary) && !has_age {
        return true;
    }
    AUDIENCE_LIMIT.captures_iter(summary).any(|c| {
        let word = c[1].to_lowercase();
        // A cohort no audience code holds is narrower than any listing.
        if word.contains("신입생")
            || word.contains("졸업")
            || word.contains("휴학생")
            || word.contains("학년")
            || word.contains("freshm")
        {
            return true;
        }
        // The limit must name exactly the listed audience: `대학원생에게만`
        // on a listing for 대학생 (or for 대학생 and 대학원생) is refused.
        let specific = |codes: Vec<&'static str>| -> BTreeSet<&'static str> {
            let narrow: BTreeSet<&str> =
                codes.iter().copied().filter(|c| *c != "student").collect();
            if narrow.is_empty() {
                codes.into_iter().collect()
            } else {
                narrow
            }
        };
        let limited = specific(super::bootstrap::audience_codes(&word));
        let listing = specific(listed.to_vec());
        limited != listing
    })
}

/// A restriction (`직영 매장에서만`, `전용`, `제외`, `할인되지 않습니다`,
/// `not valid`).
static RESTRICTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)전용|한정|에서만|에게만|만\s*(?:사용|이용|적용|가능|해당|인증|신청|구매)|제외|않습니다|않아요|불가|이상|이하|미만|초과|선착순|\bexcept\b|\bexcluding\b|\bexcluded\b|not\s+valid|\bvoid\b")
        .expect("valid restriction pattern")
});
/// `only` as a restriction (`available only to`, `only for`, `students
/// only`, `can only be redeemed`), not as an adverb (`only one step`,
/// `it only has to`) or in a compound (`Mac-only`).
static ONLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[^-\w])only\s+(?:for|to|if|when|with|available|applies|apply|valid|be|on|in|once|one\s+per|eligible|open|redeemable)\b|\b(?:available|valid|applies|open|eligible|redeemable|usable|offered|redeemed)\s+only\b|\bcan\s+only\b|[a-z]\s+only\s*(?:[.,;:)]|$)")
        .expect("valid only pattern")
});
/// Words that name the offer, its event or the person it is for. Korean
/// offer words are the evidence reader's `OFFER_SUBJECT` set; the rest name
/// the eligible person or the licence/plan granted to them.
static OFFER_OR_PERSON: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)이벤트|프로모션|혜택|행사|모집|판매|할인|쿠폰|캠페인|응모|신청|접수|참여|기획전|특가|학생|대학생|대학원생|재학|휴학|졸업|청년|청소년|교직원|교사|교육자|신청자|가입\s*대상|자격|인증|\d+\s*세|\bstudents?\b|\beducators?\b|\bteachers?\b|\bfaculty\b|\bacademic\b|\benroll?ed\b|\benrol(?:l)?ment\b|\bcourses?\b|\binstitutions?\b|\bschools?\b|\buniversit(?:y|ies)\b|\bcolleges?\b|\bapplicants?\b|\bapplying\b|\beligib|\bage\b|\baged\b|\byears?\s+old\b|\bdiscounts?\b|\boffers?\b|\bpromotions?\b|\bdeals?\b|\beducation\s+(?:plan|workspace|account|licen[cs]e|pricing|discount|program)|\bstudent\s+(?:plan|discount|licen[cs]e|pricing|account|membership|pack)|^\s*(?:you(?:['’]ll|\s+will)?\s+(?:need|must|have\s+to)|to\s+(?:qualify|be\s+eligible|apply))\b|\byou(?:['’]re|\s+are)\s+required\s+to\b")
        .expect("valid offer-or-person pattern")
});
/// A clause with no subject of its own, which reads as the offer's: a
/// note lead, or an elliptical restriction (`Not valid on sale items.`,
/// `Limited to one per order.`, `1인 1회 사용할 수 있습니다.`).
static SUBJECTLESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:※|\*|＊|†|‡|\[\d+\]|\d\)|[¹²³⁴⁵⁶⁷⁸⁹]|단[,\s]|다만|유의\s*사항|각주|not\s+valid|valid\b|only\b|limited\b|limit\b|excludes?\b|excluding\b|available\b|must\b|requires?\b|one\s+per\b|maximum\b|subject\s+to\b|applies\b|cannot\b|restricted\b)")
        .expect("valid subjectless pattern")
});
/// A Korean topic or subject marker on a word (`매장 운영 시간은`).
static KO_TOPIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[가-힣](?:은|는|이|가)\s").expect("valid topic pattern"));

/// Whether a clause is about this offer or the person it is for (§28
/// binding): it names the offer, its event or the eligible person, or it
/// has no subject of its own (a note lead, an elliptical restriction, a
/// Korean clause with no topic). A clause about something else on the page
/// (`The interface is only available in English`, `the 3 computer limit`)
/// is no condition of the offer.
///
/// TODO(corporate-audit-fix 2b): replace this body with a call to the
/// evidence reader's shared "clause whose subject is the offer" relation
/// (`resilience::offer_subject`, built on `OFFER_SUBJECT`) once it is
/// public. This is the only place the binding lives on the publish side.
pub fn offer_subject_clause(clause: &str) -> bool {
    if OFFER_OR_PERSON.is_match(clause) || SUBJECTLESS.is_match(clause) {
        return true;
    }
    let korean = clause.chars().any(|c| ('가'..='힣').contains(&c));
    korean && !KO_TOPIC.is_match(clause)
}
/// Further limits: counts, caps, combinations, requirements, a qualifying
/// lead (`1인 1회`, `최대`, `중복 적용 불가`, `단,`, `※`, `not available`).
static LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\d+\s*인\s*\d+\s*회|\d+\s*회\s*(?:한정|제한|까지)|최대|한도|중복|필수|자격|^\s*(?:※|\*|＊|†|단[,\s]|다만|유의\s*사항|각주)|not\s+(?:available|eligible|included|combinable|applicable)|unavailable|cannot|can't|\blimit|\bmust\b|\brequire|\bper\s+(?:customer|person|account)|\bmaximum\b|\bup\s+to\b")
        .expect("valid limit pattern")
});
/// A period with a date (`이벤트 기간: 2026.10.01 ~ 2026.10.31`).
static DATED_PERIOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:20\d{2}\s*[./\-년]\s*\d{1,2}\s*[./\-월]\s*\d{1,2}).*(?:까지|~|∼|종료|마감|until|through)|(?:기간|까지|until|through|ends?)\s*.*20\d{2}\s*[./\-년]\s*\d{1,2}")
        .expect("valid period pattern")
});
/// A footnote reference attached to a word (`혜택*`, `할인¹`, `혜택※1`).
static FOOTNOTE_REF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\S([*＊†‡]+|※\d*|[¹²³⁴⁵⁶⁷⁸⁹]+)").expect("valid footnote pattern")
});

/// Footnote marks in a shown text: a mark attached to a word (`혜택*`) or a
/// `<sup>` of the section (`1)`, `[2]`) that the text carries.
fn footnote_marks(text: &str, section: &OfferSection) -> Vec<String> {
    let mut marks: Vec<String> = FOOTNOTE_REF
        .captures_iter(text)
        .map(|c| c[1].to_string())
        .collect();
    let compact = compact_lower(text);
    marks.extend(
        section
            .sup_marks
            .iter()
            .filter(|mark| compact.contains(&compact_lower(mark)))
            .cloned(),
    );
    marks.sort();
    marks.dedup();
    marks
}

/// Whether a sentence restricts the offer: a restriction, a limit, a
/// qualifying lead or a dated period.
pub fn restricting(sentence: &str) -> bool {
    RESTRICTION.is_match(sentence)
        || ONLY.is_match(sentence)
        || LIMIT.is_match(sentence)
        || DATED_PERIOD.is_match(sentence)
}

/// A condition of the offer: a restricting clause about the offer or the
/// person it is for ([`offer_subject_clause`]).
pub fn offer_condition(sentence: &str) -> bool {
    restricting(sentence) && offer_subject_clause(sentence)
}

/// Every condition of the offer in the section ([`offer_condition`]), in
/// page order, each once.
pub fn restrictions(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        for sentence in sentences(line) {
            if offer_condition(&sentence) && !out.contains(&sentence) {
                out.push(sentence);
            }
        }
    }
    out
}

/// The offer's own section, as shown text.
#[derive(Debug, Clone, Default)]
pub struct OfferSection {
    /// Visible block lines, whitespace normalised, in page order.
    pub lines: Vec<String>,
    /// Restricting text the section holds but cannot be shown word for word:
    /// in a collapsed or hidden element, or in an attribute (alt, title,
    /// aria-label, a tooltip).
    pub uncarried: Vec<String>,
    /// Text of the section's `<sup>` elements: footnote marks.
    pub sup_marks: Vec<String>,
}

const SECTION_BLOCKS: &[&str] = &[
    "p",
    "div",
    "li",
    "ul",
    "ol",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "td",
    "th",
    "tr",
    "dt",
    "dd",
    "dl",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "nav",
    "aside",
    "table",
    "form",
    "blockquote",
    "figcaption",
    "caption",
    "button",
    "label",
    "option",
    "summary",
    "details",
    "body",
];

/// The offer's own section (§28). Reader A refuses a page that holds more
/// than one offer (`multi_offer_page`), so a page that reaches display is a
/// single-offer page and its section is the whole page without the site's
/// own chrome (head, scripts, and `header`/`nav`/`footer` or their roles as
/// direct children of `<body>`).
///
/// TODO(corporate-audit-fix): the shared offer-section definition (a
/// single-offer page is the whole page, otherwise the offer's own section)
/// is being built in the evidence reader; replace this body with a call to
/// its public function once it lands. This is the only place the definition
/// lives on the publish side.
pub fn offer_section(html: &str) -> OfferSection {
    use scraper::{Html, Node};
    let document = Html::parse_document(&super::evidence::strip_comments(html));
    let site_chrome = |node: ego_tree::NodeRef<'_, Node>| {
        std::iter::once(node).chain(node.ancestors()).any(|a| {
            let Some(el) = a.value().as_element() else {
                return false;
            };
            let body_level = a
                .parent()
                .and_then(|p| p.value().as_element().map(|e| e.name() == "body"))
                .unwrap_or(false);
            matches!(
                el.name(),
                "head" | "title" | "script" | "style" | "noscript" | "template" | "svg"
            ) || (body_level
                && (matches!(el.name(), "header" | "nav" | "footer")
                    || matches!(
                        el.attr("role"),
                        Some("navigation" | "banner" | "contentinfo")
                    )))
        })
    };
    // Only a condition of the offer makes hidden or attribute text one that
    // cannot be carried (a link's `aria-label="Terms"`, a hidden menu's
    // `Usage limits` restrict nothing about this offer).
    let notice = offer_condition;
    let mut section = OfferSection::default();
    let mut current = String::new();
    let mut current_block = None;
    for node in document.root_element().descendants() {
        if site_chrome(node) {
            continue;
        }
        if let Some(el) = node.value().as_element() {
            for attr in ["alt", "title", "aria-label", "data-tooltip"] {
                if let Some(value) = el.attr(attr).map(squash_words) {
                    if notice(&value) && !section.uncarried.contains(&value) {
                        section.uncarried.push(value);
                    }
                }
            }
            continue;
        }
        let Some(text) = node.value().as_text() else {
            continue;
        };
        let piece = squash_words(text);
        if piece.is_empty() {
            continue;
        }
        let hidden = node.ancestors().any(|a| {
            a.value()
                .as_element()
                .is_some_and(super::evidence::element_hidden)
        });
        if hidden {
            if notice(&piece) {
                section.uncarried.push(piece);
            }
            continue;
        }
        let in_sup = node
            .ancestors()
            .any(|a| a.value().as_element().is_some_and(|el| el.name() == "sup"));
        if in_sup && piece.chars().count() <= 6 && !section.sup_marks.contains(&piece) {
            section.sup_marks.push(piece.clone());
        }
        let block = node.ancestors().find(|a| {
            a.value()
                .as_element()
                .is_some_and(|el| SECTION_BLOCKS.contains(&el.name()))
        });
        let block = block.map(|b| b.id());
        if block != current_block && !current.is_empty() {
            section
                .lines
                .push(tidy_spacing(&std::mem::take(&mut current)));
        }
        current_block = block;
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(&piece);
    }
    if !current.is_empty() {
        section.lines.push(tidy_spacing(&current));
    }
    section
}

/// Reader A reads a short line together with the next one ("학생 할인" +
/// "10% 할인"), so a quote can open with site chrome ("Menu 학생은 …",
/// "Sign in Talk to sales Get two years …"). The quote is narrowed to the one
/// visible line inside it that names the audience as the benefit's target and
/// keeps every value of the quote; a quote inside one line stays as it is.
/// `None`: no single line holds the fact.
pub fn own_line_quote(quote: &str, lines: &[String]) -> Option<String> {
    let quote = squash_words(quote);
    if lines.iter().any(|line| squash_words(line).contains(&quote)) {
        return Some(quote);
    }
    let values = value_tokens(&quote);
    let free = |text: &str| {
        let lower = text.to_lowercase();
        lower.contains("무료") || lower.contains("free")
    };
    lines
        .iter()
        .map(|line| squash_words(line))
        .filter(|line| {
            !line.is_empty()
                && line.chars().count() <= super::bootstrap::MAX_QUOTE_CHARS
                && quote.contains(line.as_str())
                && super::bootstrap::find_target(line).is_some()
                && values.is_subset(&value_tokens(line))
                && (!free(&quote) || free(line))
        })
        .max_by_key(|line| line.chars().count())
}

/// The whole sentences of `line` that hold `quote` (compared without
/// whitespace).
fn claim_sentences(line: &str, quote: &str) -> Option<String> {
    let parts = sentences(line);
    let compact = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let needle = compact(quote);
    if needle.is_empty() {
        return None;
    }
    let mut starts = Vec::new();
    let mut joined = String::new();
    for part in &parts {
        starts.push(joined.chars().count());
        joined.push_str(&compact(part));
    }
    let byte = joined.find(&needle)?;
    let start = joined[..byte].chars().count();
    let end = start + needle.chars().count();
    let first = starts.iter().rposition(|s| *s <= start)?;
    let last = starts.iter().rposition(|s| *s < end)?;
    Some(parts[first..=last].join(" "))
}

/// The whole sentences of consecutive section lines that hold `quote`
/// (reader A reads a short heading line together with the next line).
fn covering_sentences(lines: &[String], quote: &str) -> Option<Vec<String>> {
    let compact = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let needle = compact(quote);
    if needle.is_empty() {
        return None;
    }
    // Every sentence of every line, with its start in the joined text.
    let mut pieces: Vec<(usize, String)> = Vec::new();
    let mut joined = String::new();
    for line in lines {
        for sentence in sentences(line) {
            pieces.push((joined.chars().count(), sentence.clone()));
            joined.push_str(&compact(&sentence));
        }
    }
    let byte = joined.find(&needle)?;
    let start = joined[..byte].chars().count();
    let end = start + needle.chars().count();
    let first = pieces.iter().rposition(|(s, _)| *s <= start)?;
    let last = pieces.iter().rposition(|(s, _)| *s < end)?;
    Some(
        pieces[first..=last]
            .iter()
            .map(|(_, s)| s.clone())
            .collect(),
    )
}

/// The shown summary (§28): the Korean summary composed from the facts
/// ([`korean_summary`]), then [`ORIGINAL_LABEL`] and the whole page
/// sentences that state each fact, then [`NOTES_LABEL`] and every condition
/// of the offer ([`offer_condition`]) in its section, word for word. Refused only when a fact is not in the
/// section (`fact_missing`), when a restriction cannot be shown word for
/// word (`condition_not_carried`) or a footnote mark has no footnote
/// (`footnote_unresolved`). Checked again by [`display_invariant`].
pub fn compose_display(html: &str, offer: &Offer) -> Result<String, &'static str> {
    let section = offer_section(html);
    if !section.uncarried.is_empty() {
        return Err("condition_not_carried");
    }
    let mut claims: Vec<String> = Vec::new();
    for quote in summary_quotes(offer) {
        let found = match own_line_quote(quote, &section.lines) {
            Some(own) => section
                .lines
                .iter()
                .find_map(|line| claim_sentences(line, &own))
                .map(|claim| vec![claim]),
            None => None,
        }
        .or_else(|| covering_sentences(&section.lines, quote))
        .ok_or("fact_missing")?;
        for claim in found {
            if !claims.contains(&claim) {
                claims.push(claim);
            }
        }
    }
    let claim_refs: Vec<&str> = claims.iter().map(String::as_str).collect();
    let claims_text = verbatim_summary(&claim_refs);
    let shown = compact_lower(&claims_text);
    let mut notes: Vec<String> = Vec::new();
    for clause in restrictions(&section.lines) {
        if !shown.contains(&compact_lower(&clause)) && !notes.contains(&clause) {
            notes.push(clause);
        }
    }
    for marker in footnote_marks(&claims_text, &section) {
        let footnote = section
            .lines
            .iter()
            .find(|line| line.trim_start().starts_with(marker.as_str()))
            .ok_or("footnote_unresolved")?;
        if !notes.contains(footnote) && !shown.contains(&compact_lower(footnote)) {
            notes.push(footnote.clone());
        }
    }
    let korean = korean_summary(offer)?;
    Ok(if notes.is_empty() {
        format!("{korean} {ORIGINAL_LABEL} {claims_text}")
    } else {
        format!(
            "{korean} {ORIGINAL_LABEL} {claims_text} {NOTES_LABEL} {}",
            notes.join(" ")
        )
    })
}

/// Words squashed to single spaces, with no space before closing
/// punctuation (a link's text ends `portal .` in the page text).
fn tidy_spacing(text: &str) -> String {
    static BEFORE_PUNCT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r" ([.,!?)\]。])(\s|$)").expect("valid spacing pattern"));
    BEFORE_PUNCT
        .replace_all(&squash_words(text), "$1$2")
        .into_owned()
}

/// Not written by `publish` (§26: a corporate offer never carries an
/// amount); kept as the reviewer probes' read of the numeric grammar.
/// A won value whose sentence
/// both extractors read alike, that no negation, threshold, cap or range
/// touches, whose own clause fits the positive grammar, with a known
/// frequency (monthly or once). One value per column; anything else NULL.
pub fn grammar_amounts(quotes: &[&str]) -> (Option<i32>, Option<i32>) {
    let (mut monthly, mut once): (Vec<i32>, Vec<i32>) = (Vec::new(), Vec::new());
    for quote in quotes {
        if !super::resilience::extractors_agree(quote)
            || VALUE_NEGATED.is_match(quote)
            || VALUE_CONDITIONED.is_match(quote)
            || super::resilience::value_range_b(quote)
        {
            continue;
        }
        let joined = DIGIT_COMMA.replace_all(quote, "$1$2");
        for clause in VALUE_CLAUSE_BREAK.split(&joined) {
            for value in value_tokens(clause) {
                let Some(won) = value.strip_suffix('원').and_then(|n| n.parse::<i32>().ok())
                else {
                    continue;
                };
                if clause_kind(clause, &value).is_none() {
                    continue;
                }
                let frequency = frequency_of(clause);
                if frequency == BTreeSet::from(["monthly"]) {
                    monthly.push(won);
                } else if frequency == BTreeSet::from(["once"]) {
                    once.push(won);
                }
            }
        }
    }
    let single = |mut values: Vec<i32>| {
        values.sort_unstable();
        values.dedup();
        (values.len() == 1).then(|| values[0])
    };
    (single(monthly), single(once))
}

/// Gates 3–4 and the restore, inside the attempt's transaction. Gate 1
/// (live) and gate 2 (scan clean) are decided by the caller. Everything the
/// publish writes runs in a savepoint that is rolled back unless the
/// programme row was actually restored.
pub async fn publish(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    offer: &Offer,
    page_hash: Option<&str>,
    page_text: Option<&str>,
    page_html: Option<&str>,
    page_url: &str,
) -> Result<Published> {
    // Every programme tied to the source: any non-retired monitor in any
    // verification mode, and any live programme that links the source URL.
    let tied: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT id FROM (
             SELECT m.program_id AS id FROM program_source_monitors m
             WHERE m.candidate_id = $1 AND m.verification_state <> 'retired'
             UNION
             SELECT p.id FROM programs p JOIN benefit_source_candidates c ON c.id = $1
             WHERE p.retirement_reason IS NULL
               AND rtrim(p.official_url, '/') IN (rtrim(c.url, '/'), rtrim(c.canonical_url, '/'))
         ) tied ORDER BY id",
    )
    .bind(candidate_id)
    .fetch_all(&mut **tx)
    .await?;
    let [program_id] = tied.as_slice() else {
        return Ok(Published::Refused(format!(
            "programmes_on_source:{}",
            tied.len()
        )));
    };
    let program_id = *program_id;
    let evidence_auto: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM program_source_monitors WHERE program_id = $1
                        AND candidate_id = $2 AND verification_mode = 'evidence_auto'
                        AND verification_state <> 'retired')",
    )
    .bind(program_id)
    .bind(candidate_id)
    .fetch_one(&mut **tx)
    .await?;
    if !evidence_auto {
        return Ok(Published::Refused("not_evidence_auto".into()));
    }
    // The opt-out's lock order: every programme on the source, by id,
    // before any read that decides or any write (review M4). The state is
    // read after the lock, so a concurrent opt-out that committed first is
    // seen.
    super::optout::lock_programmes(tx, &[candidate_id]).await?;
    // Review M3: an open opt-out covering this source refuses publishing by
    // itself, in any scope, whether or not it placed a hold here (a
    // display-only opt-out, or a programme created after it). Matched as
    // intake matches: the opt-out's source domains (or the host under its
    // domain), and the URL prefix on the source or the page read.
    let Some((retired, optout)) = sqlx::query_as::<_, (bool, bool)>(
        "SELECT (p.retirement_reason IS NOT NULL OR p.program_status = 'closed'),
                EXISTS (SELECT 1 FROM program_source_holds h WHERE h.program_id = p.id
                        AND h.resolved_at IS NULL AND h.hold_reason = 'publisher_optout')
                OR EXISTS (
                    SELECT 1 FROM benefit_source_optouts o
                    JOIN benefit_source_candidates c ON c.id = $2
                    LEFT JOIN benefit_source_domains d ON d.id = c.domain_id
                    WHERE o.lifted_at IS NULL AND o.effective_at IS NOT NULL
                      AND CASE WHEN o.url_prefix IS NULL
                               THEN c.domain_id = ANY(o.domain_ids)
                                    OR (o.domain IS NOT NULL AND d.domain IS NOT NULL
                                        AND (d.domain = o.domain OR right(d.domain, length(o.domain) + 1) = '.' || o.domain))
                               ELSE starts_with(c.url, o.url_prefix) OR starts_with(c.canonical_url, o.url_prefix)
                                    OR starts_with($3, o.url_prefix)
                          END)
         FROM programs p WHERE p.id = $1",
    )
    .bind(program_id)
    .bind(candidate_id)
    .bind(page_url)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(Published::Refused("programme_missing".into()));
    };
    if retired {
        return Ok(Published::Refused("programme_retired".into()));
    }
    if optout {
        return Ok(Published::Refused("publisher_optout".into()));
    }
    let programme = sqlx::query_as::<_, PublicProgramme>(
        "SELECT p.program_type, p.title, p.summary, p.official_url, p.application_method, p.eligibility_condition,
                p.submission_documents, p.screening_method, p.application_steps,
                p.normalized_payload->>'conditions' AS benefit_conditions,
                p.application_start_at, p.application_end_at, p.deadline_at, p.benefit_valid_until,
                p.benefit_amount_monthly, p.benefit_amount_semester, p.benefit_amount_once,
                p.min_age, p.max_age, p.regions, p.region_districts, p.region_scope,
                p.school_scope, p.tags
         FROM programs p WHERE p.id = $1",
    )
    .bind(program_id)
    .fetch_one(&mut **tx)
    .await?;
    if programme.program_type != "corporate_benefit" {
        return Ok(Published::Refused(format!(
            "program_type:{}",
            programme.program_type
        )));
    }
    let mut quotes: Vec<&str> = Vec::new();
    for fact in offer.facts.iter().chain(offer.verification.iter()) {
        if !quotes.contains(&fact.quote.as_str()) {
            quotes.push(&fact.quote);
        }
    }
    let side = side_rows(tx, program_id).await?;
    if !side.is_empty() {
        return Ok(Published::SideData(side));
    }
    let Some(page_text) = page_text else {
        return Ok(Published::Refused("page_text_missing".into()));
    };
    // §25: the offer is shown as the page states it. The title is the
    // brand and the audience word, the summary the page's own sentences,
    // and an amount only where the grammar and both extractors agree.
    let brand: String = sqlx::query_scalar(
        "SELECT COALESCE(NULLIF(btrim(provider_name), ''), NULLIF(btrim(company_name), ''), '') FROM programs WHERE id = $1",
    )
    .bind(program_id)
    .fetch_one(&mut **tx)
    .await?;
    // §28: the shown text is the facts' own sentences and every
    // restricting clause of the offer's section; a corporate offer never
    // carries an amount (discounts live in the
    // sentence and stay out of support-money sums).
    let Some(page_html) = page_html else {
        return Ok(Published::Refused("page_html_missing".into()));
    };
    let display = match compose_display(page_html, offer) {
        Ok(text) => text,
        Err(reason) => return Ok(Published::Refused(format!("display:{reason}"))),
    };
    let mut shown = programme.clone();
    shown.title = match offer_title(&brand, offer) {
        Ok(title) => title,
        Err(reason) => return Ok(Published::Refused(format!("display:{reason}"))),
    };
    shown.summary = Some(display);
    shown.benefit_amount_monthly = None;
    shown.benefit_amount_semester = None;
    shown.benefit_amount_once = None;
    if let Err(reason) =
        verify_verbatim_fields(&shown, offer, &quotes, page_html, page_text, page_url)
    {
        return Ok(Published::Refused(reason));
    }

    let mut savepoint = tx.begin().await?;
    match write_and_restore(
        &mut savepoint,
        candidate_id,
        program_id,
        &quotes,
        page_hash,
        &shown,
    )
    .await?
    {
        Ok(()) => {
            // The title changed: store its 기업혜택 chip with the write.
            mazimi_core::corporate_categories::refresh(&mut savepoint, Some(&[program_id]), 1)
                .await?;
            savepoint.commit().await?;
            Ok(Published::Restored {
                program_id,
                facts: quotes.len(),
            })
        }
        Err(reason) => {
            savepoint.rollback().await?;
            Ok(Published::Refused(reason))
        }
    }
}

/// Replaces the facts and restores the programme. The inner `Err` is a
/// restore that did not happen; the caller rolls everything back.
async fn write_and_restore(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    program_id: Uuid,
    quotes: &[&str],
    page_hash: Option<&str>,
    shown: &PublicProgramme,
) -> Result<std::result::Result<(), String>> {
    let shown_written = sqlx::query(
        "UPDATE programs SET title = $2, summary = $3, benefit_amount_monthly = NULL,
                benefit_amount_semester = NULL, benefit_amount_once = NULL,
                normalized_payload = COALESCE(normalized_payload, '{}'::jsonb)
                    || jsonb_build_object('display', 'verbatim'),
                updated_at = now()
         WHERE id = $1",
    )
    .bind(program_id)
    .bind(&shown.title)
    .bind(&shown.summary)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if shown_written != 1 {
        return Ok(Err("restore_not_applied:display".into()));
    }
    // The new evidence replaces the old; old facts are retired, not deleted.
    let old_quotes: Vec<String> = sqlx::query_scalar(
        "UPDATE corporate_offer_facts SET status = 'retired', last_outcome = 'superseded_by_reextract',
                updated_at = now()
         WHERE program_id = $1 AND status <> 'retired' RETURNING quote",
    )
    .bind(program_id)
    .fetch_all(&mut **tx)
    .await?;
    // Contract §16: no window, no shadow window, no fingerprint (and so no
    // page digest); the next evidence run adopts the fact under the page it
    // reads. Only the replaced quotes are kept, so the audit seed never
    // re-inserts them.
    let anchors = json!({"previous_quotes": old_quotes});
    for quote in quotes {
        let (fact_type, value) = read_quote(quote);
        let written = sqlx::query(
            "INSERT INTO corporate_offer_facts (program_id, fact_type, value, quote, quote_key, channel,
                 evidence_source, pass_b, status, adopt_before, last_confirmed_at, anchors)
             VALUES ($1, $2, $3, $4, $5, 'page', 'reextract', $6, 'published',
                     now() + make_interval(days => $7), now(), $8)
             ON CONFLICT (program_id, quote_key) DO UPDATE SET
                 status = 'published', evidence_source = 'reextract', window_signature = NULL,
                 shadow_window_signature = NULL, adopt_before = EXCLUDED.adopt_before,
                 last_confirmed_at = now(), last_outcome = NULL, anchors = EXCLUDED.anchors,
                 updated_at = now()",
        )
        .bind(program_id)
        .bind(fact_type)
        .bind(&value)
        .bind(quote)
        .bind(quote_key(program_id, quote))
        .bind(json!({"source": "reextract", "reading": value}))
        .bind(ADOPT_WITHIN_DAYS as i32)
        .bind(&anchors)
        .execute(&mut **tx)
        .await?
        .rows_affected();
        if written != 1 {
            return Ok(Err("restore_not_applied:fact".into()));
        }
    }
    restore(tx, candidate_id, program_id, page_hash).await
}

/// The `benefit_source_checks.error_summary` of a fresh-extraction
/// publish; the evidence job counts them for its flap breaker.
pub(crate) const REPUBLISHED_SUMMARY: &str = "fresh extraction published";

/// The evidence job's restore of a confirmed programme
/// (`evidence::confirm_program` + `restore_confirmed`), scoped to one
/// programme. Those functions are private to `evidence.rs`; keep this in
/// step with them, or call them once they are exposed. Every statement that
/// must change a row is checked; the inner `Err` names the one that did not.
async fn restore(
    tx: &mut Transaction<'_, Postgres>,
    candidate_id: Uuid,
    program_id: Uuid,
    page_hash: Option<&str>,
) -> Result<std::result::Result<(), String>> {
    let keys: Vec<String> = sqlx::query_scalar(
        "SELECT quote_key FROM corporate_offer_facts WHERE program_id = $1 AND status <> 'retired' ORDER BY quote_key",
    )
    .bind(program_id)
    .fetch_all(&mut **tx)
    .await?;
    let set_hash = hex(&Sha256::digest(keys.join(",").as_bytes()));
    sqlx::query(
        "INSERT INTO benefit_source_checks (candidate_id, check_kind, content_hash, error_summary, review_status)
         VALUES ($1, 'unchanged', $2, $3, 'not_required')",
    )
    .bind(candidate_id)
    .bind(page_hash)
    .bind(REPUBLISHED_SUMMARY)
    .execute(&mut **tx)
    .await?;
    let monitors = sqlx::query(
        "UPDATE program_source_monitors
         SET verification_state = 'current', evidence_page_hash = $3, evidence_verified_at = now(),
             verified_at = now(), review_due_at = now() + make_interval(days => $5),
             evidence_set_hash = $4, change_detected_at = NULL, last_error = NULL, updated_at = now()
         WHERE program_id = $1 AND candidate_id = $2 AND verification_mode = 'evidence_auto'",
    )
    .bind(program_id)
    .bind(candidate_id)
    .bind(page_hash)
    .bind(set_hash)
    .bind(EVIDENCE_MAX_AGE_DAYS as i32)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if monitors != 1 {
        return Ok(Err("restore_not_applied:monitor".into()));
    }
    sqlx::query(
        "UPDATE benefit_source_candidates
         SET status = 'approved', last_successful_check_at = now(), last_checked_at = now(),
             consecutive_failures = 0, last_error = NULL,
             next_check_at = now() + make_interval(days => $2), updated_at = now()
         WHERE id = $1 AND status IN ('approved', 'change_review', 'unreachable_review', 'verification_due', 'fetched', 'error')",
    )
    .bind(candidate_id)
    .bind(EVIDENCE_MAX_AGE_DAYS as i32)
    .execute(&mut **tx)
    .await?;
    crate::source_monitoring::refresh_program_health(tx, candidate_id).await?;
    let restored: Vec<Uuid> = sqlx::query_scalar(
        "WITH recoverable AS (
             SELECT p.id, bool_or(h.previous_is_active) AS active,
                    min(h.previous_program_status) FILTER (WHERE h.previous_is_active) AS status
             FROM programs p
             JOIN program_source_holds h ON h.program_id = p.id AND h.resolved_at IS NULL
             WHERE p.id = $1 AND p.source_health = 'current' AND p.program_status = 'review_required'
               AND NOT p.is_active AND p.retirement_reason IS NULL
               AND (LEAST(p.benefit_valid_until, p.application_end_at, p.deadline_at) IS NULL
                    OR LEAST(p.benefit_valid_until, p.application_end_at, p.deadline_at) > now())
               AND NOT EXISTS (SELECT 1 FROM program_source_monitors m
                               WHERE m.program_id = p.id AND m.verification_state NOT IN ('current', 'retired'))
               AND NOT EXISTS (SELECT 1 FROM program_source_holds o
                               WHERE o.program_id = p.id AND o.resolved_at IS NULL
                                 AND o.hold_reason = ANY($2))
             GROUP BY p.id
         )
         UPDATE programs p
         SET is_active = r.active, program_status = COALESCE(r.status, 'review_required'), updated_at = now()
         FROM recoverable r WHERE p.id = r.id AND r.active
         RETURNING p.id",
    )
    .bind(program_id)
    .bind(super::reverify::ONLY_OWN_FLOW_LIFTS)
    .fetch_all(&mut **tx)
    .await?;
    if restored.as_slice() != [program_id] {
        return Ok(Err("restore_not_applied".into()));
    }
    let released = sqlx::query(
        "UPDATE program_source_holds h
         SET resolved_at = now(), resolution = 'confirmed_unchanged',
             resolution_note = 'Automatically restored: freshly extracted verbatim evidence on the live official page'
         FROM programs p
         WHERE p.id = h.program_id AND p.id = $1 AND h.resolved_at IS NULL AND p.is_active
           AND p.source_health = 'current'
           AND h.hold_reason <> ALL($2)",
    )
    .bind(program_id)
    .bind(super::reverify::ONLY_OWN_FLOW_LIFTS)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if released == 0 {
        return Ok(Err("restore_not_applied:holds".into()));
    }
    super::events::resolve_for_candidate(tx, candidate_id, "confirmed").await?;
    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displayed_values_are_bound_to_audience_kind_and_frequency() {
        let ok = |shown: &str, quotes: &[&str]| displayed_values_bound(shown, quotes, &[]).is_ok();
        assert!(ok("대학생 10% 할인", &["대학생 인증 시 10% 할인"]));
        assert!(ok("월 1만 원 할인", &["매월 1만원 할인 쿠폰을 드립니다"]));
        assert!(ok("10% 할인", &["학생 10% 할인"]));
        assert!(!ok(
            "대학원생 10% 할인",
            &["대학생 10% 할인", "대학원생 20% 할인"]
        ));
        assert!(!ok("대학생 10% 할인", &["결제 금액의 10% 적립"]));
        assert!(!ok("대학생 50% 할인 + 첫 달 무료", &["대학생 50% 할인"]));
        assert!(!ok("매월 5,000원 할인", &["가입 시 1회 5,000원 할인 쿠폰"]));
        assert!(!ok("학생 10% 할인", &["대학원생 전용 10% 할인"]));
        // A quote naming no audience leans on the page's audience.
        assert!(displayed_values_bound(
            "대학생 10% 할인",
            &["10% 할인"],
            &["university_student", "student"]
        )
        .is_ok());
        assert!(displayed_values_bound("대학생 10% 할인", &["10% 할인"], &["youth"]).is_err());
    }

    /// The positive grammar (§24): a value publishes only when its own
    /// clause is `[audience] [frequency] [how] value kind [benefit words]`.
    #[test]
    fn a_value_publishes_only_through_the_positive_grammar() {
        let a = ["university_student", "student"];
        let ok = |shown: &str, quote: &str| displayed_values_bound(shown, &[quote], &a).is_ok();
        assert!(ok(
            "대학생 10% 할인",
            "대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다."
        ));
        assert!(ok(
            "대학생 1만 원 할인",
            "대학생은 학생증 제시 시 전 메뉴 10,000원 할인 혜택을 받을 수 있습니다."
        ));
        assert!(ok(
            "대학생 매월 5,000원 할인",
            "대학생 인증 시 매월 5,000원 할인 쿠폰을 드립니다"
        ));
        for (shown, quote) in [
            ("대학생 10% 할인", "대학생 노트북 구매 시 10% 할인"),
            ("대학생 3,000원 할인", "대학생 3,000원 이상 구매 시 할인"),
            ("대학생 20% 할인", "대학생 최대 20% 할인"),
            ("대학생 10% 할인", "대학생 10% 할인이 종료되었습니다"),
            (
                "대학생 10% 할인",
                "대학생 10% 할인은 10월부터 15% 할인으로 바뀝니다",
            ),
        ] {
            assert!(!ok(shown, quote), "{quote}");
        }
    }

    /// §25: the shown title has no value; amounts are filled only through
    /// the grammar, both extractors and a known frequency.
    #[test]
    fn the_verbatim_display_parts() {
        assert_eq!(
            verbatim_summary(&[
                "대학생  10% 할인",
                "학생 혜택 대학생  10% 할인",
                "Students get 10% off"
            ]),
            "학생 혜택 대학생 10% 할인 Students get 10% off"
        );
        assert_eq!(
            grammar_amounts(&["대학생 인증 시 매월 5,000원 할인 쿠폰을 드립니다"]),
            (Some(5000), None)
        );
        assert_eq!(
            grammar_amounts(&["대학생 최초 1회 5,000원 할인 쿠폰"]),
            (None, Some(5000))
        );
        for quote in [
            "대학생 5,000원 할인 쿠폰 (3만원 이상 구매 시)",
            "대학생 5,000원 할인",
            "대학생 매월 5,000원 할인이 종료되었습니다",
            "대학생 매월 1만~2만원 할인",
        ] {
            assert_eq!(grammar_amounts(&[quote]), (None, None), "{quote}");
        }
    }

    #[test]
    fn value_tokens_are_normalised() {
        assert_eq!(
            value_tokens("월 1만 원 할인"),
            value_tokens("월 10,000원 할인")
        );
        assert_eq!(value_tokens("3천원 적립"), value_tokens("3,000원 적립"));
        assert_eq!(value_tokens("10 % 할인"), value_tokens("10% 할인"));
        assert_ne!(value_tokens("1만 원"), value_tokens("1,000원"));
        assert_eq!(value_tokens("1.5만원"), value_tokens("15,000원"));
        assert_eq!(value_tokens("1만 5천원"), value_tokens("15,000원"));
    }

    #[test]
    fn every_displayed_value_must_be_stated() {
        assert!(
            displayed_values_evidenced("대학생 10% 할인", &["학생증 제시 시 10% 할인"]).is_ok()
        );
        assert!(
            displayed_values_evidenced("월 1만 원 할인", &["대학생 월 10,000원 할인 혜택"]).is_ok()
        );
        assert_eq!(
            displayed_values_evidenced("대학생 10% 할인", &["학생 20% 할인"]),
            Err("values_differ:10%".into())
        );
        assert_eq!(
            displayed_values_evidenced("학생 무료", &["학생 무료 이용"]),
            Err("no_displayed_value".into())
        );
    }
}

#[cfg(test)]
mod r5_gate {
    use super::*;
    use crate::corporate_auto::bootstrap::extract_offer;
    use crate::corporate_auto::evidence::visible_text;

    fn page(body: &str) -> String {
        format!("<html><head><title>t</title></head><body><main><h1>대학생 할인 프로그램</h1>{body}<div class=\"about\"><p>본 프로그램은 대학생과 대학원생을 위한 학생 할인 혜택입니다. 자세한 사항은 매장 직원에게 문의하시기 바랍니다. 학생 할인은 온라인 스토어와 오프라인 매장에서 이용할 수 있으며 학생증으로 재학 여부를 확인합니다.</p><p>매장 운영 시간은 매일 오전 9시부터 오후 10시까지이며 명절 당일에는 일부 매장이 휴무할 수 있습니다. 온라인 주문은 연중무휴로 가능하며 배송은 영업일 기준 2~3일이 소요됩니다. 고객센터는 평일 오전 9시부터 오후 6시까지 운영합니다.</p><p>매장 위치와 운영 정보는 매장 찾기 메뉴에서 확인하실 수 있습니다. 회원 가입은 무료이며 누구나 앱에서 가입할 수 있습니다. 개인정보 처리방침과 이용약관은 하단 링크에서 확인할 수 있습니다.</p></div><p>{}</p></main></body></html>", "매장과 온라인에서 이용할 수 있는 안내 문구입니다. ".repeat(20))
    }

    /// What the live path (`publish`) would show: whole blocks plus notes,
    /// verified on the page text, never an amount.
    fn live(brand: &str, html: &str) -> Result<String, String> {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 19).expect("date");
        let offer = extract_offer(html, today).map_err(|e| format!("extract:{e}"))?;
        let mut quotes: Vec<&str> = Vec::new();
        for f in offer.facts.iter().chain(offer.verification.iter()) {
            if !quotes.contains(&f.quote.as_str()) {
                quotes.push(&f.quote);
            }
        }
        if !matches!(page_blocked_scan(html, &quotes), ScanVerdict::Clean) {
            return Err("scan".into());
        }
        let display = compose_display(html, &offer).map_err(|r| format!("display:{r}"))?;
        let programme = PublicProgramme {
            program_type: "corporate_benefit".into(),
            title: korean_title(brand, &offer).map_err(|r| format!("title:{r}"))?,
            summary: Some(display.clone()),
            official_url: Some("https://shop.example.com/student".into()),
            application_method: None,
            eligibility_condition: None,
            submission_documents: None,
            screening_method: None,
            application_steps: None,
            benefit_conditions: None,
            application_start_at: None,
            application_end_at: None,
            deadline_at: None,
            benefit_valid_until: None,
            benefit_amount_monthly: None,
            benefit_amount_semester: None,
            benefit_amount_once: None,
            min_age: None,
            max_age: None,
            regions: None,
            region_districts: None,
            region_scope: None,
            school_scope: None,
            tags: None,
        };
        verify_verbatim_fields(
            &programme,
            &offer,
            &quotes,
            html,
            &visible_text(html),
            "https://shop.example.com/student",
        )?;
        Ok(display)
    }

    /// The r5 probe, run verbatim: every window summary followed by a note,
    /// cut mid-line or mixing audiences is refused; the digit brand passes.
    #[test]
    fn r5_probe_verdicts() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "corporate_auto::publish::probe_r5_pub::probe_r5_verbatim",
                "--nocapture",
                "--test-threads=1",
            ])
            .output()
            .expect("child test run");
        assert!(output.status.success(), "{output:?}");
        let out = String::from_utf8_lossy(&output.stdout).into_owned();
        let line = |name: &str| {
            out.lines()
                .find_map(|l| {
                    l.find(&format!("R5V {name} "))
                        .map(|at| l[at..].to_string())
                })
                .unwrap_or_else(|| panic!("{name} missing: {out}"))
        };
        for name in [
            "V1_condition_next_block",
            "V2_long_line_cut",
            "V3_monthly_amount_cap_next",
            "V4_other_audience_section",
            "V6_exclusion_next",
            "V7_member_only_next",
            "V8_period_next_future",
        ] {
            let l = line(name);
            assert!(
                l.contains("verify=Err") || l.contains("extract_err") || !l.contains("scan=Clean"),
                "{l}"
            );
        }
        // V5: a window summary has no Korean template part: §28 refuses it
        // (`no_original_text`). The digit brand is the `11번가` control of
        // `r5_r6_live_refuse_or_show_the_condition`.
        assert!(
            line("V5_brand_with_digit").contains("no_original_text"),
            "{out}"
        );
    }

    /// The r7 probe, run verbatim (§28): a restriction anywhere in the
    /// offer's section (an aside, a form, a nav or a header inside the
    /// content, another heading or column, a footnote) is either shown word
    /// for word or the offer refuses; one that cannot be shown (a collapsed
    /// panel, an inactive tab, alt text, a tooltip) always refuses; the two
    /// controls publish.
    #[test]
    fn r7_probe_verdicts() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "corporate_auto::publish::probe_r7::probe_r7_structure",
                "--nocapture",
                "--test-threads=1",
            ])
            .output()
            .expect("child test run");
        assert!(output.status.success(), "{output:?}");
        let out = String::from_utf8_lossy(&output.stdout).into_owned();
        let lines: Vec<&str> = out
            .lines()
            .filter_map(|l| l.find("R7 ").map(|at| &l[at..]))
            .collect();
        assert_eq!(lines.len(), 22, "{out}");
        let online = Some("※ 학생 할인은 온라인 스토어 전용입니다.");
        let outlet = Some("아울렛 상품은 할인되지 않습니다");
        let direct = Some("직영 매장에서만 사용할 수 있습니다.");
        // (case, the condition it must show; None = must refuse)
        let cases: [(&str, Option<&str>); 22] = [
            ("T1b_aside_before", online),
            ("T1c_aside_after_with_gap", online),
            ("T3b_hidden_accordion_gap", None),
            ("T13b_role_button_gap", outlet),
            ("T15b_nav_gap", outlet),
            ("T16_footer_page_terms", Some("")),
            ("C0_control", Some("")),
            ("T1_aside_callout", online),
            (
                "T2_article_footer",
                Some("※ 아울렛 매장은 할인되지 않습니다."),
            ),
            ("T3_hidden_accordion_outside", None),
            ("T4_inactive_tab", None),
            ("T5_image_notice", None),
            ("T6_no_listed_word_other_heading", direct),
            ("T7_footnote_id_link", direct),
            ("T8_div_headings", outlet),
            ("T9_details_outside", outlet),
            ("T10_form_condition", Some("재학생만 인증할 수 있습니다.")),
            ("T11_header_hero", Some("※ 본 혜택은 신규 회원 전용입니다.")),
            ("T12_column_split", direct),
            ("T13_role_button_notice", outlet),
            ("T14_tooltip_condition", None),
            ("T15_nav_inside_main_condition", outlet),
        ];
        for (name, condition) in cases {
            let line = lines
                .iter()
                .find(|l| l.starts_with(&format!("R7 {name} ")))
                .unwrap_or_else(|| panic!("{name} missing"));
            let shown = line.contains("=> Ok(");
            match condition {
                None => assert!(!shown, "{line}"),
                Some(cond) => {
                    if name == "C0_control" || name == "T16_footer_page_terms" {
                        assert!(shown, "{line}");
                    }
                    if shown {
                        assert!(line.contains(cond), "{name} hid its condition: {line}");
                    }
                }
            }
        }
    }

    /// The r5 and r6 cases through the live path (§28), on the probe page
    /// and on a bare page that holds only the case: refuse, or show the
    /// condition in the shown text word for word.
    #[test]
    fn r5_r6_live_refuse_or_show_the_condition() {
        let bare = |body: &str| {
            format!(
                "<html><head><title>t</title></head><body><nav><a href=\"/\">홈</a></nav><main>{body}<h2>매장 정보</h2><p>{}</p></main><footer><a href=\"/terms\">이용약관</a></footer></body></html>",
                "매장 운영 시간과 위치는 지점마다 다를 수 있으니 방문 전에 확인해 주세요. ".repeat(12)
            )
        };
        let cases: Vec<(&str, &str, &str, Option<&str>)> = vec![
            ("V1", "예시샵", "<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>※ 3만원 이상 구매 시에만 적용됩니다.</p>", Some("※ 3만원 이상 구매 시에만 적용됩니다.")),
            ("V2", "예시샵", "<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있으며 이 혜택은 매장을 방문하시는 모든 고객님을 위한 특별 프로모션으로 운영되고 있습니다 다만 3만원 이상 구매하신 경우에만 적용되며 일부 매장은 대상에서 빠집니다</p>", Some("다만 3만원 이상 구매하신 경우에만 적용되며 일부 매장은 대상에서 빠집니다")),
            ("V3", "예시샵", "<p>대학생 인증 시 매월 5,000원 할인 쿠폰을 드립니다</p><p>※ 가입 후 3개월 동안만 제공됩니다.</p>", Some("※ 가입 후 3개월 동안만 제공됩니다.")),
            ("V4", "예시샵", "<section><h2>대학생</h2><p>대학생 10% 할인</p></section><section><h2>청소년</h2><p>청소년 매월 5,000원 할인 쿠폰을 드립니다</p></section>", None),
            ("V6", "예시샵", "<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>※ 아메리카노는 할인 대상에서 제외됩니다.</p>", Some("※ 아메리카노는 할인 대상에서 제외됩니다.")),
            ("V7", "예시샵", "<p>대학생 인증 시 매월 5,000원 할인 쿠폰을 드립니다</p><p>※ 유료 멤버십(월 4,900원) 가입 고객에 한합니다.</p>", Some("※ 유료 멤버십(월 4,900원) 가입 고객에 한합니다.")),
            ("V8", "예시샵", "<p>대학생 인증 시 매월 5,000원 할인 쿠폰을 드립니다</p><p>이벤트 기간: 2026.10.01 ~ 2026.10.31</p>", Some("이벤트 기간: 2026.10.01 ~ 2026.10.31")),
            ("A1", "예시샵", r#"<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>학생증은 실물만 인정됩니다.</p><p>※ 3만원 이상 구매 시에만 적용됩니다.</p>"#, Some("※ 3만원 이상 구매 시에만 적용됩니다.")),
            ("A2", "예시샵", r#"<table><tr><th>혜택</th><td>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</td></tr><tr><th>적용 매장</th><td>직영 매장 한정</td></tr></table>"#, Some("직영 매장 한정")),
            ("A3", "예시샵", r#"<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다. <sup>1)</sup></p><p>매장 정보는 아래를 참고해 주세요.</p>"#, None),
            ("A3b", "예시샵", r#"<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>매장 정보는 아래를 참고해 주세요.</p><p>각주: 아울렛 상품은 할인되지 않습니다.</p>"#, Some("아울렛 상품은 할인되지 않습니다.")),
            ("A4", "예시샵", r#"<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><ul><li>온라인 스토어 전용 혜택입니다.</li><li>1인 1회 사용할 수 있습니다.</li></ul>"#, Some("온라인 스토어 전용 혜택입니다. 1인 1회 사용할 수 있습니다.")),
            ("A5", "예시샵", r#"<section><h2>학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><section><h2>안내</h2><p>본 할인은 2026년 11월 30일까지 진행합니다.</p></section>"#, None),
            ("A6", "예시샵", r#"<p>※ 아래 혜택은 대학원생에게만 적용됩니다.</p><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p>"#, None),
            ("A7", "예시샵", r#"<h2>학생 할인 (온라인 전용)</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p>"#, Some("학생 할인 (온라인 전용)")),
            ("A8", "예시샵", r#"<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>참고로 아울렛 상품은 할인되지 않습니다.</p>"#, Some("참고로 아울렛 상품은 할인되지 않습니다.")),
            ("A9", "예시샵", r#"<p>Verified university students get 10% off all full-price items.</p><p>Not valid on sale items or gift cards.</p>"#, Some("Not valid on sale items or gift cards.")),
            ("A10", "예시샵", r#"<div class="mobile"><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></div><div class="pc"><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다. 단, 아울렛 매장은 빠집니다.</p></div>"#, Some("단, 아울렛 매장은 빠집니다.")),
            ("A11", "예시샵", r#"<div class="offer"><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></div><div class="note"><p>참고로 아울렛 상품은 할인되지 않습니다.</p></div>"#, Some("참고로 아울렛 상품은 할인되지 않습니다.")),
            ("A12", "예시샵", r#"<p>나이키 멤버 전용 혜택</p><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p>"#, Some("나이키 멤버 전용 혜택")),
        ];
        for (name, brand, body, condition) in cases {
            for (where_, html) in [("probe", page(body)), ("bare", bare(body))] {
                let result = live(brand, &html);
                println!("R6LIVE {where_} {name} => {result:?}");
                match (condition, &result) {
                    (None, Ok(shown)) => panic!("{name} ({where_}) must refuse, showed {shown:?}"),
                    (Some(cond), Ok(shown)) => assert!(
                        shown.contains(cond),
                        "{name} ({where_}) hid the condition: {shown:?}"
                    ),
                    _ => {}
                }
            }
        }
        // The control: a bare page with one offer publishes its fact
        // sentence and its condition (brand with digits included).
        let control = bare("<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>※ 3만원 이상 구매 시에만 적용됩니다.</p></section>");
        assert_eq!(
            live("11번가", &control).as_deref(),
            Ok("대학생은 10% 할인 혜택을 받을 수 있습니다. 자격 확인: 학생증. 원문: 대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다. 유의사항(원문): ※ 3만원 이상 구매 시에만 적용됩니다.")
        );
    }

    /// The 35 saved real pages through the live path: what each publishing
    /// page would show, word for word.
    #[test]
    fn real_pages_display() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/real_pages/");
        let index: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{dir}index.json")).expect("index"),
        )
        .expect("json");
        let audit = crate::corporate_auto::evidence::audit();
        let mut tally: std::collections::BTreeMap<String, usize> = Default::default();
        let mut published: Vec<(String, String)> = Vec::new();
        for e in index.as_array().expect("array") {
            let Some(file) = e["file"].as_str() else {
                *tally.entry("no_file".into()).or_default() += 1;
                continue;
            };
            let Ok(html) = std::fs::read_to_string(format!("{dir}{file}")) else {
                *tally.entry("unreadable".into()).or_default() += 1;
                continue;
            };
            let brand = audit
                .items
                .iter()
                .find(|i| i.id.to_string() == e["program_id"].as_str().unwrap_or(""))
                .map(|i| i.provider.clone())
                .unwrap_or_default();
            let result = live(&brand, &html);
            let key = match &result {
                Ok(_) => "publish".to_string(),
                Err(r) => r.split(':').take(2).collect::<Vec<_>>().join(":"),
            };
            println!(
                "R5REAL {:?} brand={brand:?} => {result:?}",
                e["title"].as_str().unwrap_or("")
            );
            *tally.entry(key).or_default() += 1;
            if let Ok(text) = result {
                published.push((e["title"].as_str().unwrap_or("").to_string(), text));
            }
        }
        println!("R5REAL_TALLY {tally:?}");
        // §28: what each publishing saved page shows, word for word: the
        // Korean summary of the facts, the facts' own sentences, then every
        // condition bound to the offer.
        // Any change is a change in what users read and is pinned here.
        let expected: Vec<(String, String)> = vec![
            ("Axure RP 학생 1년 무료".into(), "학생은 1년 동안 무료로 이용할 수 있습니다. 원문: Free Subscriptions for Students and Teachers Students : Click the button below to request a one-year Axure RP subscription each year you are enrolled in school.".into()),
            ("Bootstrap Studio 학생 무료 라이선스".into(), "학생은 재학 기간 동안 무료로 이용할 수 있습니다. 자격 확인: GitHub Student Developer Pack. 원문: If you are a student we have a special non-commercial license for you, which is free for the duration of your study.".into()),
            ("GitHub Copilot 학생 무료 이용".into(), "학생은 무료로 이용할 수 있습니다. 원문: Access GitHub Copilot for free as a student".into()),
            ("Azure for Students 무료 크레딧".into(), "학생은 무료로 이용할 수 있습니다. 자격 확인: 학교 이메일. 원문: Build in the cloud with Azure for Students for free 유의사항(원문): Try Azure for Students or Azure for Students Starter—no credit card required. Available only to full-time university students* Available only to new Azure customers No, you can't use your credit for Azure Marketplace offers.".into()),
            ("Polypane 학생 1년 무료".into(), "학생은 1년 동안 무료로 이용할 수 있습니다. 자격 확인: GitHub Student Developer Pack. 원문: Free for a year for students".into()),
            // Review L2: Sketch states two terms (one year; for as long as
            // the use is part of the studies) and is refused, never shown with one.
            ("Termius 학생 Pro·Team 무료".into(), "학생은 무료로 이용할 수 있습니다. 자격 확인: GitHub Student Developer Pack. 원문: Termius is free for students".into()),
            ("Tower Pro 학생 12개월 무료".into(), "학생은 1년 동안 무료로 이용할 수 있습니다. 자격 확인: GitHub Student Developer Pack. 원문: Tower for Students Get free access to Tower \"Pro\" as a student. You will receive 12 months of Tower for free.".into()),
        ];
        assert_eq!(published, expected);
    }

    /// §28: the shown text is the Korean template of the facts, the facts'
    /// own sentences and every condition of the offer, word for word; each
    /// part is checked against the live page.
    #[test]
    fn display_invariant_checks_content_not_layout() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        let pad =
            "매장 운영 시간과 위치는 지점마다 다를 수 있으니 방문 전에 확인해 주세요. ".repeat(12);
        let html = &format!("<main><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>※ 3만원 이상 구매 시에만 적용됩니다.</p><p>{pad}</p></main>");
        let html = html.as_str();
        let offer = crate::corporate_auto::bootstrap::extract_offer(html, today).unwrap();
        let text = visible_text(html);
        let facts = summary_quotes(&offer);
        let shown = compose_display(html, &offer).unwrap();
        assert_eq!(
            shown,
            "대학생은 10% 할인 혜택을 받을 수 있습니다. 자격 확인: 학생증. 원문: 대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다. 유의사항(원문): ※ 3만원 이상 구매 시에만 적용됩니다."
        );
        assert_eq!(
            display_invariant(&shown, &offer, html, &text, &facts),
            Ok(())
        );
        let check = |summary: &str| display_invariant(summary, &offer, html, &text, &facts);
        // A condition left out.
        assert_eq!(
            check(shown.split(" 유의사항(원문):").next().unwrap()),
            Err("condition_missing")
        );
        // A Korean part that is not the template of the facts.
        assert_eq!(
            check(&shown.replace("10% 할인 혜택을", "20% 할인 혜택을")),
            Err("not_the_template")
        );
        // A window cut out of a sentence.
        assert_eq!(
            check(&shown.replace(
                "전 메뉴 10% 할인 혜택을 받을 수 있습니다. 유의",
                "전 메뉴 10% 할인 유의"
            )),
            Err("not_a_sentence")
        );
        assert_eq!(
            check("대학생은 10% 할인 혜택을 받을 수 있습니다."),
            Err("no_original_text")
        );
        // A long page is no reason to refuse.
        let long = format!(
            "<main><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><p>{}</p></main>",
            "매장 위치는 지점마다 다릅니다. ".repeat(200)
        );
        let offer = crate::corporate_auto::bootstrap::extract_offer(&long, today).unwrap();
        let shown = compose_display(&long, &offer).unwrap();
        assert_eq!(
            display_invariant(
                &shown,
                &offer,
                &long,
                &visible_text(&long),
                &summary_quotes(&offer)
            ),
            Ok(())
        );
        // A restriction that cannot be shown word for word.
        let hidden = &format!("<main><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p><div style=\"display:none\"><p>아울렛 상품은 할인되지 않습니다.</p></div><p>{pad}</p></main>");
        let offer = crate::corporate_auto::bootstrap::extract_offer(hidden, today).unwrap();
        assert_eq!(
            compose_display(hidden, &offer),
            Err("condition_not_carried")
        );
    }

    /// §28 binding: a restricting clause is a condition of the offer only
    /// when it is about the offer or the person it is for.
    #[test]
    fn conditions_are_bound_to_the_offer() {
        for kept in [
            "You’ll need a macOS compatible device – our Mac app requires macOS Sequoia (15.0.0) or newer",
            "Also, if enrolled in a course, it’s required that the course fees are at a minimum the regular cost of Sketch.",
            "There’s also a minimum age requirement for an Education Workspace – it’s 16.",
            "Not valid on sale items or gift cards.",
            "Limited to one new license per student.",
            "※ 3만원 이상 구매 시에만 적용됩니다.",
            "1인 1회 사용할 수 있습니다.",
            "직영 매장 한정",
            "아울렛 상품은 할인되지 않습니다.",
            "The student discount is for the online store only.",
        ] {
            assert!(offer_condition(kept), "{kept}");
        }
        for dropped in [
            "Unfortunately 32bit Windows is quickly losing relevance and we can't afford the resources to build and test the app there.",
            "The interface of our program is only available in English at the moment.",
            "But the freely provided hosting service - Bootstrap Studio Sites - has a 5 website limit.",
            "How does the 3 computer limit work?",
            "it only has to speed up your workflow by 2 minutes and 15 seconds on each of those days to save you more than it costs.",
            "Getting your free Workspace is only one step on your journey.",
            "I have an existing Mac-only license account purchased with an Education discount.",
            "Download Sketch, start a trial and set up your Workspace (no credit card or payment details required).",
            "매장 운영 시간은 지점마다 다를 수 있습니다.",
        ] {
            assert!(!offer_condition(dropped), "{dropped}");
        }
    }

    /// A FAQ question heading is not read into the answer under it: the
    /// period fact quotes the answer alone, and the shown text asks nothing.
    #[test]
    fn a_faq_question_heading_is_not_part_of_the_offer() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        let pad =
            "매장 운영 시간과 위치는 지점마다 다를 수 있으니 방문 전에 확인해 주세요. ".repeat(12);
        let html = format!("<main><h2>Acme for Students</h2><p>Get free access to Acme Pro as a student.</p><h3>How long can I use Acme?</h3><p>You will receive 12 months of Acme for free as a student.</p><p>{pad}</p></main>");
        let offer = crate::corporate_auto::bootstrap::extract_offer(&html, today).unwrap();
        let period = offer
            .facts
            .iter()
            .find(|f| f.kind == "free_months")
            .expect("the stated period is read");
        assert_eq!(period.value, serde_json::json!(12));
        assert!(
            offer.facts.iter().all(|f| !is_question(&f.quote)),
            "{:?}",
            offer.facts
        );
        let shown = compose_display(&html, &offer).unwrap();
        assert!(!shown.contains("How long"), "{shown}");
        assert!(
            shown.contains("You will receive 12 months of Acme for free as a student."),
            "{shown}"
        );
    }

    /// A won amount is templated only as the discount its own quote states;
    /// a price or another amount has no template and is refused.
    #[test]
    fn a_won_amount_is_templated_only_as_a_stated_discount() {
        assert_eq!(
            won_discount("대학생 전 메뉴 10,000원 할인", 10_000).as_deref(),
            Some("10,000")
        );
        assert_eq!(
            won_discount("학생 3000 원 할인 쿠폰", 3_000).as_deref(),
            Some("3,000")
        );
        assert_eq!(won_discount("학생 요금 월 9,900원", 9_900), None);
        assert_eq!(won_discount("10,000원 할인", 5_000), None);
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        let pad =
            "매장 운영 시간과 위치는 지점마다 다를 수 있으니 방문 전에 확인해 주세요. ".repeat(12);
        let offer = |body: &str| {
            crate::corporate_auto::bootstrap::extract_offer(
                &format!("<main><p>{body}</p><p>{pad}</p></main>"),
                today,
            )
            .unwrap()
        };
        assert_eq!(
            korean_value(&offer(
                "대학생은 학생증 제시 시 전 메뉴 10,000원 할인 혜택을 받을 수 있습니다."
            )),
            Ok((
                "10,000원 할인".to_string(),
                "10,000원 할인 혜택을 받을 수 있습니다.".to_string()
            ))
        );
        assert_eq!(
            korean_value(&offer("대학생 전용 요금제는 월 9,900원입니다.")),
            Err("template_value_missing")
        );
    }

    /// A shown text that limits the offer to another or a narrower
    /// audience than its listing contradicts itself and is refused.
    #[test]
    fn a_narrower_audience_in_the_text_refuses() {
        let listed = ["university_student", "student"];
        let narrowed = |text: &str| audience_narrowed(text, &listed, false);
        assert!(narrowed(
            "※ 아래 혜택은 대학원생에게만 적용됩니다. 대학생은 10% 할인됩니다."
        ));
        assert!(narrowed("신입생 한정 10% 할인됩니다."));
        assert!(narrowed("대학생 10% 할인 (만 24세 이하)"));
        assert!(narrowed("대학생 10% 할인 (19~24세)"));
        assert!(narrowed("청소년 전용 쿠폰을 드립니다."));
        assert!(narrowed("Graduate students only."));
        assert!(!narrowed("대학생 전용 10% 할인됩니다."));
        assert!(!narrowed("재학생에 한해 10% 할인됩니다."));
        assert!(!narrowed("대학생은 학생증 제시 시 10% 할인됩니다."));
        assert!(!audience_narrowed(
            "대학생 10% 할인 (만 24세 이하)",
            &listed,
            true
        ));
        // A listing for both 대학생 and 대학원생 under `대학원생에게만`.
        let both = ["university_student", "graduate", "student"];
        assert!(audience_narrowed(
            "※ 아래 혜택은 대학원생에게만 적용됩니다.",
            &both,
            false
        ));
    }

    /// §28 composition: the facts' own sentences, then every restricting
    /// clause of the section word for word; a footnote mark needs its
    /// footnote, a hidden or attribute restriction refuses.
    #[test]
    fn display_is_composed_from_facts_and_carries_every_condition() {
        let offer = |html: &str| {
            crate::corporate_auto::bootstrap::extract_offer(
                html,
                chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(),
            )
            .unwrap()
        };
        let pad =
            "매장 운영 시간과 위치는 지점마다 다를 수 있으니 방문 전에 확인해 주세요. ".repeat(12);
        let page = |body: &str| {
            format!("<html><body><nav><a href=\"/\">홈 회원 전용</a></nav><main>{body}<p>{pad}</p></main><footer>이용약관</footer></body></html>")
        };
        let html = page("<section><h2>대학생 할인</h2><p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p></section><aside><p>※ 아울렛 상품은 할인되지 않습니다.</p></aside>");
        assert_eq!(
            compose_display(&html, &offer(&html)).as_deref(),
            Ok("대학생은 10% 할인 혜택을 받을 수 있습니다. 자격 확인: 학생증. 원문: 대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다. 유의사항(원문): ※ 아울렛 상품은 할인되지 않습니다.")
        );
        // Site chrome (`회원 전용` in the top nav) is not the offer's section.
        let plain =
            page("<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p>");
        assert_eq!(
            compose_display(&plain, &offer(&plain)).as_deref(),
            Ok("대학생은 10% 할인 혜택을 받을 수 있습니다. 자격 확인: 학생증. 원문: 대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.")
        );
        let footnote =
            page("<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.*</p>");
        assert_eq!(
            compose_display(&footnote, &offer(&footnote)),
            Err("footnote_unresolved")
        );
        let noted = page("<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.*</p><p>* 온라인 스토어에서 사용할 수 있습니다.</p>");
        assert_eq!(
            compose_display(&noted, &offer(&noted)).as_deref(),
            Ok("대학생은 10% 할인 혜택을 받을 수 있습니다. 자격 확인: 학생증. 원문: 대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.* 유의사항(원문): * 온라인 스토어에서 사용할 수 있습니다.")
        );
        for extra in [
            "<div style=\"display:none\"><p>아울렛 상품은 할인되지 않습니다.</p></div>",
            "<p><img src=\"/n.jpg\" alt=\"유의사항: 아울렛 상품 제외\"></p>",
            "<p>안내 <span title=\"아울렛 제외\">ⓘ</span></p>",
        ] {
            let html = page(&format!(
                "<p>대학생은 학생증 제시 시 전 메뉴 10% 할인 혜택을 받을 수 있습니다.</p>{extra}"
            ));
            assert_eq!(
                compose_display(&html, &offer(&html)),
                Err("condition_not_carried"),
                "{extra}"
            );
        }
        assert_eq!(
            sentences("대학생은 10% 할인됩니다. 단, 아울렛은 제외! 끝"),
            vec!["대학생은 10% 할인됩니다.", "단, 아울렛은 제외!", "끝"]
        );
    }

    #[test]
    fn brand_digits_pass_other_digits_refuse() {
        let ok = |title: &str| {
            let brand = ["학생 혜택", "청년 혜택", "혜택"]
                .iter()
                .find_map(|w| title.strip_suffix(w))
                .map(str::trim);
            brand.is_some_and(|b| !VALUE_CLAIM.is_match(b) && !b.contains(['%', '원']))
        };
        assert!(ok("1Password 학생 혜택"));
        assert!(ok("11번가 학생 혜택"));
        assert!(!ok("예시샵 10% 학생 혜택"));
        assert!(!ok("예시샵 5,000원 학생 혜택"));
        assert!(!ok("예시샵 무료"));
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;

    /// The public column list is read from the API's own constant, so a
    /// column added there fails here until it is classified.
    #[test]
    fn every_public_column_of_programs_is_classified() {
        let source = include_str!("../../../api/src/routes/programs.rs");
        let start = source
            .find("pub(crate) const PROGRAM_COLUMNS: &str =")
            .expect("PROGRAM_COLUMNS in the API");
        let rest = &source[start..];
        let end = rest.find(';').unwrap();
        let api: BTreeSet<String> = rest[..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .collect::<String>()
            .replace("\\", "")
            .split(',')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();
        let classified: BTreeSet<String> = PROGRAM_COLUMN_CLASSES
            .iter()
            .map(|(c, _)| c.to_string())
            .collect();
        assert_eq!(api, classified);
    }
}

#[cfg(test)]
mod detail_table_tests {
    use super::*;
    use std::sync::LazyLock;

    static TABLE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?i)\b(?:FROM|JOIN)\s+([a-z_][a-z0-9_]*)").unwrap());

    /// The body of `fn name` in `source` (up to the next top-level `}`).
    fn body<'a>(source: &'a str, signature: &str) -> &'a str {
        let start = source
            .find(signature)
            .unwrap_or_else(|| panic!("{signature} not found"));
        let rest = &source[start..];
        let end = rest.find("\n}\n").expect("function end");
        &rest[..end]
    }

    fn tables(text: &str) -> BTreeSet<String> {
        TABLE
            .captures_iter(text)
            .map(|c| c[1].to_ascii_lowercase())
            .collect()
    }

    /// Every table the public detail endpoint reads is classified in
    /// [`DETAIL_TABLE_CLASSES`]: the route, the detail assembly, the loaders
    /// it calls and the `current_programs` view. A new join, a new loader or
    /// a redefined view fails here until it is classified.
    #[test]
    fn every_table_the_detail_page_reads_is_classified() {
        let routes = include_str!("../../../api/src/routes/programs.rs");
        let detail = body(routes, "pub(crate) async fn load_program_detail(");
        // The loaders the detail calls are exactly the ones read below.
        let loaders: BTreeSet<String> = regex::Regex::new(r"crate::services::([a-z_:]+)\(")
            .unwrap()
            .captures_iter(detail)
            .map(|c| c[1].to_string())
            .collect();
        assert_eq!(
            loaders,
            [
                "application_policy::with_resources",
                "application_procedure::derive",
                "application_procedure::guest_stages",
                "application_resources::load",
                "evidence::load",
                "required_documents::load"
            ]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>(),
            "a new loader on the detail page must be read here"
        );
        let mut read = tables(body(routes, "pub async fn get_program("));
        read.extend(tables(detail));
        let resources =
            include_str!("../../../api/src/services/application_resources/repository.rs");
        read.extend(tables(body(resources, "pub async fn list_on(")));
        let documents = include_str!("../../../api/src/services/required_documents.rs");
        read.extend(tables(body(documents, "pub async fn load(")));
        let evidence = include_str!("../../../api/src/services/evidence.rs");
        read.extend(tables(body(evidence, "pub(crate) async fn load(")));
        // Pure today (no query); read so a future query is classified too.
        let procedure = include_str!("../../../api/src/services/application_procedure.rs");
        read.extend(tables(body(procedure, "pub fn derive(")));
        read.extend(tables(body(procedure, "pub fn guest_stages(")));
        // The view the route reads, as last defined by a migration: the newest
        // file under infra/migrations that defines current_programs, found
        // here at test time so redefining the view never needs this test
        // edited. Its last definition in that file is the one in force.
        let (_, view) = std::fs::read_dir(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../infra/migrations"
        ))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "sql"))
        .filter_map(|e| {
            let sql = std::fs::read_to_string(e.path()).ok()?;
            let at = sql
                .rfind("CREATE OR REPLACE VIEW current_programs")
                .or_else(|| sql.rfind("CREATE VIEW current_programs"))?;
            let view = &sql[at..];
            let view = view[..view.find(';').unwrap_or(view.len())].to_owned();
            Some((e.file_name().to_string_lossy().to_string(), view))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .expect("a migration defines current_programs");
        assert!(view.contains("FROM programs"), "{view}");
        read.extend(tables(&view));
        let classified: BTreeSet<String> = DETAIL_TABLE_CLASSES
            .iter()
            .map(|(t, _)| t.to_string())
            .collect();
        let missing: Vec<&String> = read.difference(&classified).collect();
        assert!(
            missing.is_empty(),
            "unclassified detail tables: {missing:?}"
        );
    }

    #[test]
    fn every_side_table_has_a_row_predicate() {
        for (table, class) in DETAIL_TABLE_CLASSES {
            if *class == "side" {
                assert!(side_predicate(table).is_some(), "{table}");
            }
        }
    }
}

// The reviewer's Part D publish probes, kept verbatim; `tests::partd_gate`
// in resilience asserts them.
#[cfg(test)]
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "publish_partd_probes.rs"]
mod probe_pub;

#[cfg(test)]
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "publish_r2_probes.rs"]
mod probe_r2_pub;

#[cfg(test)]
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "publish_r3_probes.rs"]
mod probe_r3_pub;

#[cfg(test)]
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "publish_r4_probes.rs"]
mod probe_r4_pub;

#[cfg(test)]
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "publish_r5_probes.rs"]
mod probe_r5_pub;

#[cfg(test)]
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "publish_r6_probes.rs"]
mod probe_r6;

#[cfg(test)]
#[rustfmt::skip]
#[allow(unused_imports, dead_code, clippy::all)]
#[path = "publish_r7_probes.rs"]
mod probe_r7;
