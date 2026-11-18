//! 정부24 공공서비스 혜택 API client
//!
//! API: https://api.odcloud.kr/api/gov24/v3/serviceList,
//! https://api.odcloud.kr/api/gov24/v3/supportConditions and
//! https://api.odcloud.kr/api/gov24/v3/serviceDetail (same key and dataset,
//! 행정안전부 공공서비스 혜택정보, 이용허락범위 제한 없음).
//! Env: GOV_BENEFITS_API_KEY
//!
//! Response is JSON with flat `data` array.
//! Pagination: page (1-based), perPage
//!
//! Each `serviceList` row is stored with its `supportConditions` row, verbatim,
//! under [`SUPPORT_CONDITIONS_KEY`]. Both endpoints must be read completely:
//! a run that could not read the conditions fails and is retried, instead of
//! storing rows without them and flapping every content hash.
//!
//! The `serviceDetail` fields the link resolver and the detail page need
//! (온라인신청사이트URL, 접수기관명, 구비서류, 신청방법) are stored under
//! [`DETAIL_KEY`]. They are side data:
//! the key is left out of the change hash (`sources::volatile_keys`), so a
//! failed or partial detail read never flaps a record, and a record without
//! it keeps its stored link candidates.

use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use tracing::{info, warn};

use super::{
    content_hash, effective_page_size, ensure_complete, http, DataSource, RawRecord, SourceFetch,
};

const BASE_URL: &str = "https://api.odcloud.kr/api/gov24/v3/serviceList";
const CONDITIONS_URL: &str = "https://api.odcloud.kr/api/gov24/v3/supportConditions";
const DETAIL_URL: &str = "https://api.odcloud.kr/api/gov24/v3/serviceDetail";
const PAGE_SIZE: u32 = 100;
/// The detail rows are long; 1,000 a page reads the catalogue in about 11 calls.
const DETAIL_PAGE_SIZE: u32 = 1000;
/// The whole serviceDetail read (about 11 calls) stops after this long.
const DETAIL_READ_LIMIT: std::time::Duration = std::time::Duration::from_secs(180);
/// Pause between pages of the second endpoint, so one sync never bursts.
const CONDITIONS_PAGE_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

/// Payload key holding the service's verbatim `supportConditions` row.
pub const SUPPORT_CONDITIONS_KEY: &str = "supportConditions";
/// Payload key holding [`DETAIL_FIELDS`] of the service's `serviceDetail` row.
pub const DETAIL_KEY: &str = "detail";
/// The `serviceDetail` fields kept: the online application site, the
/// receiving office, the documents to submit, and the application procedure
/// text (`신청방법`; the list's own `신청방법` is only channel codes).
pub const DETAIL_FIELDS: &[&str] = &["온라인신청사이트URL", "접수기관명", "구비서류", "신청방법"];
/// A kept detail text longer than this is cut (characters).
const DETAIL_FIELD_MAX_CHARS: usize = 4000;

pub struct GovBenefitsSource {
    client: Client,
    api_key: String,
}

impl GovBenefitsSource {
    pub fn new(api_key: String) -> Self {
        Self {
            client: super::http::key_in_query_client(None),
            api_key,
        }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("GOV_BENEFITS_API_KEY").context("GOV_BENEFITS_API_KEY not set")?;
        Ok(Self::new(key))
    }

    /// Every `supportConditions` row, keyed by 서비스ID. Fails unless the
    /// provider's whole list was read.
    async fn fetch_conditions(&self) -> Result<HashMap<String, Value>> {
        let mut rows = HashMap::new();
        let mut page = 1u32;
        loop {
            let resp = self.fetch_page_from(CONDITIONS_URL, page).await?;
            let fetched = resp.data.len();
            for row in resp.data {
                if let Some(id) = row.get("서비스ID").and_then(Value::as_str) {
                    let id = id.to_string();
                    rows.insert(id, row);
                }
            }
            let total = resp.matchCount.or(resp.totalCount);
            let page_size = effective_page_size(resp.perPage, fetched, PAGE_SIZE);
            if fetched == 0
                || (fetched as u64) < page_size
                || total.is_some_and(|t| u64::from(page) * page_size >= t)
            {
                anyhow::ensure!(
                    total.is_none_or(|t| rows.len() as u64 >= t),
                    "gov_benefits: supportConditions incomplete ({} of {:?})",
                    rows.len(),
                    total
                );
                return Ok(rows);
            }
            anyhow::ensure!(page < 1000, "gov_benefits: conditions pagination limit");
            page += 1;
            tokio::time::sleep(CONDITIONS_PAGE_DELAY).await;
        }
    }

