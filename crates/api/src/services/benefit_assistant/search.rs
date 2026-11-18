//! Cross-programme question ("부산 사는 휴학생이 받을 수 있는 장학금?"):
//! a rule-parsed [`SearchIntent`] narrows the SAME recommendation preview
//! the home list uses, run on the saved profile with the question's region
//! and enrollment status in place of the saved ones. The profile is not
//! changed; financial products are excluded as in bundles. No model.
use super::super::{
    application_preparation::Error,
    benefit_comparison::FINANCIAL_PRODUCT_SQL,
    grounded_qa::intent::{self, SearchIntent},
    recommendations,
};
use crate::assistant_provider::is_out_of_scope;
use mazimi_core::models::{FilterSummary, RecommendScope, RecommendationItem, UserProfile};
use mazimi_core::youth_fit;
use serde::Serialize;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

pub const MAX_RESULTS: usize = 5;

/// How a row meets the fields the question names, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TopicMatch {
    /// The row's field is one asked for and its title names it.
    Both,
    /// Another field, but the title names the topic or the question's word
    /// ("어학 응시료" under 취업 for "어학 교육비"): the user's own word in
    /// the title beats a field match alone.
    Title,
    /// The row's field is one asked for.
    Field,
    /// Only the summary mentions it ("월세 실비" in a scholarship): listed
    /// after every on-topic row and never counted as one.
    Mentions,
}

impl TopicMatch {
    pub fn on_topic(self) -> bool {
        self != Self::Mentions
    }
}

#[derive(Serialize)]
pub struct SearchResult {
    #[serde(flatten)]
    pub item: RecommendationItem,
    /// Question keywords the title or summary contains.
    pub matched_keywords: Vec<String>,
    /// The youth_fit card label of a demoted row the question asked for
    /// ("아동·청소년 대상일 수 있음"); such rows come after the others.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub youth_fit_label: Option<&'static str>,
    /// `None` when the question names no field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic_match: Option<TopicMatch>,
    /// Days from today (Korea) to the deadline; `None` without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_days: Option<i64>,
    /// The question named a region and this row is that region's own (not
    /// nationwide): ranked before nationwide rows.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
    /// The row's own regions when they leave out the region the search ran
    /// with (a reviewed regional exception lists it): listed after the
    /// region's rows and labelled, so it never reads as that region's (QA
    /// B11).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outside_region: Vec<&'static str>,
    /// The question bounds the amount and the row stores none for its
    /// period: listed after the rows that meet the bound, never as meeting
    /// it (audit M6).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub amount_unregistered: bool,
    /// The question bounds the amount and the row stores none for its
    /// period (an amount for another period, or none): listed after.
    #[serde(skip)]
    pub amount_unread: bool,
}

/// A title that says the programme is closed: a closed word ending the
/// title ("2026 청년 인턴 모집 마감", "- 신청 마감") or the whole of a
/// bracket ("(접수마감)", "[마감]"). A closed word with more after it
/// ("신청 마감 연장", "마감일 안내", "신청 마감 D-3", "(신청 마감 2026.11.30)")
/// or a condition before it ("예산소진 시 조기마감") is a date or a rule, not
/// a closure; "사업종료" names who a programme is for ("폐업(사업종료)
/// 소상공인 …") (re-review 4 M2).
pub(crate) fn closed_title(title: &str) -> bool {
    const ENDING: &[&str] = &[
        "신청마감",
        "모집마감",
        "접수마감",
        "마감완료",
        "마감됨",
        "모집종료",
        "접수종료",
        "신청종료",
    ];
    // A bracket holding just these says closed ("(조기 종료)": re-review 7
    // L1).
    const BRACKETED: &[&str] = &["마감", "종료", "조기마감", "조기종료"];
    // Ends with a closed word (or one with a completion ending: "접수마감되었
    // 습니다"), with no date or condition right before it.
    let closes = |text: &str| {
        let text = ["되었습니다", "됐습니다", "되었음", "되었어요", "됐어요"]
            .iter()
            .find_map(|e| text.strip_suffix(e))
            .unwrap_or(text);
        ENDING
            .iter()
            .any(|w| text.strip_suffix(w).is_some_and(|before| !opens(before)))
    };
    let compact: String = title.chars().filter(|c| !c.is_whitespace()).collect();
    let bracketed = [('(', ')'), ('[', ']'), ('【', '】'), ('<', '>')]
        .iter()
        .any(|(open, close)| {
            compact.split(*open).skip(1).any(|rest| {
                rest.split_once(*close)
                    .is_some_and(|(inside, _)| BRACKETED.contains(&inside) || closes(inside))
            })
        });
    // 사업종료 ending the title, bare or as the trailing bracket ("청년 월세
    // 지원 사업종료", "(사업종료)"); mid-title it names who a programme is for
    // ("폐업(사업종료) 소상공인 …") (re-review 6 L1).
    // A trailing bracket may carry a year or 됨 ("(2025년 사업종료)",
    // "(사업종료됨)") (re-review 7 L1).
    let trimmed = compact.trim_end_matches(['.', '!']);
    let trailing_bracket = trimmed
        .strip_suffix([')', ']'])
        .and_then(|t| t.rfind(['(', '[']).map(|at| &t[at + 1..]));
    let ended = trimmed.ends_with("사업종료")
        || trimmed.ends_with("사업종료됨")
        || trailing_bracket.is_some_and(|inside| {
            ["사업종료", "사업종료됨"].contains(&inside) || year_ended(inside)
        });
    closes(compact.trim_end_matches(['.', '!'])) || bracketed || ended
}

