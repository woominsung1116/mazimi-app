use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use mazimi_core::categories::{self, Categories, CategoryRow};
use mazimi_core::corporate_categories;
use mazimi_core::models::Program;
use mazimi_core::youth_fit;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

// ── Query parameters ────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct ProgramQuery {
    /// Search text ([`search_sql`]): every whitespace-separated term must
    /// occur in the title, provider or summary. Blank = no search.
    pub q: Option<String>,
    /// Filter by program_type: policy | welfare | scholarship
    pub category: Option<String>,
    /// Backwards-compatible alias used by the web/mobile clients.
    pub program_type: Option<String>,
    /// Filter by benefit provider origin (corporate, government, etc.).
    pub provider_kind: Option<String>,
    /// Filter benefits redeemable at or otherwise linked to a brand. No
    /// current client sends it: the app browses 기업혜택 by category chips.
    /// TODO(remove brand_id): kept only for installed TestFlight build 6,
    /// which still sends it; remove this param (and the public
    /// `/api/v1/brands` list) once this release's app update is live.
    pub brand_id: Option<Uuid>,
    /// Filter by region (matched against regions TEXT[])
    pub region: Option<String>,
    /// Filter by program_status: active | upcoming | open (anything else
    /// is a 400).
    pub status: Option<String>,
    /// Sort order: deadline_asc (default) | deadline (rolling rows last) |
    /// benefit_desc | relevance
    pub sort: Option<String>,
    /// Only rows whose deadline's Korean date is within this many days of
    /// today (D-0 … D-N, 0–31).
    pub closing_within: Option<i64>,
    /// Provider tab: policy | scholarship | corporate (or 정책 | 장학금 |
    /// 기업혜택), matched against every tab a row is listed under.
    pub provider: Option<String>,
    /// Field tab: housing | living | education | welfare | finance |
    /// employment (or the Korean labels).
    pub field: Option<String>,
    /// 기업혜택 browsing chip ([`mazimi_core::corporate_categories`]): a code
    /// or its Korean label. Limits the list to the corporate tab.
    pub corporate_category: Option<String>,
    /// 탐색 소득 구간 (Figma 34065:3334): a 기준 중위소득 band code
    /// ([`mazimi_core::income::MEDIAN_BANDS`]); only rows that flag it.
    pub income_band: Option<String>,
    /// Page number (1-based, default 1)
    pub page: Option<i64>,
    /// Items per page (default 20, max 100)
    pub per_page: Option<i64>,
}

// ── Detail response ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct EligibilityRule {
    pub id: Uuid,
    pub rule_json: serde_json::Value,
    pub hard_filter_json: Option<serde_json::Value>,
    pub explain_json: Option<serde_json::Value>,
    pub version: i32,
}

/// A required document as the detail page lists it
/// ([`crate::services::required_documents`], the one source).
pub use crate::services::required_documents::RequiredDocument as ProgramDocument;

#[derive(Debug, Serialize)]
pub struct ProgramDetail {
    #[serde(flatten)]
    pub program: Program,
    /// `title` for display ([`mazimi_core::title_display`]).
    pub title_display: String,
    /// Earliest of `deadline_at`, `application_end_at` and
    /// `benefit_valid_until`; the same value recommendation cards and the
    /// dashboard order by.
    pub effective_deadline: Option<chrono::DateTime<chrono::Utc>>,
    /// Korean calendar date of `effective_deadline` (D-day basis).
    pub deadline_date: Option<chrono::NaiveDate>,
    pub eligibility_rules: Vec<EligibilityRule>,
    pub required_documents: Vec<ProgramDocument>,
    /// `※` notes of the documents text the list was split from.
    pub required_document_notes: Vec<String>,
    /// No document rows, and the notice's documents field states that
    /// nothing is to be submitted ([`mazimi_core::documents::states_none`]):
    /// the app says "공고상 별도 제출 서류 없음" instead of the unknown copy.
    pub no_documents_required: bool,
    pub brands: Vec<ProgramBrandSummary>,
    pub application_policy: crate::services::application_policy::ApplicationPolicy,
    /// 신청 방법: contact, channel and period lines and the notice's own
    /// procedure steps ([`crate::services::application_procedure::derive`]).
    pub application_procedure: crate::services::application_procedure::ApplicationProcedure,
    /// 신청 진행 단계 for a guest. A signed-in viewer's own stages come from
    /// `GET /api/v1/programs/{id}/progress` (private, never in this cacheable payload).
    pub application_progress: Vec<mazimi_core::application_procedure::StageView>,
    pub benefit_conditions: Option<String>,
    /// `benefit_conditions` cleaned for display.
    pub benefit_conditions_display: Option<String>,
    pub benefit_conditions_lines: Vec<String>,
    #[serde(flatten)]
    pub summary: SummaryDisplay,
    /// Citations for the age, region, enrollment, hard-rule, window and
    /// document facts above, tagged by `check`. Omitted when none exist.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<mazimi_core::models::CheckEvidence>,
    #[serde(flatten)]
    pub youth_fit: YouthFit,
    #[serde(flatten)]
    pub categories: Categories,
}