    /// [`DETAIL_FIELDS`] of every `serviceDetail` row, keyed by 서비스ID.
    /// Best effort: a failed page ends the read with what was read so far
    /// (records without a detail keep their stored link candidates).
    async fn fetch_details(&self) -> HashMap<String, Value> {
        let mut rows = HashMap::new();
        let mut page = 1u32;
        loop {
            let resp = match self.fetch_sized(DETAIL_URL, page, DETAIL_PAGE_SIZE).await {
                Ok(resp) => resp,
                Err(error) => {
                    warn!(source = "gov_benefits", page, error = %error, "serviceDetail read stopped; records without a detail keep their links");
                    return rows;
                }
            };
            let fetched = resp.data.len();
            for row in resp.data {
                if let Some(id) = row.get("서비스ID").and_then(Value::as_str) {
                    rows.insert(id.to_string(), detail_projection(&row));
                }
            }
            let total = resp.matchCount.or(resp.totalCount);
            let page_size = effective_page_size(resp.perPage, fetched, DETAIL_PAGE_SIZE);
            if fetched == 0
                || (fetched as u64) < page_size
                || total.is_some_and(|t| u64::from(page) * page_size >= t)
                || page >= 100
            {
                return rows;
            }
            page += 1;
            tokio::time::sleep(CONDITIONS_PAGE_DELAY).await;
        }
    }

    async fn fetch_page(&self, page: u32) -> Result<GovApiResponse> {
        self.fetch_page_from(BASE_URL, page).await
    }

    async fn fetch_page_from(&self, url: &str, page: u32) -> Result<GovApiResponse> {
        self.fetch_sized(url, page, PAGE_SIZE).await
    }

    async fn fetch_sized(&self, url: &str, page: u32, per_page: u32) -> Result<GovApiResponse> {
        let response = http::send(
            self.client.get(url).query(&[
                ("serviceKey", self.api_key.as_str()),
                ("page", &page.to_string()),
                ("perPage", &per_page.to_string()),
            ]),
            "gov_benefits",
        )
        .await?;
        let text = http::text(response, "gov_benefits").await?;

        serde_json::from_str::<GovApiResponse>(&text)
            .with_context(|| format!("gov_benefits: JSON schema changed at {url} page {page}"))
    }
}

/// Attach the service's conditions row, verbatim, to its list row.
pub(crate) fn attach_conditions(item: &mut Value, conditions: &HashMap<String, Value>) {
    let id = item
        .get("서비스ID")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let (Some(id), Some(object)) = (id, item.as_object_mut()) {
        if let Some(row) = conditions.get(&id) {
            object.insert(SUPPORT_CONDITIONS_KEY.to_string(), row.clone());
        }
    }
}

/// The kept [`DETAIL_FIELDS`] of one `serviceDetail` row (string values only).
pub(crate) fn detail_projection(row: &Value) -> Value {
    let mut kept = serde_json::Map::new();
    for field in DETAIL_FIELDS {
        if let Some(text) = row.get(*field).and_then(Value::as_str) {
            kept.insert(
                (*field).to_string(),
                Value::String(text.chars().take(DETAIL_FIELD_MAX_CHARS).collect()),
            );
        }
    }
    Value::Object(kept)
}

/// The trimmed documents text (`구비서류`) of a stored record's
/// [`DETAIL_KEY`] row. `None` (no detail row: the serviceDetail read failed
/// or stopped early; or a row without the text) means "not read": the
/// stored documents are kept. A one-run upstream gap therefore never
/// blanks a text or reaches users as a change; a text the source really
/// removed stays until it is replaced.
pub fn detail_documents(payload: &Value) -> Option<String> {
    payload
        .get(DETAIL_KEY)
        .and_then(|row| crate::pipeline::source_text(row, "구비서류"))
}