/// What right before a closed word makes it a date or a rule, not a
/// closure: a date ("11월 30일", "11.30.", "12/31", "D-3"), a time word
/// ("오늘", "금일", "내일", "모레", "당일", "곧", "이번 주", "다음 주",
/// "매주 금요일", "매월 말일", "월말"), or a
/// condition ("예산소진 시", "선착순", "소진될 경우", "소진 후", "까지")
/// (re-review 5 M3, re-review 6 L1). "조기 신청마감" is closed early, not a
/// condition; "예산소진 시 조기마감" is read by its 시.
/// "2025년사업종료", "2025사업종료": a year, then 사업종료.
fn year_ended(inside: &str) -> bool {
    inside.strip_suffix("사업종료").is_some_and(|year| {
        let digits = year.trim_end_matches('년');
        digits.len() == 4 && digits.chars().all(|c| c.is_ascii_digit())
    })
}

fn opens(before: &str) -> bool {
    let b = before.trim_end_matches(['.', '/', '-', '~']);
    let mut tail = b.chars().rev();
    let last = tail.next();
    let date = last.is_some_and(|c| c.is_ascii_digit())
        || (matches!(last, Some('일' | '월')) && tail.next().is_some_and(|c| c.is_ascii_digit()))
        || before.ends_with("D-");
    date || [
        "시",
        "오늘",
        "금일",
        "내일",
        "모레",
        "당일",
        "곧",
        "이번주",
        "다음주",
        "매주",
        "매월",
        "월말",
        "말일",
        "요일",
        "선착순",
        "경우",
        "후",
        "까지",
    ]
    .iter()
    .any(|w| b.ends_with(w))
}

/// A field with rows when the asked one has none.
#[derive(Debug, Clone, Serialize)]
pub struct NearestField {
    pub field: &'static str,
    pub label: &'static str,
    pub count: usize,
}

/// What the topic filter did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TopicSummary {
    pub fields: Vec<&'static str>,
    pub labels: Vec<&'static str>,
    /// Rows of the asked fields (or whose title names them).
    pub on_topic: usize,
    /// Rows whose summary alone mentions the topic.
    pub mentions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nearest: Option<NearestField>,
}

#[derive(Serialize)]
pub struct Search {
    pub version: u8,
    pub intent: SearchIntent,
    /// Out of scope (state changes, hypotheticals, calculations): no search.
    pub unsupported: bool,
    /// Nothing narrowed the search; results are the saved profile's top list.
    pub intent_empty: bool,
    pub required_profile_fields: Vec<&'static str>,
    pub filter_summary: Option<FilterSummary>,
    /// Listed programmes that also match the question's type and keywords.
    pub total_candidates: usize,
    pub results: Vec<SearchResult>,
    pub not_found: bool,
    pub notice: String,
    /// `None` when the question names no field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<TopicSummary>,
    /// The region the search ran with (the question's, else the profile's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region_label: Option<&'static str>,
    /// The saved profile's region, when the question named another one: the
    /// answer says so instead of asserting the asked one (re-review 4 H2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_region_label: Option<&'static str>,
    /// The question named a region the user has not saved: rows carry no
    /// residence reason and the answer says the region as its basis.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub region_basis: bool,
    /// The question's own content words (ones some catalogue title has)
    /// that no listed row matches, title or summary: the answer says so
    /// before the field's rows (PR #20 review H3: "전세사기" listed every housing row as if
    /// it had been found).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched_words: Vec<String>,
}

/// Provider tabs a requested programme type stands for. Policy and
/// scholarship requests read the stored provider tabs, so a housing-type
/// policy is a 정책 and a scholarship filed as a policy is a 장학금.
fn provider_of(program_type: &str) -> Option<&'static str> {
    match program_type {
        "policy" => Some("policy"),
        "scholarship" => Some("scholarship"),
        _ => None,
    }
}

/// Whether `item` is one of the requested programme types.
pub(crate) fn provider_matches(types: &[&str], item: &RecommendationItem) -> bool {
    types.is_empty()
        || types.contains(&item.program_type.as_str())
        || types.iter().any(|t| {
            provider_of(t).is_some_and(|p| item.categories.provider_categories.iter().any(|c| c == p))
        })
        // "할인" asks for both offer types: a corporate scholarship counts.
        || (types.contains(&"corporate_benefit")
            && types.contains(&"campus_benefit")
            && item.categories.provider_categories.iter().any(|c| c == "corporate"))
}

/// How `item` meets `fields` (see [`TopicMatch`]); `None` = off topic.
pub(crate) fn topic_match(
    fields: &[&str],
    soft: &SearchIntent,
    item: &RecommendationItem,
    summary: Option<&str>,
) -> Option<TopicMatch> {
    use mazimi_core::categories::field_cues_in;
    let field = item
        .categories
        .field_category
        .as_deref()
        .is_some_and(|f| fields.contains(&f));
    let title = fields
        .iter()
        .any(|f| !field_cues_in(f, &item.title).is_empty())
        || !intent::matched_keywords(soft, &item.title, None).is_empty();
    let mentions = summary.is_some_and(|s| {
        fields.iter().any(|f| !field_cues_in(f, s).is_empty())
            || !intent::matched_keywords(soft, "", Some(s)).is_empty()
    });
    match (field, title) {
        (true, true) => Some(TopicMatch::Both),
        (true, false) => Some(TopicMatch::Field),
        (false, true) => Some(TopicMatch::Title),
        (false, false) if mentions => Some(TopicMatch::Mentions),
        _ => None,
    }
}

/// The keywords a named field already covers ("월세" under 주거): they also
/// make a title on topic.
fn covered_keywords(intent: &SearchIntent) -> SearchIntent {
    let mut covered = SearchIntent::default();
    for (keyword, forms) in intent.keywords.iter().zip(&intent.keyword_forms) {
        if !intent.fields.is_empty()
            && mazimi_core::categories::fields_named(keyword)
                .iter()
                .any(|f| intent.fields.contains(f))
        {
            covered.keywords.push(keyword.clone());
            covered.keyword_forms.push(forms.clone());
        }
    }
    covered
}