/// The summary cleaned for display ([`mazimi_core::display_text`]); the
/// stored `summary` is served unchanged next to it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SummaryDisplay {
    pub summary_display: Option<String>,
    pub summary_lines: Vec<String>,
}

impl SummaryDisplay {
    fn of(summary: Option<&str>) -> Self {
        let text = mazimi_core::display_text::normalize(summary);
        Self {
            summary_display: text.display,
            summary_lines: text.lines,
        }
    }
}

/// The stored youth_fit verdict of a programme (`None` = not computed yet)
/// and, for a demoted one, its neutral card label.
#[derive(Debug, Clone, Default, Serialize)]
pub struct YouthFit {
    pub youth_fit: Option<&'static str>,
    pub youth_fit_label: Option<&'static str>,
}

impl YouthFit {
    fn of(stored: Option<&str>) -> Self {
        let verdict = stored.and_then(youth_fit::Verdict::parse);
        Self {
            youth_fit: verdict.map(youth_fit::Verdict::as_str),
            youth_fit_label: verdict.and_then(youth_fit::Verdict::label),
        }
    }
}

/// The stored verdict of one programme (a reviewed verdict wins).
const YOUTH_FIT_SQL: &str = "(SELECT COALESCE(yf.reviewed_verdict, yf.verdict) \
     FROM program_youth_fit yf WHERE yf.program_id = programs.id)";

/// The profile facts that exempt youth_fit tiers for a signed-in viewer
/// (the same exemptions as the recommendation gate).
#[derive(sqlx::FromRow)]
struct ViewerProfile {
    birth_year: Option<i32>,
    household_size: Option<i32>,
    employment_status: Option<String>,
    has_disability: Option<bool>,
    veteran_family: Option<bool>,
}

impl ViewerProfile {
    fn gate(self) -> youth_fit::GateProfile {
        youth_fit::GateProfile {
            self_employed: self
                .employment_status
                .as_deref()
                .and_then(mazimi_core::canonical_employment_status)
                == Some("self_employed"),
            has_disability: self.has_disability,
            veteran_family: self.veteran_family,
            age: self
                .birth_year
                .map(|year| mazimi_core::seoul_year(chrono::Utc::now()) - year),
            household_size: self.household_size,
        }
    }
}

/// One catalogue list row.
#[derive(Debug, Serialize)]
pub struct ProgramListItem {
    #[serde(flatten)]
    pub program: Program,
    /// `title` for display ([`mazimi_core::title_display`]).
    pub title_display: String,
    /// Earliest of the three deadline columns, as on the detail page.
    pub effective_deadline: Option<chrono::DateTime<chrono::Utc>>,
    /// Korean calendar date of `effective_deadline` (D-day basis).
    pub deadline_date: Option<chrono::NaiveDate>,
    #[serde(flatten)]
    pub youth_fit: YouthFit,
    #[serde(flatten)]
    pub categories: Categories,
    #[serde(flatten)]
    pub summary: SummaryDisplay,
}

#[derive(sqlx::FromRow)]
struct ListRow {
    #[sqlx(flatten)]
    program: Program,
    youth_fit: Option<String>,
    #[sqlx(flatten)]
    categories: CategoryRow,
    /// Rows the filter matches (`count(*) OVER ()`): the total, read in the
    /// same scan as the page.
    total_rows: i64,
}

/// Every column of `programs` the API serializes, for handlers that read
/// from the `current_programs` view and must not leak provenance columns
/// through `SELECT *` in the future.
pub(crate) const PROGRAM_COLUMNS: &str =
    "id, program_type, source_type, source_id, title, summary, \
     provider_name, official_url, program_status, application_start_at, \
     application_end_at, benefit_amount_monthly, benefit_amount_semester, \
     benefit_amount_once, region_scope, school_scope, tags, raw_payload, \
     normalized_payload, min_age, max_age, regions, deadline_at, benefit_valid_until, is_active, \
     last_synced_at, created_at, updated_at, \
     company_name, company_logo_url, benefit_category, provider_kind, application_steps, \
     application_method, submission_documents, screening_method, region_districts, \
     eligibility_condition, age_basis";