/// Attach the service's detail row after the record's hash and id are taken.
pub(crate) fn attach_detail(item: &mut Value, details: &HashMap<String, Value>) {
    let id = item
        .get("서비스ID")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let (Some(id), Some(object)) = (id, item.as_object_mut()) {
        if let Some(row) = details.get(&id) {
            object.insert(DETAIL_KEY.to_string(), row.clone());
        }
    }
}

impl DataSource for GovBenefitsSource {
    fn name(&self) -> &'static str {
        "gov_benefits"
    }

    async fn fetch_all(&self) -> Result<SourceFetch> {
        let conditions = self.fetch_conditions().await?;
        info!(
            source = self.name(),
            conditions = conditions.len(),
            "support conditions fetched"
        );
        // Bounded on its own: the detail read must never push the daily
        // government job past its time limit.
        let details = match tokio::time::timeout(DETAIL_READ_LIMIT, self.fetch_details()).await {
            Ok(details) => details,
            Err(_) => {
                warn!(source = self.name(), "serviceDetail read exceeded its time limit; records without a detail keep their links");
                HashMap::new()
            }
        };
        info!(
            source = self.name(),
            details = details.len(),
            "service details fetched"
        );
        let mut records = Vec::new();
        let mut page = 1u32;
        let mut reported_total: Option<u64> = None;

        loop {
            info!(source = self.name(), page, "fetching page");
            let resp = self.fetch_page(page).await?;

            let items = resp.data;

            if items.is_empty() {
                warn!(
                    source = self.name(),
                    page, "empty item list — stopping pagination"
                );
                break;
            }

            let fetched = items.len();
            for mut item in items {
                attach_conditions(&mut item, &conditions);
                let hash = content_hash(&item);
                // Content-derived, never position-derived: see youth_center.
                let source_id = item
                    .get("서비스ID")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("gov_unknown_{}", &hash[..16]));
                // After the id and hash: neither may depend on the detail.
                attach_detail(&mut item, &details);

                records.push(RawRecord {
                    source_id,
                    payload: item,
                    content_hash: hash,
                });
            }

            reported_total = resp.matchCount.or(resp.totalCount);
            let page_size = effective_page_size(resp.perPage, fetched, PAGE_SIZE);
            let last_page = (fetched as u64) < page_size
                || reported_total.is_some_and(|total| u64::from(page) * page_size >= total);
            if last_page {
                break;
            }
            anyhow::ensure!(page < 1000, "gov_benefits: pagination limit exceeded");
            page += 1;
        }

        let complete = ensure_complete(self.name(), records.len(), reported_total);
        info!(
            source = self.name(),
            total = records.len(),
            complete,
            "fetch complete"
        );
        Ok(SourceFetch::unfiltered(records, complete))
    }
}

// ── Response shapes ───────────────────────────────────────────────────────────
//
// {
//   "currentCount": 100,
//   "data": [{
//     "서비스ID": "000000465790",
//     "서비스명": "...",
//     "서비스목적요약": "...",
//     "서비스분야": "보육·교육",
//     "소관기관명": "교육부",
//     "지원대상": "...",
//     "지원내용": "...",
//     "선정기준": "...",
//     "신청방법": "...",
//     "신청기한": "상시신청",
//     "상세조회URL": "https://www.gov.kr/...",
//     "전화문의": "...",
//     "지원유형": "현금(감면)"
//   }],
//   "matchCount": 10921,
//   "page": 1,
//   "perPage": 100,
//   "totalCount": 10921
// }

// ── 신청기한 ──────────────────────────────────────────────────────────────────

/// An application window stated with explicit calendar days in `신청기한`.
/// `start` is `None` for a bare deadline ("2026. 2. 6.까지").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatedWindow {
    pub start: Option<NaiveDate>,
    pub end: NaiveDate,
}

/// Words that make the dates of a text something other than an intake
/// window: business or operating periods (사업기간, 운영기간) and insurance
/// or event-relative claim periods (사고, 보험, 보장, 청구, 발생, 이내) whose
/// dates are a coverage period, not a deadline.
///
/// Words that only say intake may continue beyond or between the dates
/// (상시, 수시, 연중, 매월, 추가, 미정, 연장) do not void a dated window: the
/// dates are still the intake the notice states, and a later round arrives
/// as a changed text (audit M3).
const NOT_AN_INTAKE: &[&str] = &[
    "사업기간",
    "운영기간",
    "사고",
    "보험",
    "보장",
    "청구",
    "발생",
    "이내",
];