fn notice(intent: &SearchIntent) -> String {
    let mut applied: Vec<&str> = Vec::new();
    applied.extend(intent.region_label);
    applied.extend(intent.enrollment_label);
    let fields: Vec<&str> = intent
        .fields
        .iter()
        .filter_map(|f| mazimi_core::categories::field_label(f))
        .collect();
    let fields = fields.join("·");
    if !fields.is_empty() {
        applied.push(&fields);
    }
    let keywords = intent.keywords.join("·");
    if !keywords.is_empty() {
        applied.push(&keywords);
    }
    let amount = intent
        .amount_min
        .map(|min| intent::amount_label(i64::from(min), intent.amount_period))
        .unwrap_or_default();
    if !amount.is_empty() {
        applied.push(&amount);
    }
    let lead = if applied.is_empty() {
        "질문에서 좁힐 조건을 찾지 못해 저장한 프로필로 찾았어요.".to_owned()
    } else {
        format!(
            "질문에 적은 조건({})으로 찾았어요. 저장한 프로필은 바꾸지 않았어요.",
            applied.join(" · ")
        )
    };
    // An exclusion the question asked for is not applied: said, never a
    // silent filter (audit M1).
    let negated = if intent.negated.is_empty() {
        String::new()
    } else {
        format!(" ‘{}’ 빼기는 적용하지 않았어요.", intent.negated.join("·"))
    };
    // The not-final note belongs to the answer that lists rows, once.
    format!("{lead}{negated} 예금·대출 같은 금융상품은 제외해요.")
}

pub async fn search(pool: &PgPool, user: Uuid, question: &str) -> Result<Search, Error> {
    if is_out_of_scope(question) {
        let mut response = empty(intent::parse(question));
        response.intent = SearchIntent::default();
        response.unsupported = true;
        response.notice = "이 질문은 혜택 찾기로 답할 수 없는 요청이에요. 프로필·알림·신청 상태는 바꾸지 않았어요.".into();
        return Ok(response);
    }
    search_with_intent(pool, user, intent::parse(question)).await
}

fn empty(intent: SearchIntent) -> Search {
    Search {
        version: 1,
        intent_empty: intent.is_empty(),
        notice: notice(&intent),
        intent,
        unsupported: false,
        required_profile_fields: Vec::new(),
        filter_summary: None,
        total_candidates: 0,
        results: Vec::new(),
        not_found: false,
        topic: None,
        region_label: None,
        saved_region_label: None,
        region_basis: false,
        unmatched_words: Vec::new(),
    }
}

/// A row's display meta, as the candidate query reads it.
type RowMeta = (Option<String>, bool, Option<String>, Option<String>);

/// The candidate list one profile reading gives: the recommendation preview
/// and the rows' summaries and youth_fit facts. A relaxed re-search in the
/// same run changes only the filters after it (window, keywords, type), so
/// it reuses this instead of scoring the catalogue again (code review M6).
#[derive(Clone)]
pub struct Candidates {
    key: (String, Option<String>, String),
    items: Vec<RecommendationItem>,
    filter_summary: FilterSummary,
    meta: HashMap<Uuid, RowMeta>,
    /// Each row's own region list (empty = nationwide).
    regions: HashMap<Uuid, Vec<String>>,
    gate: youth_fit::GateProfile,
    region: &'static str,
    /// The saved profile's region (canonical code), if any.
    saved: Option<&'static str>,
    /// Each row's stored one-off amount (`benefit_amount_once`, support
    /// money only), which the list's items do not carry.
    once_amounts: HashMap<Uuid, i32>,
}

/// One run's [`Candidates`].
pub type SearchCache = Option<Candidates>;

/// The search behind [`search`] for an intent that is already structured
/// (the agent's typed `search_benefits` arguments).
pub async fn search_with_intent(
    pool: &PgPool,
    user: Uuid,
    intent: SearchIntent,
) -> Result<Search, Error> {
    search_with_cache(pool, user, intent, &mut None).await
}