/// [`PROGRAM_COLUMNS`] for a list page: the payload columns, which the
/// list never serializes, are not read (audit L7).
static LIST_COLUMNS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    PROGRAM_COLUMNS.replacen(
        "raw_payload, normalized_payload,",
        "NULL::jsonb AS raw_payload, NULL::jsonb AS normalized_payload,",
        1,
    )
});

/// Assemble the detail payload around an already loaded program row. The
/// public detail and the admin detail differ only in how they select the row
/// (view with visibility filter vs. raw table), so the sub-queries live here
/// once.
pub(crate) async fn load_program_detail(
    pool: &PgPool,
    program: Program,
) -> Result<ProgramDetail, sqlx::Error> {
    let id = program.id;
    let eligibility_rules = sqlx::query_as::<
        _,
        (
            Uuid,
            serde_json::Value,
            Option<serde_json::Value>,
            Option<serde_json::Value>,
            i32,
        ),
    >(
        "SELECT id, rule_json, hard_filter_json, explain_json, version \
         FROM eligibility_rules \
         WHERE program_id = $1 \
         ORDER BY version DESC",
    )
    .bind(id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(
        |(eid, rule_json, hard_filter_json, explain_json, version)| EligibilityRule {
            id: eid,
            rule_json,
            hard_filter_json,
            explain_json,
            version,
        },
    )
    .collect::<Vec<_>>();

    let brands = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, String, bool)>(
        "SELECT b.id, b.name, b.company_name, b.logo_url, \
                pb.relationship_type, pb.is_primary \
         FROM program_brands pb \
         JOIN brands b ON b.id = pb.brand_id \
         WHERE pb.program_id = $1 AND b.is_active = true \
         ORDER BY pb.is_primary DESC, b.discovery_priority DESC, b.name ASC",
    )
    .bind(id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(
        |(id, name, company_name, logo_url, relationship_type, is_primary)| ProgramBrandSummary {
            id,
            name,
            company_name,
            logo_url,
            relationship_type,
            is_primary,
        },
    )
    .collect::<Vec<_>>();

    let mut connection = pool.acquire().await?;
    let documents = crate::services::required_documents::load(&mut connection, &program).await?;
    let resources = crate::services::application_resources::load(&mut connection, &program).await?;
    let hard_filter = eligibility_rules
        .first()
        .and_then(|rule| rule.hard_filter_json.as_ref());
    let evidence = crate::services::evidence::load(&mut connection, &[(&program, hard_filter)])
        .await?
        .remove(&id)
        .unwrap_or_default();
    let youth_fit = sqlx::query_scalar::<_, Option<String>>(&format!(
        "SELECT {YOUTH_FIT_SQL} FROM programs WHERE programs.id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut *connection)
    .await?
    .flatten();
    let benefit_conditions = program
        .normalized_payload
        .as_ref()
        .and_then(|v| v.get("conditions"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let conditions_text = mazimi_core::display_text::normalize(benefit_conditions.as_deref());
    let categories = sqlx::query_as::<_, CategoryRow>(&format!(
        "SELECT {} FROM programs WHERE programs.id = $1",
        categories::columns_sql("programs")
    ))
    .bind(id)
    .fetch_optional(&mut *connection)
    .await?
    .unwrap_or_default();
    let application_policy =
        crate::services::application_policy::with_resources(&program, resources);
    let application_procedure =
        crate::services::application_procedure::derive(&program, &application_policy);
    Ok(ProgramDetail {
        application_procedure,
        application_progress: crate::services::application_procedure::guest_stages(),
        title_display: mazimi_core::title_display::title_display(&program.title),
        categories: categories.into(),
        youth_fit: YouthFit::of(youth_fit.as_deref()),
        effective_deadline: program.effective_deadline(),
        deadline_date: program.deadline_date(),
        summary: SummaryDisplay::of(program.summary.as_deref()),
        benefit_conditions_display: conditions_text.display,
        benefit_conditions_lines: conditions_text.lines,
        benefit_conditions,
        application_policy,
        program,
        eligibility_rules,
        no_documents_required: documents.none_required,
        required_documents: documents.items,
        required_document_notes: documents.notes,
        brands,
        evidence,
    })
}

#[derive(Debug, Serialize)]
pub struct ProgramBrandSummary {
    pub id: Uuid,
    pub name: String,
    pub company_name: Option<String>,
    pub logo_url: Option<String>,
    pub relationship_type: String,
    pub is_primary: bool,
}

// ── Paginated list response ──────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ProgramListResponse {
    pub items: Vec<ProgramListItem>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub has_next: bool,
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn db_error(e: sqlx::Error) -> (StatusCode, Json<Value>) {
    crate::errors::internal_error(e)
}

/// The viewer of a public list (from the bearer token, if any) and the
/// youth_fit exemptions of their profile. A failed profile read serves the
/// anonymous list rather than an error.
async fn viewer_gate(
    pool: &PgPool,
    secret: &crate::JwtSecret,
    headers: &HeaderMap,
) -> (Option<Uuid>, youth_fit::GateProfile) {
    let viewer = crate::auth::bearer_user(headers, &secret.0);
    let gate = match viewer {
        Some(user) => sqlx::query_as::<_, ViewerProfile>(
            "SELECT birth_year, household_size, employment_status,
                    has_disability_answer AS has_disability, veteran_family_answer AS veteran_family
             FROM user_profiles WHERE user_id = $1",
        )
        .bind(user)
        .fetch_optional(pool)
        .await
        .unwrap_or_else(|_| {
            tracing::warn!("youth_fit viewer profile read failed; serving the anonymous list");
            None
        })
        .map(ViewerProfile::gate)
        .unwrap_or_default(),
        None => youth_fit::GateProfile::default(),
    };
    (viewer, gate)
}

/// The response headers of a list that depends on the viewer's token.
fn viewer_headers(viewer: Option<Uuid>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::VARY,
        axum::http::HeaderValue::from_static("Authorization"),
    );
    if viewer.is_some() {
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("private, no-store"),
        );
    }
    headers
}

// ── GET /api/v1/programs/corporate-categories ────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CorporateCategoryCount {
    pub code: &'static str,
    pub label: &'static str,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct CorporateCategoryCounts {
    /// Chip order; categories with no visible programme are left out.
    pub items: Vec<CorporateCategoryCount>,
    /// Visible programmes of the 기업혜택 tab (the 전체 chip), including rows
    /// whose chip is not stored yet.
    pub total: i64,
}

/// Visible programmes per 기업혜택 chip, under the same `q`, `status`,
/// `region` (and every other list filter) the list gets: a chip's count is
/// the total `GET /api/v1/programs` answers with that `corporate_category`
/// and the same filters. `corporate_category` itself, paging and sort are
/// ignored.
pub async fn corporate_category_counts(
    State(pool): State<PgPool>,
    State(secret): State<crate::JwtSecret>,
    headers: HeaderMap,
    Query(params): Query<ProgramQuery>,
) -> Result<(HeaderMap, Json<CorporateCategoryCounts>), (StatusCode, Json<Value>)> {
    let (viewer, gate) = viewer_gate(&pool, &secret, &headers).await;
    let filter = list_filter(&params, gate, false)?;
    // The stored chip (mazimi_core::corporate_categories::refresh). A row not
    // filled yet (NULL) has no chip but is in 전체, as in the list.
    let sql = format!(
        "SELECT programs.browse_category AS code, COUNT(*) AS n \
         FROM current_programs programs {where_clause} AND {tab} \
         GROUP BY 1",
        where_clause = filter.where_clause,
        tab = categories::corporate_tab_sql("programs"),
    );
    let q = sqlx::query_as::<_, (Option<String>, i64)>(&sql);
    let mut rows = bind_all!(q, filter.binds)
        .fetch_all(&pool)
        .await
        .map_err(db_error)?;
    let total = rows.iter().map(|(_, n)| n).sum();
    rows.sort_by_key(|(code, _)| corporate_categories::order(code.as_deref().unwrap_or("")));
    let items: Vec<CorporateCategoryCount> = rows
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .filter_map(|(code, count)| {
            let code = corporate_categories::parse(code.as_deref()?)?;
            Some(CorporateCategoryCount {
                code,
                label: corporate_categories::label(code)?,
                count,
            })
        })
        .collect();
    Ok((
        viewer_headers(viewer),
        Json(CorporateCategoryCounts { items, total }),
    ))
}

/// One positional bind of [`ListFilter`].
pub(crate) enum Bind {
    Text(String),
    Static(&'static str),
    Uuid(Uuid),
    Int(i32),
}

/// Bind `binds` in order to a sqlx query (`query`, `query_as` or
/// `query_scalar`).
macro_rules! bind_all {
    ($query:expr, $binds:expr) => {{
        let mut q = $query;
        for bind in &$binds {
            q = match bind {
                Bind::Text(v) => q.bind(v.clone()),
                Bind::Static(v) => q.bind(*v),
                Bind::Uuid(v) => q.bind(*v),
                Bind::Int(v) => q.bind(*v),
            };
        }
        q
    }};
}
pub(crate) use bind_all;

/// The validated filters of a catalogue list as one `WHERE` clause over
/// `current_programs programs` and its binds. `$1` is the search text when
/// there is one (the relevance sort reads it).
pub(crate) struct ListFilter {
    pub(crate) where_clause: String,
    pub(crate) binds: Vec<Bind>,
    has_fts: bool,
    demoted: String,
    /// Rows shown only because a disability or veteran flag is unanswered.
    open_tier: String,
}

impl ListFilter {
    /// The next free positional parameter.
    fn next_param(&self) -> usize {
        self.binds.len() + 1
    }
}

/// Longest search text read (characters).
pub(crate) const SEARCH_CHARS: usize = 80;

/// Most search terms read; each costs one substring scan per row.
pub(crate) const SEARCH_TERMS: usize = 5;

/// The search text of a request, the one entry every search reads: at most
/// [`SEARCH_CHARS`], split on whitespace, a term another term contains
/// dropped (it adds nothing to "every term occurs"), at most
/// [`SEARCH_TERMS`] terms, joined by one space. `None` when blank (audit
/// L3: a blank search is no search). Bounds the work an anonymous request
/// can ask for (review B-M1).
pub(crate) fn search_text(q: Option<&str>) -> Option<String> {
    let text: String = q?.trim().chars().take(SEARCH_CHARS).collect();
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut terms: Vec<&str> = Vec::new();
    for (i, word) in words.iter().enumerate() {
        let implied = words
            .iter()
            .enumerate()
            .any(|(j, other)| other.contains(word) && (other.len() > word.len() || j < i));
        if !implied && terms.len() < SEARCH_TERMS {
            terms.push(word);
        }
    }
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// The terms of the search text bound at `param`, each as an escaped
/// `ILIKE` pattern (`%`, `_` and `\` are literal), in `term(pattern)`.
fn search_terms_sql(param: &str) -> String {
    format!(
        "(SELECT '%' || replace(replace(replace(t, '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%' AS pattern \
          FROM regexp_split_to_table(btrim({param}), '\\s+') AS split(t) WHERE t <> '') AS term"
    )
}

/// SQL predicate of the catalogue search (audit H2): every whitespace-
/// separated term of the text bound at `param` occurs somewhere in the
/// title, provider name or summary of the row `alias` (a substring, so
/// "월세" finds "청년월세 특별지원"). The one search the catalogue list, its
/// chip counts and the campus tab apply. Binds nothing.
pub(crate) fn search_sql(param: &str, alias: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM {terms} WHERE NOT ({alias}title ILIKE term.pattern \
           OR COALESCE({alias}provider_name, '') ILIKE term.pattern \
           OR COALESCE({alias}summary, '') ILIKE term.pattern))",
        terms = search_terms_sql(param)
    )
}

/// Whether every term occurs in the title: the relevance sort's first key.
fn title_match_sql(param: &str, alias: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM {terms} WHERE NOT {alias}title ILIKE term.pattern)",
        terms = search_terms_sql(param)
    )
}