/// Parses `신청기한` when it states one or more complete dated ranges
/// ("2026.03.03~2026.11.30", "2026. 3. 9.(월) 09:00 ~ 3. 23.(월) 18:00",
/// "'25.12.22.~'26.12.11.", "2026년 6월 17일 ~ 30일", "20260501~20260630"),
/// or a single dated deadline ending in 까지, into the envelope of its
/// rounds ([`parse_stated_rounds`]). The reviewed table of production texts
/// pins it.
#[cfg(test)]
pub(crate) fn parse_stated_window(text: &str) -> Option<StatedWindow> {
    let rounds = parse_stated_rounds(text)?;
    let first = rounds.first()?;
    Some(StatedWindow {
        start: first.start,
        end: rounds.iter().map(|r| r.end).max()?,
    })
}

/// The round of `rounds` (sorted, disjoint) a programme stands in on
/// `today`: the first that has not ended, else the last (closed).
pub(crate) fn current_round(rounds: &[StatedWindow], today: NaiveDate) -> StatedWindow {
    *rounds
        .iter()
        .find(|round| round.end >= today)
        .or(rounds.last())
        .expect("parse_stated_rounds never returns an empty list")
}

/// The dated rounds a text states, sorted, overlapping ones merged (a
/// sub-period such as 집중신청기간 is part of its round). Accepts one or more
/// complete dated ranges ("2026.03.03~2026.11.30", "2026. 3. 9.(월) 09:00 ~
/// 3. 23.(월) 18:00", "'25.12.22.~'26.12.11.", "2026년 6월 17일 ~ 30일",
/// "20260501~20260630") or one dated deadline ending in 까지. Anything else
/// is `None`: undated words alone ("상시"), month-only periods, an open end
/// ("~ 예산 소진 시"), a range without a year, an inverted range, a stray
/// number the grammar does not explain, or any [`NOT_AN_INTAKE`] word. Times
/// of day are ignored; callers apply whole Korean calendar days.
pub(crate) fn parse_stated_rounds(text: &str) -> Option<Vec<StatedWindow>> {
    read_rounds(text, &mut None)
}

/// A range the text states with both years written whose end is before its
/// start ("20260601 ~ 20250710"): a provider defect, which a source that
/// rejects such records ([`crate::pipeline`] for 온통청년) reads instead of
/// "no window".
pub(crate) fn stated_inverted_range(text: &str) -> Option<(NaiveDate, NaiveDate)> {
    let mut inverted = None;
    read_rounds(text, &mut inverted);
    inverted
}