/// [`search_with_intent`] reusing `cache` when the profile reading (region,
/// enrollment status, requested youth_fit targets) is the cached one.
pub async fn search_with_cache(
    pool: &PgPool,
    user: Uuid,
    intent: SearchIntent,
    cache: &mut SearchCache,
) -> Result<Search, Error> {
    let mut response = empty(intent);
    // Relevance reranker (ASSISTANT_SEARCH_RERANK_ENABLED): the query vector
    // off the async runtime; filter_candidates then reranks from cached
    // programme vectors.
    let rerank = match crate::services::relevance::global() {
        Some(relevance) => {
            let words = rerank_words(&response.intent);
            let r = relevance.clone();
            let w = words.clone();
            tokio::task::spawn_blocking(move || r.query_vector(&w))
                .await
                .ok()
                .flatten()
                .map(|query| (relevance, query, words))
        }
        None => None,
    };
    let key = (
        response.intent.region.unwrap_or("").to_owned(),
        response.intent.enrollment_status.map(str::to_owned),
        format!(
            "{:?}",
            youth_fit::requested_targets(&response.intent.keywords.join(" "))
        ),
    );
    if let Some(cached) = cache.as_ref().filter(|c| c.key == key) {
        let cached = cached.clone();
        return Ok(filter_candidates(response, cached, rerank.as_ref()));
    }
    let mut tx = pool.begin().await.map_err(crate::errors::internal_error)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    let profile = sqlx::query_as::<_, UserProfile>("SELECT * FROM user_profiles WHERE user_id=$1")
        .bind(user)
        .fetch_optional(&mut *tx)
        .await
        .map_err(crate::errors::internal_error)?;
    let birth_year = profile.as_ref().and_then(|p| p.birth_year);
    let region = response.intent.region.or_else(|| {
        profile
            .as_ref()
            .and_then(|p| p.region_code.as_deref())
            .and_then(mazimi_core::canonical_region_code)
    });
    if birth_year.is_none() {
        response.required_profile_fields.push("birth_year");
    }
    if region.is_none() {
        response.required_profile_fields.push("region_code");
    }
    let (Some(mut profile), Some(region)) = (profile, region) else {
        tx.commit().await.map_err(crate::errors::internal_error)?;
        return Ok(response);
    };
    if birth_year.is_none() {
        tx.commit().await.map_err(crate::errors::internal_error)?;
        return Ok(response);
    }
    // A saved 시·군·구 belongs to the saved sido; a question about another
    // sido drops it rather than sending a district outside the region.
    let saved = profile
        .region_code
        .as_deref()
        .and_then(mazimi_core::canonical_region_code);
    if saved != Some(region) {
        profile.city_code = None;
    }
    let saved_enrollment = profile
        .enrollment_status
        .as_deref()
        .and_then(mazimi_core::canonical_enrollment_status);
    profile.region_code = Some(region.to_owned());
    if let Some(status) = response.intent.enrollment_status {
        profile.enrollment_status = Some(status.to_owned());
    }
    // Profile fields the question names and the user has not saved: the
    // decision reads them as the search's basis, never as met (audit H3,
    // arch review M3).
    let mut assumed: Vec<&str> = Vec::new();
    if response.intent.region.is_some() && saved != Some(region) {
        assumed.push("region_code");
    }
    if response
        .intent
        .enrollment_status
        .is_some_and(|asked| saved_enrollment != Some(asked))
    {
        assumed.push("enrollment_status");
    }
    let input = super::bundles::profile_input(profile)
        .ok_or_else(|| crate::errors::internal_error("Validated profile missing fields"))?;
    // Demoted youth_fit rows (farms, firms, children, …) are left out unless
    // the question clearly asks for their target ("축산 농가", "아이 교육비").
    let requested = youth_fit::requested_targets(&response.intent.keywords.join(" "));
    let gate = recommendations::youth_fit_gate_for(&input);
    let recommendation =
        recommendations::preview_with(&mut tx, input, RecommendScope::All, &requested, &assumed)
            .await?;
    let ids: Vec<Uuid> = recommendation.items.iter().map(|i| i.program_id).collect();
    type Meta = (Uuid, Option<String>, bool, Option<String>, Option<String>);
    type MetaValue = RowMeta;
    let meta: HashMap<Uuid, MetaValue> = sqlx::query_as::<_, Meta>(&format!(
        "SELECT programs.id, summary, {FINANCIAL_PRODUCT_SQL},
                    COALESCE(yf.reviewed_verdict, yf.verdict),
                    CASE WHEN yf.reviewed_verdict IS NULL THEN yf.target ELSE yf.reviewed_target END
             FROM programs LEFT JOIN program_youth_fit yf ON yf.program_id = programs.id
             WHERE programs.id = ANY($1)"
    ))
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?
    .into_iter()
    .map(|(id, summary, financial, fit, target)| (id, (summary, financial, fit, target)))
    .collect();
    let mut once_amounts: HashMap<Uuid, i32> = HashMap::new();
    let regions: HashMap<Uuid, Vec<String>> = sqlx::query_as::<
        _,
        (Uuid, Option<Vec<String>>, Option<i32>, String),
    >(
        "SELECT id, regions, benefit_amount_once, program_type FROM programs WHERE id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(crate::errors::internal_error)?
    .into_iter()
    .map(|(id, regions, once, program_type)| {
        // A corporate offer's amounts are no support money.
        if let Some(once) = once.filter(|_| mazimi_core::pays_support(&program_type)) {
            once_amounts.insert(id, once);
        }
        (id, regions.unwrap_or_default())
    })
    .collect();
    tx.commit().await.map_err(crate::errors::internal_error)?;
    let candidates = Candidates {
        key,
        items: recommendation.items,
        filter_summary: recommendation.filter_summary,
        meta,
        regions,
        gate,
        region,
        saved,
        once_amounts,
    };
    *cache = Some(candidates.clone());
    Ok(filter_candidates(response, candidates, rerank.as_ref()))
}

/// The topic words a search is reranked against: its keywords and the
/// labels of the fields it names.
fn rerank_words(intent: &SearchIntent) -> Vec<String> {
    intent
        .keywords
        .iter()
        .cloned()
        .chain(
            intent
                .fields
                .iter()
                .filter_map(|f| mazimi_core::categories::field_label(f))
                .map(str::to_owned),
        )
        .collect()
}

type Rerank = (
    std::sync::Arc<crate::services::relevance::Relevance>,
    Vec<f32>,
    Vec<String>,
);

/// The question's filters and ranking over a candidate list.
fn filter_candidates(
    mut response: Search,
    candidates: Candidates,
    rerank: Option<&Rerank>,
) -> Search {
    let Candidates {
        items,
        filter_summary,
        meta,
        regions,
        gate,
        region,
        saved,
        once_amounts,
        ..
    } = candidates;
    // A region the question names: its own programmes before nationwide ones
    // (no region, or every sido) (holdout round 4).
    let asked_region = response.intent.region;
    response.filter_summary = Some(filter_summary);
    response.region_label = Some(mazimi_core::region_label(region));
    response.saved_region_label = saved
        .filter(|s| *s != region)
        .map(mazimi_core::region_label);
    // The question names a region the user has not saved (another sido, or
    // none saved): the profile was read as living there only to search, so
    // no row says it meets "{region} 거주" (re-review 5 H2). The answer
    // says the region as the question's basis instead.
    let unsaved_region = asked_region.is_some() && saved != Some(region);
    response.region_basis = unsaved_region;
    let intent = &response.intent;
    let soft = covered_keywords(intent);
    // Free-text words rank; they never filter as an AND (QA B3: a typo or a
    // leftover ending emptied searches another word answered). A named field
    // is the topic: its rows stay, ranked by the words, and words no row
    // matches are reported (`unmatched_words`). With no field named (a
    // programme type, a deadline window or "the earliest" is no topic), the
    // words are the topic: a row is a candidate only when its title says
    // one of them (an OR; PR #20 review H2/H3). Meaning (the encoder, when
    // on) re-ranks and drops under its floor; it never admits a row no word
    // matches. Nothing left is an honest "찾지 못했어요".
    let words_only = intent.fields.is_empty() && !intent.keywords.is_empty();
    // The catalogue vocabulary the asked words are judged against: every
    // candidate's title (rows the filters drop included) and the field
    // labels. Titles name what a programme is for; summaries are prose whose
    // verbs and quantifiers ("덜어주", "하나") would pass as topics. A word that
    // occurs nowhere in it is noise (a typo, a verb or particle left in) and
    // is never quoted back; a topic word the catalogue knows but no listed
    // row has is reported (`unmatched_words`).
    let vocabulary: String = items
        .iter()
        .map(|item| item.title.clone())
        .chain(
            mazimi_core::categories::FIELDS
                .iter()
                .map(|(_, label)| (*label).to_owned()),
        )
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<String>()
        .to_lowercase();
    let asked_words: Vec<(String, Vec<String>)> = intent
        .keywords
        .iter()
        .cloned()
        .zip(intent.keyword_forms.iter().cloned())
        .collect();
    let fields = intent.fields.clone();
    let today = mazimi_core::seoul_date(chrono::Utc::now());
    let mut pool_fields: HashMap<&'static str, usize> = HashMap::new();
    let mut results: Vec<SearchResult> = Vec::new();
    for item in items {
        let (summary, financial, fit, target) = meta
            .get(&item.program_id)
            .map(|(s, f, v, t)| (s.as_deref(), *f, v.as_deref(), t.as_deref()))
            .unwrap_or((None, false, None, None));
        // Labelled (and listed last) only when the row is listed because
        // the question asked for its target; a row the user's own profile
        // keeps is a normal result.
        let verdict = fit.and_then(youth_fit::Verdict::parse);
        let requested_only = verdict.is_some_and(|v| {
            youth_fit::recommendation_exclusion(
                v,
                target.and_then(youth_fit::Target::parse),
                gate,
                &[],
            )
            .is_some()
        });
        if financial || !provider_matches(&intent.program_types, &item) {
            continue;
        }
        // An excluded region filters the rows' regions, not the profile: a
        // row local to excluded regions leaves, nationwide rows stay. It is
        // read after the cache, so a cached preview never skips it.
        let row_regions = regions.get(&item.program_id);
        if !intent.excluded_regions.is_empty()
            && row_regions.is_some_and(|list| {
                !list.is_empty()
                    && list.len() < 17
                    && list.iter().all(|r| {
                        mazimi_core::canonical_region_code(r)
                            .is_some_and(|code| intent.excluded_regions.contains(&code))
                    })
            })
        {
            continue;
        }
        // A row whose own title says it is closed is no candidate.
        if closed_title(&item.title) {
            continue;
        }
        let matched_keywords = intent::matched_keywords(intent, &item.title, summary);
        // Words alone name what the row is: its title says one of them (QA
        // B12: "카카오" listed a counselling row whose summary mentions
        // 카카오톡). A summary mention only ranks.
        if words_only && intent::matched_keywords(intent, &item.title, None).is_empty() {
            continue;
        }
        let deadline_days = item
            .deadline
            .as_deref()
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            .map(|d| (d - today).num_days());
        // A calendar period of Korean days (audit M2): "다음 달" is the 1st
        // to the last of next month.
        if let Some(to) = intent.deadline_within_days {
            let from = i64::from(intent.deadline_from_days.unwrap_or(0));
            if !deadline_days.is_some_and(|d| (from..=i64::from(to)).contains(&d)) {
                continue;
            }
        }
        if intent.earliest && deadline_days.is_none_or(|d| d < 0) {
            continue;
        }
        // A stated lower bound on the amount (audit M6): the stored amount
        // of the period it names (any stored period without one).
        let mut amount_unregistered = false;
        let mut amount_unread = false;
        if let Some(min) = intent.amount_min {
            let once = once_amounts.get(&item.program_id).copied();
            let any_amount = item.benefit_amount_monthly.is_some()
                || item.benefit_amount_semester.is_some()
                || once.is_some();
            let stored: Vec<i32> = match intent.amount_period {
                Some("monthly") => vec![item.benefit_amount_monthly],
                Some("semester") => vec![item.benefit_amount_semester],
                Some("once") => vec![once],
                _ => vec![
                    item.benefit_amount_monthly,
                    item.benefit_amount_semester,
                    once,
                ],
            }
            .into_iter()
            .flatten()
            .collect();
            // Meets: listed. Below: dropped. An amount stored only for
            // another period cannot be compared with the bound, and listed
            // it would read as meeting it: dropped (round-3 review L-9). No
            // amount at all: listed after, labelled.
            if stored.is_empty() && any_amount {
                continue;
            } else if stored.is_empty() {
                amount_unread = true;
                amount_unregistered = true;
            } else if !stored.iter().any(|amount| *amount >= min) {
                continue;
            }
        }
        if let Some(field) = item.categories.field_category.as_deref().and_then(|f| {
            mazimi_core::categories::FIELDS
                .iter()
                .find(|(c, _)| *c == f)
        }) {
            *pool_fields.entry(field.0).or_default() += 1;
        }
        let topic = if fields.is_empty() {
            None
        } else {
            match topic_match(&fields, &soft, &item, summary) {
                Some(topic) => Some(topic),
                None => continue,
            }
        };
        let item_id = item.program_id;
        results.push(SearchResult {
            item,
            matched_keywords,
            youth_fit_label: verdict
                .filter(|_| requested_only)
                .and_then(youth_fit::Verdict::label),
            topic_match: topic,
            deadline_days,
            local: asked_region.is_some_and(|r| {
                regions.get(&item_id).is_some_and(|list| {
                    !list.is_empty() && list.len() < 17 && list.iter().any(|x| x == r)
                })
            }),
            outside_region: regions
                .get(&item_id)
                .filter(|list| !list.is_empty() && !list.iter().any(|x| x == region))
                .map(|list| {
                    list.iter()
                        .filter_map(|x| mazimi_core::canonical_region_code(x))
                        .map(mazimi_core::region_label)
                        .collect()
                })
                .unwrap_or_default(),
            amount_unregistered,
            amount_unread,
        });
    }
    // On-topic rows first (field and title, title, field, then summary
    // mentions), then rows matching more question words, then labelled rows
    // after equally matching ones: an asked-for child row ("자녀 교복") is
    // not cut behind rows that match one word. A closing-soon search orders
    // by deadline within that. Stable: the recommendation order holds within
    // each group.
    let by_deadline = intent.deadline_within_days.is_some() || intent.earliest;
    results.sort_by_key(|r| {
        (
            r.amount_unread,
            r.topic_match,
            !r.outside_region.is_empty(),
            !r.local,
            std::cmp::Reverse(r.matched_keywords.len()),
            r.youth_fit_label.is_some(),
            if by_deadline { r.deadline_days } else { None },
        )
    });
    if !fields.is_empty() {
        let on_topic = results
            .iter()
            .filter(|r| r.topic_match.is_some_and(TopicMatch::on_topic))
            .count();
        let nearest = (on_topic == 0)
            .then(|| {
                let asked = fields[0];
                let count = |f: &str| pool_fields.get(f).copied().unwrap_or(0);
                mazimi_core::categories::nearest_fields(asked)
                    .iter()
                    .copied()
                    .find(|f| count(f) > 0)
                    .or_else(|| {
                        mazimi_core::categories::FIELDS
                            .iter()
                            .map(|(c, _)| *c)
                            .filter(|c| !fields.contains(c))
                            .max_by_key(|c| count(c))
                            .filter(|c| count(c) > 0)
                    })
                    .map(|f| NearestField {
                        field: f,
                        label: mazimi_core::categories::field_label(f).unwrap_or(f),
                        count: count(f),
                    })
            })
            .flatten();
        response.topic = Some(TopicSummary {
            labels: fields
                .iter()
                .filter_map(|f| mazimi_core::categories::field_label(f))
                .collect(),
            fields,
            on_topic,
            mentions: results.len() - on_topic,
            nearest,
        });
    }
    if let Some((relevance, query, words)) = rerank {
        // Tiers: the rules' topic groups, in their order.
        let mut tier = 0u8;
        let candidates: Vec<crate::services::relevance::Candidate<'_>> = results
            .iter()
            .enumerate()
            .map(|(i, r)| {
                if i > 0 && results[i - 1].topic_match != r.topic_match {
                    tier = tier.saturating_add(1);
                }
                crate::services::relevance::Candidate {
                    id: r.item.program_id,
                    title: &r.item.title,
                    summary: meta.get(&r.item.program_id).and_then(|m| m.0.as_deref()),
                    tier,
                }
            })
            .collect();
        // The floor only with keywords and no deadline window.
        let may_drop =
            !response.intent.keywords.is_empty() && response.intent.deadline_within_days.is_none();
        let order = relevance
            .rerank_with(query, words, &candidates, may_drop)
            .order;
        let mut slots: Vec<Option<SearchResult>> = results.into_iter().map(Some).collect();
        results = order.into_iter().filter_map(|i| slots[i].take()).collect();
    }
    // Words of the question's own no listed row matches (a field search).
    if !words_only {
        response.unmatched_words = asked_words
            .into_iter()
            .filter(|(word, forms)| {
                forms.iter().any(|f| vocabulary.contains(f.as_str()))
                    && !results.iter().any(|r| r.matched_keywords.contains(word))
            })
            .map(|(word, _)| word)
            .collect();
    }
    // The one that closes first, among the rows that answer the question:
    // on topic (never a summary-only mention) and in the searched region
    // (PR #20 review H2: a welfare row mentioning 주거 won by closing first).
    if response.intent.earliest {
        results.retain(|r| {
            r.topic_match.is_none_or(TopicMatch::on_topic) && r.outside_region.is_empty()
        });
        results.sort_by_key(|r| r.deadline_days);
        results.truncate(1);
    }
    response.total_candidates = results.len();
    results.truncate(MAX_RESULTS);
    response.not_found = results.is_empty();
    response.results = results;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use mazimi_core::models::{FilterSummary, RecommendationItem};

    fn item(reasons: &[&str]) -> RecommendationItem {
        RecommendationItem {
            eligibility_status: "unknown".into(),
            program_id: Uuid::new_v4(),
            title: "대구 청년 월세 지원".into(),
            title_display: String::new(),
            program_type: "policy".into(),
            summary_display: None,
            provider_name: None,
            company_name: None,
            match_score: 50,
            benefit_amount_monthly: None,
            benefit_amount_semester: None,
            deadline: None,
            reasons: reasons.iter().map(|r| (*r).to_owned()).collect(),
            missing_checks: vec![],
            official_url: None,
            application_method: None,
            submission_documents: None,
            screening_method: None,
            evidence: vec![],
            conditions_verified: false,
            verified: false,
            remaining_checks: Vec::new(),
            window: None,
            categories: Default::default(),
            conditions: Default::default(),
        }
    }

    fn titled(title: &str, field: Option<&str>) -> RecommendationItem {
        let mut row = item(&[]);
        row.title = title.into();
        row.categories.field_category = field.map(str::to_owned);
        row
    }

    fn search_rows(args: mazimi_core::agent::SearchArgs, rows: Vec<RecommendationItem>) -> Search {
        let candidates = Candidates {
            key: (String::new(), None, String::new()),
            items: rows,
            filter_summary: FilterSummary::default(),
            meta: HashMap::new(),
            regions: HashMap::new(),
            gate: youth_fit::GateProfile::default(),
            region: "busan",
            saved: Some("busan"),
            once_amounts: HashMap::new(),
        };
        filter_candidates(empty(intent::from_parts(&args)), candidates, None)
    }

    fn titles(search: &Search) -> Vec<&str> {
        search
            .results
            .iter()
            .map(|r| r.item.title.as_str())
            .collect()
    }

    /// QA B3: a word no row has (a leftover ending, "맞으") no longer
    /// empties a field search: words rank, they never AND-filter.
    #[test]
    fn keywords_rank_a_field_search_and_never_filter_it() {
        let args = mazimi_core::agent::SearchArgs {
            fields: vec!["housing".into()],
            keywords: vec!["월세".into(), "맞으".into()],
            ..Default::default()
        };
        let found = search_rows(
            args,
            vec![
                titled("청년 이사비 지원", Some("housing")),
                titled("청년 월세 지원", Some("housing")),
            ],
        );
        assert_eq!(titles(&found), ["청년 월세 지원", "청년 이사비 지원"]);
        assert!(!found.not_found);
    }

    /// QA B3, B12: with words alone, a row must match one of them (an OR,
    /// so a junk word does not empty the search), and nothing matching is an
    /// honest "not found", never the profile's unrelated rows.
    #[test]
    fn a_words_only_search_lists_matching_rows_or_nothing() {
        let rows = || {
            vec![
                titled("청소년 상담 지원", Some("welfare")),
                titled("청년 월세 지원", Some("housing")),
            ]
        };
        let either = mazimi_core::agent::SearchArgs {
            keywords: vec!["돈이".into(), "월세".into()],
            ..Default::default()
        };
        assert_eq!(titles(&search_rows(either, rows())), ["청년 월세 지원"]);
        let none = mazimi_core::agent::SearchArgs {
            keywords: vec!["카카오".into()],
            ..Default::default()
        };
        let found = search_rows(none, rows());
        assert!(found.results.is_empty() && found.not_found);
        // A summary mention is no match for words alone, and a programme
        // type ("지원금": policy) is no topic.
        let mut mentioned = titled("청소년상담1388 온라인상담 지원", Some("welfare"));
        mentioned.program_id = Uuid::from_u128(7);
        let mut meta = HashMap::new();
        meta.insert(
            mentioned.program_id,
            (Some("카카오톡으로 상담해요".to_owned()), false, None, None),
        );
        let candidates = Candidates {
            key: (String::new(), None, String::new()),
            items: vec![mentioned],
            filter_summary: FilterSummary::default(),
            meta,
            regions: HashMap::new(),
            gate: youth_fit::GateProfile::default(),
            region: "busan",
            saved: Some("busan"),
            once_amounts: HashMap::new(),
        };
        let kakao = mazimi_core::agent::SearchArgs {
            keywords: vec!["카카오".into()],
            program_types: vec!["policy".into()],
            ..Default::default()
        };
        let found = filter_candidates(empty(intent::from_parts(&kakao)), candidates, None);
        assert!(found.results.is_empty() && found.not_found);
    }

    fn closing_in(mut row: RecommendationItem, days: i64) -> RecommendationItem {
        let today = mazimi_core::seoul_date(chrono::Utc::now());
        row.deadline = Some(
            (today + chrono::Duration::days(days))
                .format("%Y-%m-%d")
                .to_string(),
        );
        row
    }

    fn with_summaries(
        rows: Vec<(RecommendationItem, Option<&str>)>,
    ) -> (Vec<RecommendationItem>, HashMap<Uuid, RowMeta>) {
        let mut meta = HashMap::new();
        let items = rows
            .into_iter()
            .map(|(mut row, summary)| {
                row.program_id = Uuid::new_v4();
                meta.insert(
                    row.program_id,
                    (summary.map(str::to_owned), false, None, None),
                );
                row
            })
            .collect();
        (items, meta)
    }

    fn search_meta(
        args: mazimi_core::agent::SearchArgs,
        rows: Vec<(RecommendationItem, Option<&str>)>,
    ) -> Search {
        let (items, meta) = with_summaries(rows);
        let candidates = Candidates {
            key: (String::new(), None, String::new()),
            items,
            filter_summary: FilterSummary::default(),
            meta,
            regions: HashMap::new(),
            gate: youth_fit::GateProfile::default(),
            region: "busan",
            saved: Some("busan"),
            once_amounts: HashMap::new(),
        };
        filter_candidates(empty(intent::from_parts(&args)), candidates, None)
    }

    /// PR #20 review H2: "the earliest" is the earliest row that answers the
    /// question: on topic (never a summary mention) and, with words alone,
    /// one whose title says them.
    #[test]
    fn the_earliest_is_on_topic_and_keeps_the_words() {
        let housing = mazimi_core::agent::SearchArgs {
            fields: vec!["housing".into()],
            earliest: true,
            ..Default::default()
        };
        let found = search_meta(
            housing,
            vec![
                (
                    closing_in(titled("청년 상담 지원", Some("welfare")), 3),
                    Some("주거 상담도 해요"),
                ),
                (
                    closing_in(titled("청년 월세 지원", Some("housing")), 20),
                    None,
                ),
            ],
        );
        assert_eq!(titles(&found), ["청년 월세 지원"]);
        let kakao = mazimi_core::agent::SearchArgs {
            keywords: vec!["카카오".into()],
            earliest: true,
            ..Default::default()
        };
        let found = search_meta(
            kakao,
            vec![(
                closing_in(titled("청소년 상담 지원", Some("welfare")), 2),
                None,
            )],
        );
        assert!(found.results.is_empty() && found.not_found);
    }

    /// PR #20 review H3: with no field, a deadline window keeps the words as
    /// the topic (a regression on main otherwise); with a field, the rows
    /// stay and the words no row matches are reported.
    #[test]
    fn words_stay_the_topic_without_a_field_and_are_reported_with_one() {
        let window = mazimi_core::agent::SearchArgs {
            keywords: vec!["카카오".into()],
            deadline_within_days: Some(7),
            ..Default::default()
        };
        let found = search_meta(
            window,
            vec![
                (
                    closing_in(titled("청소년 상담 지원", Some("welfare")), 3),
                    None,
                ),
                (
                    closing_in(titled("청년 월세 지원", Some("housing")), 5),
                    None,
                ),
            ],
        );
        assert!(found.results.is_empty() && found.not_found);
        let field = mazimi_core::agent::SearchArgs {
            fields: vec!["housing".into()],
            keywords: vec!["전세사기".into()],
            ..Default::default()
        };
        // The catalogue knows "전세사기" (a closed row the filters drop), but
        // no listed row has it: reported.
        let found = search_meta(
            field,
            vec![
                (titled("청년 월세 지원", Some("housing")), None),
                (
                    titled("전세사기 피해 지원 (접수마감)", Some("housing")),
                    None,
                ),
            ],
        );
        assert_eq!(titles(&found), ["청년 월세 지원"]);
        assert_eq!(found.unmatched_words, ["전세사기"]);
        // "월세 지원금 받을수잇는거 알랴줘": a token the catalogue never has
        // ("알랴줘", left in by the keyword reader) is noise: never reported,
        // so never quoted back.
        let junk = mazimi_core::agent::SearchArgs {
            fields: vec!["housing".into()],
            keywords: vec!["월세".into(), "알랴줘".into()],
            ..Default::default()
        };
        let found = search_meta(
            junk,
            vec![
                (titled("청년 월세 지원", Some("housing")), None),
                (titled("청년 이사비 지원", Some("housing")), None),
            ],
        );
        assert!(
            found.unmatched_words.is_empty(),
            "{:?}",
            found.unmatched_words
        );
        // "학비 부담 덜어주는 장학금" (held-out x08): a verb that only summary
        // prose has (here a row of another field) is not a topic, so it is
        // never quoted back either.
        let prose = mazimi_core::agent::SearchArgs {
            fields: vec!["education".into()],
            keywords: vec!["덜어주".into()],
            ..Default::default()
        };
        let found = search_meta(
            prose,
            vec![
                (titled("우수학생 장학금", Some("education")), None),
                (
                    titled("청년 월세 지원", Some("housing")),
                    Some("주거비 부담을 덜어주는 지원"),
                ),
            ],
        );
        assert_eq!(titles(&found), ["우수학생 장학금"]);
        assert!(
            found.unmatched_words.is_empty(),
            "{:?}",
            found.unmatched_words
        );
    }

    #[test]
    fn closed_titles_are_read_and_conditional_ones_are_not() {
        for title in [
            "청년 월세 지원(접수마감)",
            "[마감] 장학생 모집",
            "2026 청년 인턴 모집 마감",
            "청년 저축 - 신청 마감",
            "청년 인턴 [모집종료]",
            "청년 문화패스 (추가모집 접수마감)",
            // Re-review 5 M3 (written for that review).
            "청년 월세 지원 (사업종료)",
            // Re-review 6 L1 (written for that review): closed.
            "청년 월세 지원 사업종료",
            "청년 장학금 - 접수마감되었습니다",
            "청년 장학금 (조기 신청마감)",
            "청년 수당 사업 종료",
            // Re-review 7 L1 (written for that review): closed.
            "청년 문화 (조기 종료)",
            "청년 문화 (조기종료)",
            "청년 해외 (사업종료됨)",
            "청년 저축 (2025년 사업종료)",
            "청년 장학금 (조기마감)",
            "청년 해외 사업종료됨",
        ] {
            assert!(closed_title(title), "{title}");
        }
        for title in [
            "청년 저축 [예산소진 시 조기마감]",
            "보호종료아동 자립수당",
            "[2026년 마감] 청년 저축",
            "마감 임박 장학금",
            // Re-review 4 M2 (written for that review): open programmes.
            "청년 월세 신청 마감 연장 안내",
            "청년 월세 접수마감 연장",
            "모집 마감일 안내 - 청년 인턴",
            "청년 창업 지원 (신청 마감 2026.11.30)",
            "청년 면접수당 (예산 소진 시 접수 마감)",
            "청년 교통비 (예산소진시 모집마감)",
            "신청종료일 연장 청년 저축",
            "청년수당 신청 마감 D-3",
            "폐업(사업종료) 소상공인 재기 지원",
            "마감재 지원",
            // Re-review 5 M3 (written for that review): a date or condition
            // before the word.
            "청년 인턴 11월 30일 신청 마감",
            "청년수당 (11.30. 접수마감)",
            "청년 인턴 (12/31 모집마감)",
            "청년 문화패스 (오늘 신청마감)",
            "청년 저축 - 곧 신청 마감",
            "청년 월세 지원 (선착순 모집마감)",
            "(예산 소진될 경우 접수마감)",
            "(예산 소진 후 접수마감)",
            // Re-review 6 L1 (written for that review): open.
            "청년 일자리 (금일 신청마감)",
            "청년 교통비 (내일 접수마감)",
            "청년 교통비 (이번 주 접수마감)",
            "청년 창업 (매월 말일 접수마감)",
            "청년 월세 (당일 접수마감)",
            // Re-review 7 L1 (written for that review): open.
            "청년 대출 (월말 접수마감)",
            "청년 대출 (다음 주 접수마감)",
            "청년 대출 (모레 접수마감)",
            "청년 공간 매주 금요일 접수마감",
        ] {
            assert!(!closed_title(title), "{title}");
        }
    }

    #[test]
    fn an_amount_bound_compares_stored_amounts() {
        let amount = |title: &str, monthly: Option<i32>| {
            let mut row = titled(title, Some("living"));
            row.program_id = Uuid::new_v4();
            row.benefit_amount_monthly = monthly;
            row
        };
        let args = mazimi_core::agent::SearchArgs {
            program_types: vec!["policy".into()],
            amount_min: Some(300_000),
            amount_period: Some("monthly".into()),
            ..Default::default()
        };
        let found = search_rows(
            args,
            vec![
                amount("미등록 지원", None),
                {
                    // Round 2 (code L-6): an amount for another period is
                    // not "no amount".
                    let mut row = amount("학기 지원", None);
                    row.benefit_amount_semester = Some(1_000_000);
                    row
                },
                amount("월 20만원 지원", Some(200_000)),
                amount("월 50만원 지원", Some(500_000)),
            ],
        );
        assert_eq!(
            titles(&found),
            ["월 50만원 지원", "미등록 지원"],
            "round 3: a semester-only row is not comparable with a monthly bound"
        );
        assert!(!found.results[0].amount_unregistered);
        assert!(found.results[1].amount_unregistered);
    }
}