/// `status` values the catalogue filters on.
const STATUSES: [&str; 3] = ["active", "upcoming", "open"];

pub(crate) fn list_filter(
    params: &ProgramQuery,
    gate: youth_fit::GateProfile,
    with_corporate_category: bool,
) -> Result<ListFilter, (StatusCode, Json<Value>)> {
    let category = params.category.as_ref().or(params.program_type.as_ref());
    // Audit L2: `closed` never matched (the view drops closed rows) and an
    // unknown value filtered nothing silently.
    let status = params
        .status
        .as_deref()
        .map(|v| {
            STATUSES
                .iter()
                .copied()
                .find(|s| *s == v.trim())
                .ok_or_else(|| {
                    crate::services::profile_validation::field_error(
                        "상태 값이 올바르지 않아요.",
                        "status",
                        "invalid_status",
                    )
                })
        })
        .transpose()?;
    let search = search_text(params.q.as_deref());
    let income_band = params
        .income_band
        .as_deref()
        .map(|v| {
            mazimi_core::income::median_band(v).ok_or_else(|| {
                crate::services::profile_validation::field_error(
                    "소득 구간 값이 올바르지 않아요.",
                    "income_band",
                    "invalid_income_band",
                )
            })
        })
        .transpose()?;
    let region = params
        .region
        .as_deref()
        .map(crate::services::profile_validation::region_code)
        .transpose()?;
    if params
        .closing_within
        .is_some_and(|days| !(0..=31).contains(&days))
    {
        return Err(crate::services::profile_validation::field_error(
            "마감 기간은 0~31일로 선택해 주세요.",
            "closing_within",
            "invalid_closing_within",
        ));
    }
    let invalid_category = |name: &'static str| {
        move || {
            crate::services::profile_validation::field_error(
                "분류를 다시 선택해 주세요.",
                name,
                "invalid_category",
            )
        }
    };
    let provider = params
        .provider
        .as_deref()
        .map(|v| categories::parse_provider(v).ok_or_else(invalid_category("provider")))
        .transpose()?;
    let field = params
        .field
        .as_deref()
        .map(|v| categories::parse_field(v).ok_or_else(invalid_category("field")))
        .transpose()?;
    let corporate_category = if with_corporate_category {
        params
            .corporate_category
            .as_deref()
            .map(|v| {
                corporate_categories::parse(v).ok_or_else(invalid_category("corporate_category"))
            })
            .transpose()?
    } else {
        None
    };

    // youth_fit: without a search the list leaves demoted rows out; a search
    // keeps them, after the rest. A disability or veteran tier is demoted
    // only for a viewer who answered that flag "no" (no client asks it, so
    // in practice nobody): the default list keeps those rows, after the fit
    // ones (review R-M5).
    let has_fts = search.is_some();
    let demoted = youth_fit::demoted_sql("programs.id", gate);
    // The rows the same viewer would lose by answering "no" to the flags
    // they left open: the tiers shown only because a flag is unanswered.
    let open_tier = youth_fit::demoted_sql(
        "programs.id",
        youth_fit::GateProfile {
            has_disability: gate.has_disability.or(Some(false)),
            veteran_family: gate.veteran_family.or(Some(false)),
            ..gate
        },
    );

    // Public endpoints must never expose drafts created with `publish=false`.
    // Admin listing has a separate authenticated route.
    //
    // Without a `status` filter the catalogue lists exactly the programs the
    // assistant, compare and preparation endpoints accept, so nothing shown
    // here answers 404 there. `status=upcoming|closed` asks a different
    // question and keeps the looser visibility predicate below.
    let mut conditions: Vec<String> = if status.is_none() {
        vec![mazimi_core::currently_available_sql("", "now()")]
    } else {
        vec![
            "is_active = true".to_string(),
            "(LEAST(benefit_valid_until, deadline_at, application_end_at) IS NULL OR LEAST(benefit_valid_until, deadline_at, application_end_at) > now())".to_string(),
        ]
    };
    // A school's own offer is listed only by its school's 대학 tab
    // (routes::campus), never to every viewer.
    conditions.push(categories::catalogue_sql("programs"));
    let mut binds: Vec<Bind> = Vec::new();
    // Push a bind and return its positional index.
    let mut bind = |value: Bind| {
        binds.push(value);
        binds.len()
    };

    // Search: always $1 when present.
    if let Some(q) = search {
        bind(Bind::Text(q));
        conditions.push(search_sql("$1", "programs."));
    }
    if let Some(v) = category {
        let p = bind(Bind::Text(v.clone()));
        conditions.push(format!("program_type = ${p}"));
    }
    if let Some(v) = &params.provider_kind {
        let p = bind(Bind::Text(v.clone()));
        conditions.push(format!("provider_kind = ${p}"));
    }
    if let Some(v) = params.brand_id {
        let p = bind(Bind::Uuid(v));
        conditions.push(format!(
            "EXISTS (SELECT 1 FROM program_brands pb WHERE pb.program_id = programs.id AND pb.brand_id = ${p})"
        ));
    }
    if let Some(v) = region {
        let p = bind(Bind::Static(v));
        conditions.push(format!(
            "(regions IS NULL OR cardinality(regions) = 0 OR ${p} = ANY(regions))"
        ));
    }
    // status → program_status. `active` and `open` are rows open now: their
    // start has come, as on the default list (audit L2).
    if let Some(v) = status {
        let p = bind(Bind::Static(v));
        conditions.push(format!("(program_status = ${p} OR (${p} = 'open' AND program_status IN ('always_open', 'active')))"));
        if v != "upcoming" {
            conditions.push("(application_start_at IS NULL OR application_start_at <= now()) AND (LEAST(benefit_valid_until, deadline_at, application_end_at) IS NULL OR LEAST(benefit_valid_until, deadline_at, application_end_at) > now())".into());
        }
    }
    if let Some(days) = params.closing_within {
        let p = bind(Bind::Int(days as i32));
        conditions.push(format!(
            "LEAST(benefit_valid_until, deadline_at, application_end_at) > now() \
             AND {} <= {} + ${p}::int",
            mazimi_core::deadline_date_sql(""),
            mazimi_core::SEOUL_TODAY_SQL
        ));
    }
    if let Some(v) = provider {
        let p = bind(Bind::Static(v));
        conditions.push(format!(
            "{} @> ARRAY[${p}::text]",
            categories::providers_sql("programs")
        ));
    }
    if let Some(v) = field {
        let p = bind(Bind::Static(v));
        conditions.push(format!("{} = ${p}", categories::field_sql("programs")));
    }
    if let Some(v) = corporate_category {
        let p = bind(Bind::Static(v));
        conditions.push(format!(
            "{} AND programs.browse_category = ${p}",
            categories::corporate_tab_sql("programs")
        ));
    }
    if let Some(v) = income_band {
        let p = bind(Bind::Static(v));
        // NULL / empty = the source states no band limit (migration
        // 20261015000002): such a row is open to every band, corporate
        // offers included, so it stays in the list. The bands are read from
        // the table: the `current_programs` view does not carry them.
        conditions.push(format!(
            "COALESCE((SELECT cardinality(b.income_median_bands) = 0 OR ${p} = ANY(b.income_median_bands) \
             FROM programs b WHERE b.id = programs.id AND b.income_median_bands IS NOT NULL), true)"
        ));
    }
    if !has_fts {
        conditions.push(format!("NOT {demoted}"));
    }
    Ok(ListFilter {
        where_clause: format!("WHERE {}", conditions.join(" AND ")),
        binds,
        has_fts,
        demoted,
        open_tier,
    })
}