fn read_rounds(
    text: &str,
    inverted: &mut Option<(NaiveDate, NaiveDate)>,
) -> Option<Vec<StatedWindow>> {
    static PATTERNS: std::sync::OnceLock<Patterns> = std::sync::OnceLock::new();
    let patterns = PATTERNS.get_or_init(Patterns::new);
    if NOT_AN_INTAKE.iter().any(|word| text.contains(word)) {
        return None;
    }
    let text: String = text
        .chars()
        .map(|c| match c {
            '∼' | '～' | '〜' => '~',
            '’' | '‘' | '`' | '´' => '\'',
            c => c,
        })
        .collect();
    let text = patterns.noise.replace_all(&text, " ");

    let mut dates = Vec::new();
    let mut gaps = Vec::new();
    let mut cursor = 0;
    for found in patterns.date.captures_iter(&text) {
        let whole = found.get(0)?;
        gaps.push(&text[cursor..whole.start()]);
        dates.push(DateToken::from_captures(&found)?);
        cursor = whole.end();
    }
    gaps.push(&text[cursor..]);
    if dates.is_empty() {
        return None;
    }
    let explained = |gap: &str| {
        !patterns
            .label
            .replace_all(gap, " ")
            .contains(|c: char| c.is_ascii_digit())
    };
    if !gaps.iter().all(|gap| explained(gap)) {
        return None;
    }
    let connects = |gap: &str| {
        let gap = gap.trim_matches(|c: char| c.is_whitespace() || c == '.');
        gap == "~"
    };
    let compact_pair = |i: usize| {
        dates.len() == 2
            && matches!(dates[0], DateToken::Compact(_))
            && matches!(dates[1], DateToken::Compact(_))
            && gaps[i].trim() == "-"
    };

    // A lone deadline: exactly one full date followed by 까지.
    if dates.len() == 1 {
        let DateToken::Full(end) = dates[0] else {
            return None;
        };
        let tail = gaps[1].trim_start_matches(|c: char| c.is_whitespace() || c == '.');
        let lone = !gaps[0].contains('~') && tail.starts_with("까지") && !tail.contains('~');
        return lone.then(|| vec![StatedWindow { start: None, end }]);
    }

    let mut context: Option<NaiveDate> = None;
    let mut window: Option<(NaiveDate, NaiveDate)> = None;
    let mut rounds: Vec<(NaiveDate, NaiveDate)> = Vec::new();
    let mut i = 0;
    while i < dates.len() {
        // Every date must open a range, and every range must be closed.
        if gaps[i].contains('~') || i + 1 >= dates.len() {
            return None;
        }
        if !(connects(gaps[i + 1]) || compact_pair(i + 1)) {
            return None;
        }
        let start = dates[i].resolve(context, false)?;
        let end = dates[i + 1].resolve(Some(start), true)?;
        if end < start {
            let dated = |t: &DateToken| matches!(t, DateToken::Full(_) | DateToken::Compact(_));
            if dated(&dates[i]) && dated(&dates[i + 1]) {
                *inverted = Some((start, end));
            }
            return None;
        }
        // A later round with no year of its own that lands before the previous
        // round is either a year rollover ("12. 1.~12. 20., 1. 5.~1. 20.") or a
        // sub-period of the window so far (집중신청기간). Only the second is
        // safe to read; a guessed year could close a round that is still open.
        if let (DateToken::MonthDay(..), Some(previous)) = (dates[i], context) {
            let nested = window.is_some_and(|(s, e)| s <= start && end <= e);
            if start < previous && !nested {
                return None;
            }
        }
        window = Some(match window {
            None => (start, end),
            Some((s, e)) => (s.min(start), e.max(end)),
        });
        rounds.push((start, end));
        context = Some(end);
        i += 2;
    }
    if gaps[dates.len()].contains('~') {
        return None;
    }
    rounds.sort();
    let mut merged: Vec<StatedWindow> = Vec::new();
    for (start, end) in rounds {
        match merged.last_mut() {
            Some(last) if last.start.is_some_and(|s| start >= s) && start <= last.end => {
                last.end = last.end.max(end);
            }
            _ => merged.push(StatedWindow {
                start: Some(start),
                end,
            }),
        }
    }
    (!merged.is_empty()).then_some(merged)
}

/// Whether `신청기한` says, as the whole text, that nobody can apply now:
/// "신규신청불가" (only existing recipients), "신청불필요" (nothing to apply
/// for) or "OOOO년 신규모집 계획 없음". Longer texts that merely contain these
/// words ("예산 소진 시 신청 불가") are left to the date parser.
pub(crate) fn declares_not_open(text: &str) -> bool {
    let compact: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '○' | '-' | '·' | '.'))
        .collect();
    let compact = compact.trim_start_matches(|c: char| c.is_ascii_digit() || c == '년');
    matches!(
        compact,
        "신규신청불가" | "신청불필요" | "신규모집계획없음" | "신규신청불가능"
    )
}

struct Patterns {
    /// Weekday markers and times of day, removed before dates are read.
    noise: regex::Regex,
    /// One calendar day, most specific form first.
    date: regex::Regex,
    /// Numbers that label a round or term rather than a date.
    label: regex::Regex,
}