/// The relevance sort of a search bound at `$1`.
static RELEVANCE_ORDER: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "({}) DESC, ts_rank(search_tsv, plainto_tsquery('simple', $1)) DESC, \
         LEAST(deadline_at, application_end_at, benefit_valid_until) ASC NULLS LAST, id ASC",
        title_match_sql("$1", "programs.")
    )
});

// ── GET /api/v1/programs ─────────────────────────────────────────────────────

pub async fn list_programs(
    State(pool): State<PgPool>,
    State(secret): State<crate::JwtSecret>,
    headers: HeaderMap,
    Query(params): Query<ProgramQuery>,
) -> Result<(HeaderMap, Json<ProgramListResponse>), (StatusCode, Json<Value>)> {
    let page = params.page.unwrap_or(1).clamp(1, 1_000_000);
    let per_page = params.per_page.unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * per_page;
    let (viewer, gate) = viewer_gate(&pool, &secret, &headers).await;
    let filter = list_filter(&params, gate, true)?;
    let ListFilter {
        ref where_clause,
        has_fts,
        ref demoted,
        ref open_tier,
        ..
    } = filter;

    // "relevance" is only meaningful when a FTS query is present; fall back to
    // deadline_asc otherwise to keep results deterministic.
    let order_clause = match params.sort.as_deref() {
        Some("benefit_desc") => {
            "(CASE WHEN program_type = 'corporate_benefit' THEN 0 ELSE COALESCE(benefit_amount_monthly, 0) + COALESCE(benefit_amount_semester, 0) END) DESC NULLS LAST, LEAST(deadline_at, application_end_at, benefit_valid_until) ASC NULLS LAST, id ASC"
        }
        // Title matches first; the word index only breaks ties.
        Some("relevance") if has_fts => &*RELEVANCE_ORDER,
        // Rolling (상시) rows last: no deadline, or always open.
        Some("deadline") => {
            "(LEAST(deadline_at, application_end_at, benefit_valid_until) IS NULL OR program_status = 'always_open') ASC, LEAST(deadline_at, application_end_at, benefit_valid_until) ASC NULLS LAST, id ASC"
        }
        // deadline_asc is the default
        _ => "LEAST(deadline_at, application_end_at, benefit_valid_until) ASC NULLS LAST, id ASC",
    };

    // ── Count query (only for a page past the end; a page with rows reads
    // the total in its own scan) ─────────────────────────────────────────────
    let count_sql = format!("SELECT COUNT(*) FROM current_programs programs {where_clause}");

    // ── Items query ──────────────────────────────────────────────────────────
    // Limit/offset use the next two positional params after all filters.
    let limit_param = filter.next_param();
    let offset_param = limit_param + 1;
    let demoted_last = if has_fts {
        format!("({demoted}) ASC, ")
    } else {
        format!("({open_tier}) ASC, ")
    };
    let category_columns = categories::columns_sql("programs");
    let list_columns = &*LIST_COLUMNS;
    let items_sql = format!(
        "SELECT {list_columns}, {YOUTH_FIT_SQL} AS youth_fit, {category_columns}, \
                count(*) OVER () AS total_rows \
         FROM current_programs programs {where_clause} \
         ORDER BY {demoted_last}{order_clause} \
         LIMIT ${limit_param} OFFSET ${offset_param}"
    );

    let rows: Vec<ListRow> = {
        let q = sqlx::query_as::<_, ListRow>(&items_sql);
        let q = bind_all!(q, filter.binds);
        let q = q.bind(per_page).bind(offset);
        q.fetch_all(&pool).await.map_err(db_error)?
    };
    let total: i64 = match rows.first() {
        Some(row) => row.total_rows,
        None => {
            let q = sqlx::query_scalar::<_, i64>(&count_sql);
            let q = bind_all!(q, filter.binds);
            q.fetch_one(&pool).await.map_err(db_error)?
        }
    };

    let items: Vec<ProgramListItem> = {
        rows.into_iter()
            .map(|row| ProgramListItem {
                title_display: mazimi_core::title_display::title_display(&row.program.title),
                effective_deadline: row.program.effective_deadline(),
                deadline_date: row.program.deadline_date(),
                summary: SummaryDisplay::of(row.program.summary.as_deref()),
                youth_fit: YouthFit::of(row.youth_fit.as_deref()),
                categories: row.categories.into(),
                program: row.program,
            })
            .collect()
    };

    let has_next = offset + per_page < total;

    // The list depends on the viewer's token: never share it between users.
    Ok((
        viewer_headers(viewer),
        Json(ProgramListResponse {
            items,
            total,
            page,
            per_page,
            has_next,
        }),
    ))
}

// ── GET /api/v1/programs/{id} ────────────────────────────────────────────────

pub async fn get_program(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<ProgramDetail>, (StatusCode, Json<Value>)> {
    // Fetch the program row
    let program = sqlx::query_as::<_, Program>(&format!(
        "SELECT {PROGRAM_COLUMNS} \
         FROM current_programs WHERE id = $1 AND is_active = true AND (LEAST(benefit_valid_until, deadline_at, application_end_at) IS NULL OR LEAST(benefit_valid_until, deadline_at, application_end_at) > now())"
    ))
    .bind(id)
    .fetch_optional(&pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Program not found" })),
        )
    })?;

    load_program_detail(&pool, program)
        .await
        .map(Json)
        .map_err(db_error)
}

#[cfg(test)]
mod tests {
    /// Review B-M1: a search reads at most five distinct terms; a repeated
    /// or contained term is dropped, a blank one is no search.
    #[test]
    fn a_search_reads_a_bounded_set_of_terms() {
        assert_eq!(search_text(Some("  ")), None);
        assert_eq!(
            search_text(Some(" 청년  월세 ")).as_deref(),
            Some("청년 월세")
        );
        assert_eq!(
            search_text(Some("월세 청년월세 월세 청년")).as_deref(),
            Some("청년월세")
        );
        let many = (0..40)
            .map(|n| format!("t{n:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            search_text(Some(&many)).unwrap().split(' ').count(),
            SEARCH_TERMS
        );
        assert_eq!(search_text(Some("a a a a a a a a")).as_deref(), Some("a"));
    }

    use super::*;
    use std::collections::HashSet;

    /// 탐색 소득 구간: a stored band code filters on the flagged bands and keeps
    /// rows without a band limit; any other value is a 400, never an
    /// unfiltered list.
    #[test]
    fn income_band_filters_on_flagged_bands() {
        let gate = youth_fit::GateProfile::default();
        let band = ProgramQuery {
            income_band: Some("51-75".into()),
            ..ProgramQuery::default()
        };
        let filter = list_filter(&band, gate, false).expect("valid band");
        assert!(
            filter.where_clause.contains("= ANY(b.income_median_bands)"),
            "{}",
            filter.where_clause
        );
        let none = list_filter(&ProgramQuery::default(), gate, false).expect("no band");
        assert!(!none.where_clause.contains("income_median_bands"));
        let bad = ProgramQuery {
            income_band: Some("le100".into()),
            ..ProgramQuery::default()
        };
        assert!(list_filter(&bad, gate, false).is_err());
    }

    /// `#[sqlx(default)]` fields read `None` when a column list omits them,
    /// so every explicit `Program` column list must name every field the
    /// model serializes.
    #[test]
    fn explicit_column_lists_name_every_program_field() {
        let now = chrono::Utc::now();
        let program: Program = serde_json::from_value(json!({
            "id": Uuid::nil(), "program_type": "policy", "source_type": "manual",
            "title": "t", "program_status": "active", "is_active": true,
            "created_at": now, "updated_at": now
        }))
        .unwrap();
        let fields = serde_json::to_value(program).unwrap();
        assert_ne!(
            *LIST_COLUMNS, PROGRAM_COLUMNS,
            "the list drops the payloads"
        );
        for (name, list) in [
            ("PROGRAM_COLUMNS", PROGRAM_COLUMNS),
            ("LIST_COLUMNS", LIST_COLUMNS.as_str()),
            (
                "DECISION_COLUMNS",
                crate::services::recommendations::decision_columns(),
            ),
        ] {
            let words: HashSet<&str> = list
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .collect();
            for field in fields.as_object().unwrap().keys() {
                assert!(words.contains(field.as_str()), "{name} lacks {field}");
            }
        }
    }
}