impl Patterns {
    fn new() -> Self {
        let compile = |p: &str| regex::Regex::new(p).expect("static pattern compiles");
        Self {
            noise: compile(
                r"\(\s*[월화수목금토일]\s*\)|\d{1,2}\s*:\s*\d{2}|\d{1,2}\s*시(?:\s*\d{1,2}\s*분)?",
            ),
            date: compile(concat!(
                r"(?P<ky>\d{4})\s*년\s*(?P<km>\d{1,2})\s*월\s*(?P<kd>\d{1,2})\s*일",
                r"|(?P<pkm>\d{1,2})\s*월\s*(?P<pkd>\d{1,2})\s*일",
                r"|(?P<dd>\d{1,2})\s*일",
                r"|(?P<cy>\d{4})(?P<cm>\d{2})(?P<cd>\d{2})",
                r"|(?P<y>\d{4}|'\d{2})\s*[./-]\s*(?P<m>\d{1,2})\s*[./-]\s*(?P<d>\d{1,2})\.?",
                r"|(?P<pm>\d+)\s*[./-]\s*(?P<pd>\d{1,2})\.?",
            )),
            label: compile(r"\d+\s*(?:차|단계|학기|분기|회)|(?:'?\d{2}|\d{4})\s*년도?"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum DateToken {
    /// Year, month and day are all stated.
    Full(NaiveDate),
    /// YYYYMMDD; only a pair joined by "-" or "~" is a range.
    Compact(NaiveDate),
    /// Month and day; the year comes from the previous date.
    MonthDay(u32, u32),
    /// Day only ("~ 30일"); only valid as the end of a range.
    Day(u32),
}

impl DateToken {
    fn from_captures(c: &regex::Captures<'_>) -> Option<Self> {
        let num = |name: &str| c.name(name).and_then(|m| m.as_str().parse::<u32>().ok());
        let year = |name: &str| {
            let raw = c.name(name)?.as_str();
            match raw.strip_prefix('\'') {
                Some(short) => short.parse::<i32>().ok().map(|y| 2000 + y),
                None => raw.parse::<i32>().ok(),
            }
        };
        let full = |y: Option<i32>, m: Option<u32>, d: Option<u32>| {
            NaiveDate::from_ymd_opt(y?, m?, d?).filter(|date| (2000..=2100).contains(&date.year()))
        };
        if c.name("ky").is_some() {
            return full(year("ky"), num("km"), num("kd")).map(Self::Full);
        }
        if c.name("cy").is_some() {
            return full(year("cy"), num("cm"), num("cd")).map(Self::Compact);
        }
        if c.name("y").is_some() {
            return full(year("y"), num("m"), num("d")).map(Self::Full);
        }
        let (month, day) = match (num("pkm"), num("pkd"), num("pm"), num("pd")) {
            (Some(m), Some(d), ..) | (.., Some(m), Some(d)) => (m, d),
            _ => return num("dd").filter(|d| (1..=31).contains(d)).map(Self::Day),
        };
        ((1..=12).contains(&month) && (1..=31).contains(&day)).then_some(Self::MonthDay(month, day))
    }

    /// The calendar day, taking a missing year (and month) from `context`.
    fn resolve(self, context: Option<NaiveDate>, is_end: bool) -> Option<NaiveDate> {
        match self {
            Self::Full(date) | Self::Compact(date) => Some(date),
            Self::MonthDay(month, day) => NaiveDate::from_ymd_opt(context?.year(), month, day),
            Self::Day(day) if is_end => {
                let context = context?;
                NaiveDate::from_ymd_opt(context.year(), context.month(), day)
            }
            Self::Day(_) => None,
        }
    }
}

#[allow(non_snake_case)]
#[derive(Debug, Deserialize)]
struct GovApiResponse {
    data: Vec<Value>,
    /// Rows in the whole dataset; `matchCount` is the paginated subset.
    totalCount: Option<u64>,
    matchCount: Option<u64>,
    #[allow(dead_code)]
    currentCount: Option<u64>,
    #[allow(dead_code)]
    page: Option<u64>,
    /// Rows per page the provider actually served; may be below the request.
    perPage: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The detail row is side data: the change hash of a record is the same
    /// with or without it (and with a changed one), and only the kept
    /// fields are stored.
    #[test]
    fn a_detail_row_never_changes_the_record_hash_or_id() {
        let list =
            serde_json::json!({"서비스ID": "305000000174", "서비스명": "청년 월세", "조회수": 3});
        let details: HashMap<String, Value> = [(
            "305000000174".to_string(),
            detail_projection(&serde_json::json!({
                "서비스ID": "305000000174", "온라인신청사이트URL": "https://www.bokjiro.go.kr/apply",
                "접수기관명": "주민센터", "구비서류": "신분증", "지원내용": "긴 본문", "수정일시": "20261001",
                "신청방법": "1. 소속 장기요양기관으로 신청서류 제출\r\n2. 구청 신청서류 검토 및 접수"
            })),
        )]
        .into();
        let mut with_detail = list.clone();
        attach_detail(&mut with_detail, &details);
        assert_eq!(
            with_detail[DETAIL_KEY],
            serde_json::json!({"온라인신청사이트URL": "https://www.bokjiro.go.kr/apply", "접수기관명": "주민센터", "구비서류": "신분증",
                "신청방법": "1. 소속 장기요양기관으로 신청서류 제출\r\n2. 구청 신청서류 검토 및 접수"})
        );
        let hash = |v: &Value| crate::sources::record_hash("gov_benefits", v);
        assert_eq!(hash(&with_detail), hash(&list));
        let mut changed = with_detail.clone();
        changed[DETAIL_KEY]["온라인신청사이트URL"] = "https://other.go.kr".into();
        assert_eq!(hash(&changed), hash(&list));
        let mut unknown = list.clone();
        attach_detail(&mut unknown, &HashMap::new());
        assert!(unknown.get(DETAIL_KEY).is_none());
    }

    fn date(text: &str) -> Option<NaiveDate> {
        (text != "-").then(|| text.parse().expect("fixture date is YYYY-MM-DD"))
    }

    /// Every row is a distinct 신청기한 value from production, with the
    /// reviewed outcome. A change here is a policy change, not a refactor.
    #[test]
    fn real_application_period_texts_parse_to_their_reviewed_windows() {
        let table = include_str!("../../tests/fixtures/gov_application_periods.tsv");
        let mut rows = 0;
        let mut dated = 0;
        let mut wrong = Vec::new();
        for line in table.lines().filter(|l| !l.starts_with('#')) {
            let mut columns = line.splitn(3, '\t');
            let (start, end, text) = (
                columns.next().unwrap(),
                columns.next().unwrap(),
                columns.next().expect("three tab-separated columns"),
            );
            let expected = date(end).map(|end| StatedWindow {
                start: date(start),
                end,
            });
            let actual = parse_stated_window(text);
            if actual != expected {
                wrong.push(format!("{text:?}: {actual:?}, reviewed {expected:?}"));
            }
            rows += 1;
            dated += usize::from(expected.is_some());
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        assert!(rows >= 100 && dated >= 50, "{rows} rows, {dated} dated");
    }

    /// Distinct production 신청기한 values (2026-10-12).
    #[test]
    fn only_whole_not_open_statements_close_a_row() {
        for text in [
            "신규신청불가",
            "신청불필요",
            "신청 불필요 ",
            "2026년 신규모집 계획 없음",
        ] {
            assert!(declares_not_open(text), "{text}");
        }
        for text in [
            "예산 소진 시 신청 불가",
            "반기별 1회 지급(신청불필요)",
            "상시신청, 선착순 마감",
            "별도의 신청절차가 없음",
            "상시신청",
            "",
        ] {
            assert!(!declares_not_open(text), "{text}");
        }
    }

    #[test]
    fn conditions_attach_by_service_id_only() {
        let conditions = HashMap::from([(
            "000000465790".to_string(),
            serde_json::json!({"서비스ID": "000000465790", "JA0110": 3}),
        )]);
        let mut item = serde_json::json!({"서비스ID": "000000465790", "서비스명": "유아학비"});
        attach_conditions(&mut item, &conditions);
        assert_eq!(item[SUPPORT_CONDITIONS_KEY]["JA0110"], 3);
        let mut other = serde_json::json!({"서비스ID": "999"});
        attach_conditions(&mut other, &conditions);
        assert!(other.get(SUPPORT_CONDITIONS_KEY).is_none());
    }

    /// Audit M3/M4: a dated window followed by words that say intake may
    /// continue ("추가 모집 시 별도 공고") is still that window, and several
    /// rounds are kept so the current one is stored.
    #[test]
    fn rounds_are_kept_and_the_current_round_is_chosen() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        let rounds = parse_stated_rounds("2026. 3. 3. ~ 3. 20. / 2026. 5. 1. ~ 5. 22.").unwrap();
        assert_eq!(rounds.len(), 2);
        let on = |day| current_round(&rounds, day);
        assert_eq!(
            on(d(2026, 3, 10)),
            StatedWindow {
                start: Some(d(2026, 3, 3)),
                end: d(2026, 3, 20)
            }
        );
        assert_eq!(
            on(d(2026, 4, 10)),
            StatedWindow {
                start: Some(d(2026, 5, 1)),
                end: d(2026, 5, 22)
            }
        );
        assert_eq!(
            on(d(2026, 6, 1)),
            StatedWindow {
                start: Some(d(2026, 5, 1)),
                end: d(2026, 5, 22)
            }
        );
        // A sub-period inside a round is the same round.
        assert_eq!(
            parse_stated_rounds("2026.2.9.~11.30.까지(집중신청기간 : 2.9.~3.31.)")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            parse_stated_window("2026. 1. 5. ~ 1. 30. (추가 모집 시 별도 공고)"),
            Some(StatedWindow {
                start: Some(d(2026, 1, 5)),
                end: d(2026, 1, 30)
            })
        );
        assert_eq!(
            parse_stated_window("상반기 : 2026-01-02~2026-02-13 / 하반기 : 미정"),
            Some(StatedWindow {
                start: Some(d(2026, 1, 2)),
                end: d(2026, 2, 13)
            })
        );
        // Compact rounds on separate lines (온통청년 aplyYmd).
        let youth = parse_stated_rounds("20260301 ~ 20260331\\N20260901 ~ 20260930").unwrap();
        assert_eq!(youth.len(), 2);
        assert_eq!(current_round(&youth, d(2026, 4, 1)).end, d(2026, 9, 30));
    }

    #[test]
    fn ranges_collapse_to_their_envelope_and_never_invert() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert_eq!(
            parse_stated_window("2026. 3. 3. ~ 3. 20. / 2026. 5. 1. ~ 5. 22."),
            Some(StatedWindow {
                start: Some(d(2026, 3, 3)),
                end: d(2026, 5, 22)
            })
        );
        // A year-less end that falls before its start is not guessed into
        // the next year.
        assert_eq!(parse_stated_window("2025. 12. 20. ~ 1. 21."), None);
        // A year-less later round before the previous one is a rollover the
        // text does not date: never guessed.
        assert_eq!(
            parse_stated_window("2025. 12. 1. ~ 12. 20., 1. 5. ~ 1. 20."),
            None
        );
        // A sub-period inside the window so far is fine.
        assert_eq!(
            parse_stated_window("2026.2.9.~11.30.까지(집중신청기간 : 2.9.~3.31.)"),
            Some(StatedWindow {
                start: Some(d(2026, 2, 9)),
                end: d(2026, 11, 30)
            })
        );
        // An announced extension does not void the dated window (audit M3).
        assert_eq!(
            parse_stated_window("2026. 3. 3. ~ 3. 20. (신청기간 연장 예정)").map(|w| w.end),
            Some(d(2026, 3, 20))
        );
        for undated in [
            "사업기간 : 2026. 1. 1. ~ 2026. 12. 31.",
            "운영기간 2026.03.02~2026.12.31",
        ] {
            assert_eq!(parse_stated_window(undated), None, "{undated}");
        }
        // A range must start with a year.
        assert_eq!(parse_stated_window("3.3. ~ 4.1."), None);
        // A day-only token cannot open a range.
        assert_eq!(
            parse_stated_window("2026. 6. 1. ~ 6. 19. 20일 ~ 30일"),
            None
        );
        // A single date is a deadline only when it says 까지.
        assert_eq!(parse_stated_window("2026.12.01."), None);
        assert_eq!(
            parse_stated_window("2026년 6월 30일 까지").map(|w| w.end),
            Some(d(2026, 6, 30))
        );
        assert_eq!(parse_stated_window("~ 2026.12.01. 까지"), None);
        assert_eq!(parse_stated_window(""), None);
    }
}
