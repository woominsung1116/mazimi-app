//! 금융감독원 금융상품 비교공시 (finlife) API client
//!
//! Every youth product of four kinds the API discloses, in every financial
//! sector. Kinds ([`KINDS`]) and sectors ([`SECTORS`], the `topFinGrpNo`
//! codes) are two tables; each (kind, sector) pair is one *cell*, read,
//! validated and settled on its own ([`collect_cells`]). Each cell is one
//! [`PartOutcome`] of the fetch, named by its id prefix, so lifecycle
//! reconciliation leaves an unsettled cell's programmes alone while every
//! other cell is ingested and reconciled. How long a programme may stay
//! shown without being listed again is the pipeline's freshness rule
//! (`pipeline::LISTED_WITHIN_DAYS`), not this module's.
//!
//! Not read:
//! - `annuitySavingProductsSearch` (연금저축): the provider's web firewall
//!   answers it with an HTML block page (HTTP 200, measured 2026-11-10).
//! - `creditLoanProductsSearch` (개인신용대출): its rows are institution-level
//!   rate tables (every captured row is 일반신용대출, 마이너스한도대출 or
//!   장기카드대출, rates averaged per credit grade), not products, so no row
//!   can be a youth product by name and the five cells would only add
//!   requests and failure surface (captured 2026-11-10, 73 rows).
//! - 개인사업자대출: a business loan, not a personal youth product.
//!
//! What this API cannot give: 청년도약계좌, 청년희망적금,
//! 청년주택드림청약통장 and the 주택도시기금 loans (버팀목, 디딤돌) are not in
//! these endpoints (0 of 644 captured rows). They need their own source.
//!
//! Provider contract (https://finlife.fss.or.kr/finlife/api/fncCoApi/list.do?menuNo=700051
//! and https://finlife.fss.or.kr/finlife/api/finlifeApiKey/list.do?menuNo=700034,
//! read 2026-11-11):
//! - `err_cd`: 000 정상; 010 미등록 인증키; 011 중지된 인증키; 012 삭제된
//!   인증키; 013 샘플 인증키; 020 일일검색 허용횟수 초과; 021 허용된 IP가
//!   아님; 100 요청변수 누락; 101 요청변수의 부적절한 값; 900 정의되지 않은
//!   오류. The key, quota and IP codes concern every request, so the run
//!   stops asking ([`CellError::SourceRejected`]); 100/101 mean our request
//!   is wrong and need code ([`CellError::RequestRejected`]); 900 and any
//!   other code fail the cell.
//! - Quota: an individual key may make 10,000 queries a day; an
//!   organisation key has no query limit but an IP restriction. A key unused
//!   for 12 months is deleted without notice. 100 rows per query.
//!
//! Env: FSS_API_KEY, sent as the query parameter `auth`. Request errors are
//! stripped of their URL ([`http::send`]), redirects are not followed and no
//! Referer is sent, so the key never reaches a log, a stored error or
//! another host.
//!
//! Filter: [`youth_decision`], the shared audience classifier fed with the
//! 가입대상 band this module reads.
//!
//! Ids: `fss_{stem}_{fin_co_no}_{fin_prdt_cd}` for banks (the ids of the
//! deposits, savings and mortgage loans collected before the other kinds and
//! sectors were added) and `fss_{topFinGrpNo}_{stem}_…` for every other
//! sector. `fin_co_no` must be digits, so the first `_` after it ends it.
//! No cell's prefix begins another cell's ids ([`ProductKind::cell_prefix`]).
//!
//! Cost: requests are sequential, [`REQUEST_PACE`] apart, at most
//! [`MAX_PAGES_PER_CELL`] pages per cell, each with the client's 30 s
//! timeout and the shared retry policy, the whole fetch within
//! [`RUN_BUDGET`]; [`BREAKER_CELLS`] cells of one kind in a row failing the
//! same way stop that kind, a key/quota/IP rejection stops the run. Measured 2026-11-10 (100 products a page): deposits 8
//! pages (savings banks 4), savings 7 (savings banks 3), mortgage and rent
//! loans 5 each (one per sector): 25 requests and about 7 s of pacing per
//! run, plus at most one more read of each cell that failed.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use mazimi_core::audience::{self, AudienceText};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::time::Instant;
use tracing::{info, warn};

use super::{
    content_hash, ensure_complete, http, DataSource, PartOutcome, PartScope, PartState, RawRecord,
    SourceFetch, Unsettled,
};

const BASE_URL: &str = "https://finlife.fss.or.kr/finlifeapi/";
/// Pages one cell may take. The largest cell measured had 4.
const MAX_PAGES_PER_CELL: u32 = 20;
/// Fixed gap between two requests of a run.
const REQUEST_PACE: Duration = Duration::from_millis(300);
/// The whole fetch. A healthy run takes well under a minute; a provider that
/// hangs costs at most this plus one request (3 × 30 s), far below the daily
/// job's 30-minute limit, and what was read is still returned.
const RUN_BUDGET: Duration = Duration::from_secs(300);
/// Cells of one kind in a row that fail the same way, with nothing read,
/// before the run stops asking that endpoint.
const BREAKER_CELLS: usize = 3;
/// Option rows one product may carry; the largest measured had 24.
const MAX_OPTION_ROWS: usize = 200;
/// Characters one text field may hold; the longest measured had 235.
const MAX_TEXT_CHARS: usize = 2000;
/// Rows a cell needs before "a field the filter reads is empty on every
/// row" means the provider moved the field rather than a small sector.
const MIN_ROWS_FOR_SHAPE_CHECK: usize = 20;
const SOURCE: &str = "fss_financial";

/// How a kind is shown: a savings product (가입대상, rates by term), or a
/// loan (대출한도, lending rates, never an age band unless the product gives
/// one). No kind gets an application window: a disclosure period is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presentation {
    Savings,
    Loan,
}

/// One product kind of the API.
#[derive(Debug)]
pub struct ProductKind {
    pub key: &'static str,
    /// Endpoint file name under [`BASE_URL`].
    pub endpoint: &'static str,
    /// Id stem: `fss_{stem}_…` (see the module doc).
    id_stem: &'static str,
    /// Label shown in the title: "{institution} - {product} ({tag})".
    pub tag: &'static str,
    pub presentation: Presentation,
    /// The 가입대상 field the age band is read from; loans have none.
    eligibility_field: Option<&'static str>,
    /// Fields every base row of the kind carries (a value may be null).
    base_fields: &'static [&'static str],
    /// Fields every option row of the kind carries; the rates read them.
    option_fields: &'static [&'static str],
}

pub const DEPOSIT: ProductKind = ProductKind {
    key: "deposit",
    endpoint: "depositProductsSearch.json",
    id_stem: "deposit",
    tag: "예금",
    presentation: Presentation::Savings,
    eligibility_field: Some("join_member"),
    base_fields: &["join_member", "join_way"],
    option_fields: &["intr_rate", "intr_rate2", "save_trm"],
};

pub const SAVING: ProductKind = ProductKind {
    key: "saving",
    endpoint: "savingProductsSearch.json",
    id_stem: "saving",
    tag: "적금",
    ..DEPOSIT
};

pub const MORTGAGE_LOAN: ProductKind = ProductKind {
    key: "mortgage_loan",
    endpoint: "mortgageLoanProductsSearch.json",
    // `fss_loan_` is the id of the mortgage loans collected before rent
    // loans were added.
    id_stem: "loan",
    tag: "주택담보대출",
    presentation: Presentation::Loan,
    eligibility_field: None,
    base_fields: &["loan_lmt", "join_way"],
    option_fields: &["lend_rate_min", "lend_rate_max"],
};

pub const RENT_HOUSE_LOAN: ProductKind = ProductKind {
    key: "rent_house_loan",
    endpoint: "rentHouseLoanProductsSearch.json",
    id_stem: "rent_loan",
    tag: "전월세대출",
    ..MORTGAGE_LOAN
};

pub const KINDS: [&ProductKind; 4] = [&DEPOSIT, &SAVING, &MORTGAGE_LOAN, &RENT_HOUSE_LOAN];

/// A financial sector (`topFinGrpNo`).
#[derive(Debug)]
pub struct Sector {
    pub code: &'static str,
    pub name: &'static str,
    /// Whether ids carry the sector code. Banks do not: their ids predate
    /// the other sectors.
    qualifies_ids: bool,
}

pub const BANK: Sector = Sector {
    code: "020000",
    name: "은행",
    qualifies_ids: false,
};
pub const CREDIT_FINANCE: Sector = Sector {
    code: "030200",
    name: "여신전문금융",
    qualifies_ids: true,
};
pub const SAVINGS_BANK: Sector = Sector {
    code: "030300",
    name: "저축은행",
    qualifies_ids: true,
};
pub const INSURANCE: Sector = Sector {
    code: "050000",
    name: "보험",
    qualifies_ids: true,
};
pub const INVESTMENT: Sector = Sector {
    code: "060000",
    name: "금융투자",
    qualifies_ids: true,
};

pub const SECTORS: [&Sector; 5] = [
    &BANK,
    &CREDIT_FINANCE,
    &SAVINGS_BANK,
    &INSURANCE,
    &INVESTMENT,
];

impl ProductKind {
    /// The id prefix of every product of this kind in `sector`: the unit
    /// lifecycle reconciliation protects when the cell is not settled.
    pub fn cell_prefix(&self, sector: &Sector) -> String {
        if sector.qualifies_ids {
            format!("fss_{}_{}_", sector.code, self.id_stem)
        } else {
            format!("fss_{}_", self.id_stem)
        }
    }

    /// `_source_prefix` of the payload: the kind, whatever the sector.
    fn source_prefix(&self) -> String {
        format!("fss_{}", self.id_stem)
    }
}

pub struct FssFinancialSource {
    client: Client,
    api_key: String,
    base_url: String,
}

impl FssFinancialSource {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                // The key is a query parameter: a followed redirect would
                // carry it to another host, in the request and in the
                // Referer. A redirect answer is a cell failure instead.
                .redirect(reqwest::redirect::Policy::none())
                .referer(false)
                // The provider closes generic persistent-client requests
                // without an HTTP response. Keep TLS verification and use
                // explicit JSON/API identity with a fresh HTTP/1 connection.
                .http1_only()
                .default_headers(reqwest::header::HeaderMap::from_iter([
                    (
                        reqwest::header::ACCEPT,
                        reqwest::header::HeaderValue::from_static("application/json"),
                    ),
                    (
                        reqwest::header::CONNECTION,
                        reqwest::header::HeaderValue::from_static("close"),
                    ),
                ]))
                .user_agent("Mazimi/1.0 (official API client; https://mazimi.kr)")
                .build()
                .expect("reqwest client with timeout must build"),
            api_key,
            base_url: BASE_URL.to_string(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_base_url(mut self, base_url: &str) -> Self {
        self.base_url = base_url.to_string();
        self
    }

    /// Returns None (and logs a warning) when FSS_API_KEY is not set so the
    /// sync pipeline can skip this source gracefully.
    pub fn from_env() -> Option<Self> {
        match std::env::var("FSS_API_KEY") {
            Ok(key) if !key.is_empty() => Some(Self::new(key)),
            _ => {
                warn!("FSS_API_KEY not set — skipping 금융감독원 financial product sync");
                None
            }
        }
    }

    /// One page of one cell, as text. Errors never carry the URL.
    pub(crate) async fn fetch_body(
        &self,
        kind: &ProductKind,
        sector: &Sector,
        page: u32,
    ) -> Result<String> {
        let url = format!("{}{}", self.base_url, kind.endpoint);
        let response = http::send(
            self.client.get(url).query(&[
                ("auth", self.api_key.as_str()),
                ("topFinGrpNo", sector.code),
                ("pageNo", &page.to_string()),
            ]),
            SOURCE,
        )
        .await?;
        http::text(response, SOURCE).await
    }
}

impl DataSource for FssFinancialSource {
    fn name(&self) -> &'static str {
        SOURCE
    }

    async fn fetch_all(&self) -> Result<SourceFetch> {
        collect_cells(
            |kind, sector, page| self.fetch_body(kind, sector, page),
            Limits::production(),
        )
        .await
    }
}

// ── Cells ─────────────────────────────────────────────────────────────────────

/// Why a cell was not settled. The text is stored and logged, so it never
/// holds the request URL or the provider's free-text message.
#[derive(Debug)]
pub enum CellError {
    /// The request failed (already stripped of its URL).
    Transport(String),
    /// The provider answered with a redirect, which is never followed.
    Redirected(u16),
    /// Not JSON: the firewall's HTML block page, an empty or cut body.
    NonJson,
    /// The provider's own error for this request (`err_cd` 900 or unknown).
    UpstreamRejected(String),
    /// The key, the quota or the IP was rejected: no other cell can answer.
    SourceRejected(String),
    /// The provider says our request is malformed (`err_cd` 100/101).
    RequestRejected(String),
    /// JSON without the fields this collector reads.
    SchemaChanged(String),
    /// Fewer products than the provider's own total.
    ShortRead { delivered: usize, total: u64 },
    /// More pages than [`MAX_PAGES_PER_CELL`].
    PageLimit,
    /// Not requested: the run's budget was spent or the run stopped asking.
    NotAttempted(String),
}

impl CellError {
    /// Stable reason code (log field `reason`, stored with the part).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Transport(_) => "transport",
            Self::Redirected(_) => "redirected",
            Self::NonJson => "non_json_body",
            Self::UpstreamRejected(_) => "upstream_rejected",
            Self::SourceRejected(_) => "source_rejected",
            Self::RequestRejected(_) => "request_rejected",
            Self::SchemaChanged(_) => "schema_changed",
            Self::ShortRead { .. } => "short_read",
            Self::PageLimit => "page_limit",
            Self::NotAttempted(_) => "not_attempted",
        }
    }

    /// Fixed text and counts, without the code.
    fn detail(&self) -> String {
        match self {
            Self::Transport(detail) | Self::SchemaChanged(detail) | Self::NotAttempted(detail) => {
                detail.clone()
            }
            Self::Redirected(status) => format!("HTTP {status} redirect not followed"),
            Self::UpstreamRejected(err_cd)
            | Self::SourceRejected(err_cd)
            | Self::RequestRejected(err_cd) => format!("err_cd {err_cd}"),
            Self::ShortRead { delivered, total } => {
                format!("{delivered} of {total} products delivered")
            }
            Self::NonJson => "the body is not JSON".into(),
            Self::PageLimit => format!("more than {MAX_PAGES_PER_CELL} pages"),
        }
    }

    /// A failure another read may not repeat.
    fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Transport(_) | Self::NonJson | Self::UpstreamRejected(_) | Self::ShortRead { .. }
        )
    }
}

impl std::fmt::Display for CellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.detail())
    }
}

/// The class of an `err_cd` other than 000 (see the module doc).
fn rejection(err_cd: &str) -> CellError {
    let code: String = err_cd
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    match code.as_str() {
        "010" | "011" | "012" | "013" | "020" | "021" => CellError::SourceRejected(code),
        "100" | "101" => CellError::RequestRejected(code),
        _ => CellError::UpstreamRejected(code),
    }
}

/// One product: its base row and its option rows (rates).
#[derive(Debug)]
pub struct Product {
    pub base: FssBaseProduct,
    pub options: Vec<Value>,
}

/// One validated page.
#[derive(Debug, Default)]
pub struct Page {
    /// Products of the cell across all pages (`total_count`).
    pub total: Option<u64>,
    pub last_page: u32,
    pub products: Vec<Product>,
    /// Base rows the provider returned, the set-aside ones included.
    pub rows: usize,
    /// Rows (base or option) set aside as defective.
    pub set_aside: usize,
    /// Base rows with a product name.
    pub named: usize,
    /// Base rows with a non-empty eligibility field (savings kinds).
    pub with_eligibility: usize,
    /// Company and product code of set-aside base rows that still name
    /// their product.
    pub set_aside_keys: Vec<(String, String)>,
}

fn schema(detail: impl Into<String>) -> CellError {
    CellError::SchemaChanged(detail.into())
}

fn non_empty_str<'a>(row: &'a Map<String, Value>, field: &str) -> Option<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// Company and product code of a row. `fin_co_no` must be ASCII digits so
/// that ids stay self-delimiting; `fin_prdt_cd` must be printable ASCII of at
/// most [`MAX_PRODUCT_CODE_LEN`] bytes.
fn product_key(row: &Map<String, Value>) -> Option<(String, String)> {
    let company = non_empty_str(row, "fin_co_no")?;
    if !company.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // The product code goes into the id: bounded, printable, no spaces.
    let code = non_empty_str(row, "fin_prdt_cd")?.trim();
    if code.len() > MAX_PRODUCT_CODE_LEN || !code.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    Some((company.to_string(), code.to_string()))
}

/// Bytes a product code may have; the longest measured has 16.
const MAX_PRODUCT_CODE_LEN: usize = 60;

fn over_long(row: &Map<String, Value>) -> bool {
    row.values().any(|value| {
        value
            .as_str()
            .is_some_and(|s| s.chars().count() > MAX_TEXT_CHARS)
    })
}

/// Parse and validate one page of `kind`. Envelope and field-shape defects
/// (a list missing, a field gone from every row) fail the cell; a defect in
/// one row sets that row aside and is counted.
pub fn parse_page(kind: &ProductKind, body: &str) -> Result<Page, CellError> {
    let envelope: Value = serde_json::from_str(body).map_err(|_| CellError::NonJson)?;
    let result = envelope
        .get("result")
        .and_then(Value::as_object)
        .ok_or_else(|| schema("missing result"))?;
    let err_cd = result
        .get("err_cd")
        .and_then(Value::as_str)
        .ok_or_else(|| schema("missing err_cd"))?;
    if err_cd != "000" {
        return Err(rejection(err_cd));
    }
    let last_page = result
        .get("max_page_no")
        .and_then(lenient_count)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| schema("missing max_page_no"))?;
    let total = result.get("total_count").and_then(lenient_count);
    let list = |name: &str| {
        result
            .get(name)
            .and_then(Value::as_array)
            .ok_or_else(|| schema(format!("missing {name}")))
    };
    let (base_rows, option_rows) = (list("baseList")?, list("optionList")?);
    let gone_from_every_row = |rows: &[Value], fields: &[&'static str]| {
        fields.iter().copied().find(|field| {
            !rows.is_empty()
                && rows
                    .iter()
                    .all(|row| row.as_object().is_none_or(|map| !map.contains_key(*field)))
        })
    };
    if let Some(field) = gone_from_every_row(base_rows, kind.base_fields) {
        return Err(schema(format!("{field} missing on every product row")));
    }
    if let Some(field) = gone_from_every_row(option_rows, kind.option_fields) {
        return Err(schema(format!("{field} missing on every option row")));
    }

    let mut page = Page {
        total,
        last_page,
        rows: base_rows.len(),
        ..Page::default()
    };
    let mut options: HashMap<(String, String), Vec<Value>> = HashMap::new();
    for row in option_rows {
        let usable = row.as_object().and_then(|map| {
            let complete = kind.option_fields.iter().all(|f| map.contains_key(*f));
            (complete && !over_long(map))
                .then(|| product_key(map))
                .flatten()
        });
        match usable {
            Some(key) => options.entry(key).or_default().push(row.clone()),
            None => page.set_aside += 1,
        }
    }

    for row in base_rows {
        let Some(map) = row.as_object() else {
            page.set_aside += 1;
            continue;
        };
        if non_empty_str(map, "fin_prdt_nm").is_some() {
            page.named += 1;
        }
        if kind
            .eligibility_field
            .is_some_and(|field| non_empty_str(map, field).is_some())
        {
            page.with_eligibility += 1;
        }
        let key = product_key(map);
        let base = serde_json::from_value::<FssBaseProduct>(row.clone()).ok();
        let product_options = key
            .as_ref()
            .and_then(|key| options.remove(key))
            .unwrap_or_default();
        let usable = key.is_some()
            && non_empty_str(map, "fin_prdt_nm").is_some()
            && non_empty_str(map, "kor_co_nm").is_some()
            && kind.base_fields.iter().all(|f| map.contains_key(*f))
            && !over_long(map)
            && product_options.len() <= MAX_OPTION_ROWS;
        match base.filter(|_| usable) {
            Some(base) => page.products.push(Product {
                base,
                options: product_options,
            }),
            None => {
                page.set_aside += 1;
                page.set_aside_keys.extend(key);
            }
        }
    }
    let orphans: usize = options.values().map(Vec::len).sum();
    if orphans > 0 {
        warn!(
            source = SOURCE,
            event = "fss_orphan_option_rows",
            kind = kind.key,
            orphans,
            "option rows without a product on the same page were not attached"
        );
    }
    Ok(page)
}

/// What one cell delivered.
#[derive(Default)]
struct CellRead {
    records: Vec<RawRecord>,
    /// Ids of rows the youth decision left out.
    filtered_out: Vec<String>,
    /// Ids of set-aside rows that name their product.
    set_aside_ids: Vec<String>,
    /// Rows the provider returned for the cell, before the youth filter.
    seen: usize,
    /// Rows set aside as defective, repeated ids included.
    set_aside: usize,
    /// Set when the cell is not settled; `records` still holds what was read.
    problem: Option<CellError>,
}

impl CellRead {
    fn not_attempted(reason: &str) -> Self {
        Self {
            problem: Some(CellError::NotAttempted(reason.to_string())),
            ..Self::default()
        }
    }
}

/// The fetch limits of a run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub pace: Duration,
    pub budget: Duration,
    pub breaker: usize,
    /// The Korean calendar day the run judges disclosure end days against.
    pub today: chrono::NaiveDate,
}

impl Limits {
    pub fn production() -> Self {
        Self {
            pace: REQUEST_PACE,
            budget: RUN_BUDGET,
            breaker: BREAKER_CELLS,
            today: kst_today(),
        }
    }
}

/// Request bookkeeping shared by every cell of a run.
struct Run {
    limits: Limits,
    deadline: Instant,
    requests: u32,
}

/// Read every page of one cell.
async fn read_cell<F, Fut>(
    kind: &'static ProductKind,
    sector: &'static Sector,
    fetch: &mut F,
    run: &mut Run,
) -> CellRead
where
    F: FnMut(&'static ProductKind, &'static Sector, u32) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let mut read = CellRead::default();
    let mut total: Option<u64> = None;
    let mut ids = HashSet::new();
    let (mut named, mut with_eligibility) = (0usize, 0usize);
    for page in 1..=MAX_PAGES_PER_CELL {
        if Instant::now() >= run.deadline {
            read.problem = Some(CellError::NotAttempted(format!(
                "the run budget was spent before page {page}"
            )));
            return read;
        }
        if run.requests > 0 && !run.limits.pace.is_zero() {
            tokio::time::sleep(run.limits.pace).await;
        }
        run.requests += 1;
        let parsed = match fetch(kind, sector, page).await {
            Ok(body) => parse_page(kind, &body),
            Err(error) => Err(match error.downcast_ref::<http::UpstreamStatus>() {
                Some(status) if (300..400).contains(&status.status) => {
                    CellError::Redirected(status.status)
                }
                _ => CellError::Transport(format!("{error:#}")),
            }),
        };
        let parsed = match parsed {
            Ok(parsed) => parsed,
            Err(problem) => {
                read.problem = Some(problem);
                return read;
            }
        };
        // The cell's total is the largest any page gave: a later page
        // answering "total 0" disagrees with page 1, it does not end the list.
        total = match (total, parsed.total) {
            (Some(earlier), Some(now)) => Some(earlier.max(now)),
            (earlier, now) => now.or(earlier),
        };
        if parsed.rows == 0 {
            read.problem = check_empty_page(page, read.seen, total).err();
            return read;
        }
        read.seen += parsed.rows;
        read.set_aside += parsed.set_aside;
        read.set_aside_ids.extend(
            parsed
                .set_aside_keys
                .iter()
                .map(|(company, code)| format!("{}{company}_{code}", kind.cell_prefix(sector))),
        );
        named += parsed.named;
        with_eligibility += parsed.with_eligibility;
        for product in &parsed.products {
            let id = product_id(kind, sector, &product.base);
            if !ids.insert(id.clone()) {
                warn!(
                    source = SOURCE,
                    event = "fss_duplicate_product_id",
                    kind = kind.key,
                    sector = sector.code,
                    "a product id appears twice in one cell; the repeat is set aside"
                );
                read.set_aside += 1;
                continue;
            }
            // A disclosure whose end day has passed is over: not kept, so
            // the usual retirement applies whether or not the row changed.
            let ended = parse_fss_date(product.base.dcls_end_day.as_deref())
                .is_some_and(|end| end < run.limits.today);
            match youth_decision(kind, &product.base).filter(|_| !ended) {
                Some(basis) => read
                    .records
                    .push(product_record(kind, sector, product, basis)),
                None => read.filtered_out.push(id),
            }
        }
        if page >= parsed.last_page {
            if read.seen >= MIN_ROWS_FOR_SHAPE_CHECK {
                let empty_field = if named == 0 {
                    Some("fin_prdt_nm")
                } else if with_eligibility == 0 {
                    kind.eligibility_field
                } else {
                    None
                };
                if let Some(field) = empty_field {
                    read.problem = Some(schema(format!(
                        "{field} is empty on all {} rows",
                        read.seen
                    )));
                    return read;
                }
            }
            if !ensure_complete(SOURCE, read.seen, total) {
                read.problem = Some(CellError::ShortRead {
                    delivered: read.seen,
                    total: total.unwrap_or_default(),
                });
            }
            return read;
        }
    }
    read.problem = Some(CellError::PageLimit);
    read
}

/// Failure codes that, repeated cell after cell with nothing read, mean the
/// provider is not answering.
const BREAKER_CODES: &[&str] = &[
    "transport",
    "redirected",
    "non_json_body",
    "upstream_rejected",
];

/// Read every cell, in table order, one request at a time, within the run's
/// budget, then read once more each cell that failed in a way another read
/// may not repeat. Only what is source-wide by definition stops the run: a
/// key, quota or IP rejection, and the budget. [`Limits::breaker`] cells of
/// one kind (one endpoint) in a row failing the same way with nothing read
/// stop that kind only: a firewall blocks an endpoint, not the provider
/// (`annuitySavingProductsSearch`). Cells not reached are `not_attempted`.
/// What was read is returned; a run in which no cell answered fails.
pub(crate) async fn collect_cells<F, Fut>(mut fetch: F, limits: Limits) -> Result<SourceFetch>
where
    F: FnMut(&'static ProductKind, &'static Sector, u32) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let cells: Vec<(&'static ProductKind, &'static Sector)> = KINDS
        .iter()
        .flat_map(|kind| SECTORS.iter().map(move |sector| (*kind, *sector)))
        .collect();
    let mut run = Run {
        limits,
        deadline: Instant::now() + limits.budget,
        requests: 0,
    };
    let mut reads: Vec<CellRead> = Vec::with_capacity(cells.len());
    let mut stop: Option<String> = None;
    let mut kind_stops: HashMap<&'static str, String> = HashMap::new();
    let mut streak: (&'static str, &'static str, usize) = ("", "", 0);
    for (kind, sector) in &cells {
        if let Some(reason) = stop.as_ref().or_else(|| kind_stops.get(kind.key)) {
            reads.push(CellRead::not_attempted(reason));
            continue;
        }
        let read = read_cell(kind, sector, &mut fetch, &mut run).await;
        match &read.problem {
            Some(CellError::SourceRejected(code)) => {
                stop = Some(format!(
                    "the provider rejected the key, quota or IP (err_cd {code})"
                ));
            }
            Some(CellError::NotAttempted(_)) => stop = Some("the run budget was spent".into()),
            Some(problem) if read.seen == 0 && BREAKER_CODES.contains(&problem.code()) => {
                streak = if (streak.0, streak.1) == (kind.key, problem.code()) {
                    (kind.key, problem.code(), streak.2 + 1)
                } else {
                    (kind.key, problem.code(), 1)
                };
                if streak.2 >= limits.breaker {
                    warn!(
                        source = SOURCE,
                        event = "fss_kind_stopped",
                        kind = kind.key,
                        reason = streak.1,
                        "an endpoint failed in every cell asked; its other cells are not read this run"
                    );
                    kind_stops.insert(
                        kind.key,
                        format!(
                            "{} cells of {} in a row failed with {}",
                            streak.2, kind.key, streak.1
                        ),
                    );
                }
            }
            _ => streak = ("", "", 0),
        }
        reads.push(read);
    }
    if stop.is_none() {
        for (index, (kind, sector)) in cells.iter().enumerate() {
            if kind_stops.contains_key(kind.key)
                || !reads[index]
                    .problem
                    .as_ref()
                    .is_some_and(CellError::retryable)
            {
                continue;
            }
            if Instant::now() >= run.deadline {
                break;
            }
            let again = read_cell(kind, sector, &mut fetch, &mut run).await;
            info!(
                source = SOURCE,
                event = "fss_cell_reread",
                kind = kind.key,
                sector = sector.code,
                settled = again.problem.is_none(),
                "cell read again at the end of the run"
            );
            if again.problem.is_none() || again.seen > reads[index].seen {
                reads[index] = again;
            }
        }
    }

    let answered = reads
        .iter()
        .filter(|read| read.problem.is_none() || read.seen > 0)
        .count();
    if answered == 0 {
        anyhow::bail!(
            "{SOURCE}: no cell answered; first: {}",
            reads
                .iter()
                .find_map(|read| read.problem.as_ref())
                .map_or_else(|| "none".to_string(), ToString::to_string)
        );
    }
    let mut fetched = SourceFetch::default();
    for ((kind, sector), mut read) in cells.iter().zip(reads) {
        let state = match &read.problem {
            None if read.seen == 0 => PartState::Empty,
            None => PartState::Settled,
            Some(problem) => PartState::Unsettled(Unsettled {
                code: problem.code(),
                detail: format!("{} ({} × {})", problem.detail(), kind.key, sector.name),
            }),
        };
        match &read.problem {
            None => info!(
                source = SOURCE,
                event = "fss_cell_read",
                kind = kind.key,
                sector = sector.code,
                seen = read.seen,
                kept = read.records.len(),
                set_aside = read.set_aside,
                "cell read"
            ),
            Some(problem) => warn!(
                source = SOURCE,
                event = "fss_cell_unsettled",
                kind = kind.key,
                sector = sector.code,
                reason = problem.code(),
                detail = %problem.detail(),
                seen = read.seen,
                kept = read.records.len(),
                "cell not settled; its stored products are left as they are"
            ),
        }
        fetched.parts.push(PartOutcome {
            scope: PartScope::Prefix(kind.cell_prefix(sector)),
            state,
            seen: read.seen,
            kept: read.records.len(),
            set_aside: read.set_aside,
        });
        fetched.upstream_seen += read.seen;
        fetched.records.append(&mut read.records);
        fetched.filtered_out.append(&mut read.filtered_out);
        fetched.set_aside_ids.append(&mut read.set_aside_ids);
    }
    info!(
        source = SOURCE,
        requests = run.requests,
        upstream_seen = fetched.upstream_seen,
        kept = fetched.records.len(),
        stopped = stop.as_deref().unwrap_or(""),
        "fetch complete"
    );
    Ok(fetched)
}

/// An empty page is the end of the list only when the provider's own total
/// was already delivered. With products still owed (`total_count` above
/// what the earlier pages held) it is a short read, and accepting it would
/// settle a cell that collected nothing.
fn check_empty_page(page: u32, seen: usize, reported_total: Option<u64>) -> Result<(), CellError> {
    match reported_total.filter(|total| *total > seen as u64) {
        Some(total) => {
            warn!(
                source = SOURCE,
                page, seen, total, "empty page with products still owed"
            );
            Err(CellError::ShortRead {
                delivered: seen,
                total,
            })
        }
        None => Ok(()),
    }
}

/// A count given as a JSON number or a numeric string.
fn lenient_count(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

// ── Youth decision ────────────────────────────────────────────────────────────

/// What made a product a youth product (stored as `_fss_youth_basis`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YouthBasis {
    /// Its 가입대상 limits who may join to a youth age band.
    Band,
    /// A youth term or age span in its name or 가입대상 ("청년", "대학생",
    /// "사회초년생"), as the shared classifier reads it.
    Term,
    /// A generation number in its name ("2030", "1934"): nothing restricts
    /// who may join.
    Name,
}

impl YouthBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Band => "band",
            Self::Term => "term",
            Self::Name => "name",
        }
    }
}

/// Collapse runs of whitespace (line breaks in provider names) to one space.
fn squash_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a product is for young people, and on what basis. The decision
/// is the shared audience classifier's (`mazimi_core::audience`): youth and
/// no other reserved group. This module only supplies what the classifier
/// cannot read itself: the age band of the 가입대상 eligibility clauses
/// ([`eligibility_age_range`]), those clauses without brackets and perk
/// clauses (so "만 34세 이하 우대금리" never admits a product open to all),
/// and a generation number in the name ([`generation_band`]). Preferential
/// conditions (`spcl_cnd`) and notes (`etc_note`) are never read.
pub(crate) fn youth_decision(kind: &ProductKind, base: &FssBaseProduct) -> Option<YouthBasis> {
    let name = squash_spaces(base.fin_prdt_nm.as_deref().unwrap_or(""));
    let eligibility = kind
        .eligibility_field
        .and_then(|field| match field {
            "join_member" => base.join_member.as_deref(),
            _ => base.extra.get(field).and_then(Value::as_str),
        })
        .unwrap_or("");
    let clauses = eligibility_clauses(eligibility).join(", ");
    let youth = |title: &str, target: &str, (min_age, max_age): (Option<i32>, Option<i32>)| {
        let audience = audience::classify(&AudienceText {
            source_type: SOURCE,
            title,
            target: (!target.is_empty()).then_some(target),
            min_age,
            max_age,
            ..AudienceText::default()
        });
        audience.youth
    };
    // Youth and reserved for another group as well ("만 19세~34세 장애인") is
    // not a youth product. `classify` drops the groups once it sees youth,
    // so they are read separately, from the name and the clauses, for all
    // three paths.
    if !audience::reserved_groups(&name, Some(&clauses)).is_empty() {
        return None;
    }
    let band = eligibility_age_range(eligibility);
    if band.1.is_some() && youth("", "", band) {
        return Some(YouthBasis::Band);
    }
    if youth(&name, &clauses, (None, None)) {
        return Some(YouthBasis::Term);
    }
    let generation = generation_band(&name);
    if generation.1.is_some() && youth("", "", generation) {
        return Some(YouthBasis::Name);
    }
    None
}

/// Whether a savings row is a youth product (tests of the 가입대상 reading).
#[cfg(test)]
pub(crate) fn is_youth_product(product: &FssBaseProduct) -> bool {
    youth_decision(&SAVING, product).is_some()
}

/// Words that make a clause about a rate bonus or perk, not about who may
/// join ("만 34세 이하 추가 우대", "만 29세 이하 우대금리").
const BENEFIT_CLAUSE_WORDS: &[&str] = &["우대", "금리", "추가", "혜택", "이자", "보너스"];

/// The eligibility clauses of a 가입대상 text: text in brackets is dropped
/// ("실명의 개인 (만 34세 이하 우대금리)"), the rest is split on `,` `·` `/`
/// `;` and line breaks, and a clause that names a rate bonus or perk, or is
/// one by its shape ([`perk_by_shape`]), is skipped.
pub(crate) fn eligibility_clauses(text: &str) -> Vec<String> {
    let mut main = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' | '（' | '[' | '［' | '【' | '<' | '〈' => depth += 1,
            ')' | '）' | ']' | '］' | '】' | '>' | '〉' => depth = depth.saturating_sub(1),
            _ if depth == 0 => main.push(c),
            _ => {}
        }
    }
    main.split([',', '，', '·', '/', ';', '\n', '•', '※'])
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
        .filter(|clause| {
            !BENEFIT_CLAUSE_WORDS
                .iter()
                .any(|word| clause.contains(word))
        })
        .filter(|clause| !perk_by_shape(clause))
        .map(str::to_string)
        .collect()
}

/// A clause that is a perk or a condition by its shape, not its words: it
/// holds a rate ("연 1%p 가산"), or it sets an age as a topic ("만 34세 이하는
/// …", "만 29세 이상은 …"), which states what applies to that age, not who
/// may join.
fn perk_by_shape(clause: &str) -> bool {
    if clause.contains('%') {
        return true;
    }
    let compact: String = clause.chars().filter(|c| !c.is_whitespace()).collect();
    compact.match_indices('세').any(|(at, _)| {
        let rest = &compact[at + '세'.len_utf8()..];
        let rest = ["이하", "이상", "미만", "초과", "까지"]
            .iter()
            .find_map(|bound| rest.strip_prefix(bound))
            .unwrap_or(rest);
        rest.starts_with('은') || rest.starts_with('는')
    })
}

/// The age band that defines who may join, from a 가입대상 text: the bands
/// of its [`eligibility_clauses`]. Two clauses with different bands are
/// ambiguous and yield no band: a product without a band is never shown as
/// age-limited.
pub(crate) fn eligibility_age_range(text: &str) -> (Option<i32>, Option<i32>) {
    let band = eligibility_age_bounds(text);
    (band.min, band.max)
}

/// [`eligibility_age_range`] with the tightest alternative bounds
/// ([`AgeBounds`]).
pub(crate) fn eligibility_age_bounds(text: &str) -> AgeBounds {
    let mut band: Option<AgeBounds> = None;
    for clause in eligibility_clauses(text) {
        let bounds = age_bounds(&clause);
        if (bounds.min, bounds.max) == (None, None) {
            continue;
        }
        match band {
            None => band = Some(bounds),
            Some(previous) if (previous.min, previous.max) == (bounds.min, bounds.max) => {}
            Some(_) => return AgeBounds::default(),
        }
    }
    band.unwrap_or_default()
}

/// The id of a product: its cell prefix, company and product code.
fn product_id(kind: &ProductKind, sector: &Sector, base: &FssBaseProduct) -> String {
    format!(
        "{}{}_{}",
        kind.cell_prefix(sector),
        base.fin_co_no.as_deref().unwrap_or("unknown"),
        base.fin_prdt_cd
            .as_deref()
            .map(str::trim)
            .unwrap_or("unknown"),
    )
}

/// The raw record of one youth product: the provider's base row as given,
/// its option rows, and the kind, sector and youth-basis tags normalization
/// reads.
pub(crate) fn product_record(
    kind: &ProductKind,
    sector: &Sector,
    product: &Product,
    basis: YouthBasis,
) -> RawRecord {
    let source_id = product_id(kind, sector, &product.base);
    let mut payload = serde_json::to_value(&product.base).unwrap_or(Value::Null);
    if let Value::Object(ref mut map) = payload {
        for (key, value) in [
            ("_fss_kind", Value::from(kind.tag)),
            ("_source_prefix", Value::from(kind.source_prefix())),
            ("_fss_sector", Value::from(sector.name)),
            ("_fss_sector_code", Value::from(sector.code)),
            ("_fss_youth_basis", Value::from(basis.as_str())),
            ("_fss_options", Value::Array(product.options.clone())),
        ] {
            map.insert(key.to_string(), value);
        }
    }
    let content_hash = content_hash(&payload);
    RawRecord {
        source_id,
        payload,
        content_hash,
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;

    #[test]
    fn total_count_is_read_as_number_or_numeric_string() {
        assert_eq!(lenient_count(&serde_json::json!(39)), Some(39));
        assert_eq!(lenient_count(&serde_json::json!("59")), Some(59));
        assert_eq!(lenient_count(&Value::Null), None);
    }

    #[test]
    fn an_empty_page_with_products_still_owed_is_a_short_read() {
        // Provider says 59 products, page 1 is empty: not an empty success.
        assert_eq!(
            check_empty_page(1, 0, Some(59)).unwrap_err().code(),
            "short_read"
        );
        // A later empty page after the total was delivered ends the list.
        assert!(check_empty_page(2, 59, Some(59)).is_ok());
        // A provider that reports zero products, or no total at all, may
        // serve an empty page; the pipeline then sees zero upstream records.
        assert!(check_empty_page(1, 0, Some(0)).is_ok());
        assert!(check_empty_page(1, 0, None).is_ok());
    }

    #[test]
    fn err_cd_classes_follow_the_provider_table() {
        for code in ["010", "011", "012", "013", "020", "021"] {
            assert_eq!(rejection(code).code(), "source_rejected", "{code}");
        }
        for code in ["100", "101"] {
            assert_eq!(rejection(code).code(), "request_rejected", "{code}");
        }
        for code in ["900", "999", "<b>x</b>"] {
            assert_eq!(rejection(code).code(), "upstream_rejected", "{code}");
        }
        assert_eq!(rejection("<b>x</b>").detail(), "err_cd bxb");
    }
}

/// The base row of a product. The typed fields are the ones this module
/// reads; every other field the provider sends is kept in `extra`, so the
/// stored payload is the provider's row as given.
#[derive(Debug, Deserialize, serde::Serialize)]
pub struct FssBaseProduct {
    /// 금융회사 코드
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fin_co_no: Option<String>,
    /// 금융회사명
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kor_co_nm: Option<String>,
    /// 금융상품 코드
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fin_prdt_cd: Option<String>,
    /// 금융상품명
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fin_prdt_nm: Option<String>,
    /// 가입 방법
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_way: Option<String>,
    /// 기타 유의사항 (예금·적금)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etc_note: Option<String>,
    /// 대출한도 (주택담보·전월세대출)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loan_lmt: Option<String>,
    /// 공시 시작일
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dcls_strt_day: Option<String>,
    /// 공시 종료일
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dcls_end_day: Option<String>,
    /// 가입 대상 (예금·적금; 대출에는 없다)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_member: Option<String>,
    /// Every other field of the row.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// ── Normalization helper (called from pipeline.rs) ────────────────────────────

use crate::pipeline::NormalizedProgram;

/// The kind of a stored payload: by `_source_prefix`, else by the label
/// (`_fss_kind`) of payloads written before the prefix was stored.
fn kind_of(payload: &Value) -> Option<&'static ProductKind> {
    let prefix = payload["_source_prefix"].as_str();
    let tag = payload["_fss_kind"].as_str();
    KINDS
        .iter()
        .copied()
        .find(|kind| prefix.is_some_and(|p| p.strip_prefix("fss_") == Some(kind.id_stem)))
        .or_else(|| KINDS.iter().copied().find(|kind| tag == Some(kind.tag)))
}

fn text_field(payload: &Value, field: &str) -> Option<String> {
    payload[field]
        .as_str()
        .map(squash_spaces)
        .filter(|s| !s.is_empty())
}

/// 가입대상, else 기타 유의사항 (예금·적금).
fn eligibility_line(payload: &Value) -> Vec<String> {
    text_field(payload, "join_member")
        .or_else(|| text_field(payload, "etc_note"))
        .into_iter()
        .collect()
}

fn loan_limit_line(payload: &Value) -> Vec<String> {
    text_field(payload, "loan_lmt")
        .map(|limit| format!("대출한도 {limit}"))
        .into_iter()
        .collect()
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// (lowest, highest) of `field` over the option rows.
fn bounds(options: &[Value], field: &str) -> Option<(f64, f64)> {
    options
        .iter()
        .filter_map(|option| number(&option[field]))
        .fold(None, |acc, x| match acc {
            None => Some((x, x)),
            Some((lo, hi)) => Some((lo.min(x), hi.max(x))),
        })
}

fn span((lo, hi): (f64, f64)) -> String {
    if lo == hi {
        format!("{lo}")
    } else {
        format!("{lo}~{hi}")
    }
}

/// Distinct values of a text field over `options`, in the rows' order,
/// joined with "/".
fn distinct<'a>(options: impl Iterator<Item = &'a Value>, field: &str) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    for value in options.filter_map(|option| match &option[field] {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }) {
        if !value.is_empty() && !seen.contains(&value) {
            seen.push(value);
        }
    }
    (!seen.is_empty()).then(|| seen.join("/"))
}

/// "기본금리 연 3~3.5%, 최고우대금리 연 7% (12개월)": `intr_rate` is the base
/// rate, `intr_rate2` the rate with every preferential condition, and the
/// term named is the one (`save_trm`, months) the highest rate belongs to.
fn deposit_rates(options: &[Value]) -> Option<String> {
    let base = bounds(options, "intr_rate")?;
    let mut line = format!("기본금리 연 {}%", span(base));
    if let Some((_, top)) = bounds(options, "intr_rate2") {
        line.push_str(&format!(", 최고우대금리 연 {top}%"));
        let at_top = options
            .iter()
            .filter(|option| number(&option["intr_rate2"]) == Some(top));
        if let Some(terms) = distinct(at_top, "save_trm") {
            line.push_str(&format!(" ({terms}개월)"));
        }
    } else if let Some(terms) = bounds(options, "save_trm") {
        line.push_str(&format!(" ({}개월)", span(terms)));
    }
    Some(line)
}

/// "금리 연 4.16~4.36% (변동금리, 만기일시상환방식)".
fn loan_rates(options: &[Value]) -> Option<String> {
    let (lo, _) = bounds(options, "lend_rate_min")?;
    let (_, hi) = bounds(options, "lend_rate_max")?;
    let mut line = format!("금리 연 {}%", span((lo, hi)));
    let kinds: Vec<String> = ["lend_rate_type_nm", "rpay_type_nm"]
        .iter()
        .filter_map(|field| distinct(options.iter(), field))
        .collect();
    if !kinds.is_empty() {
        line.push_str(&format!(" ({})", kinds.join(", ")));
    }
    Some(line)
}

/// Today's Korean calendar date.
fn kst_today() -> chrono::NaiveDate {
    (chrono::Utc::now() + chrono::Duration::hours(9)).date_naive()
}

/// Normalize a raw FSS financial product payload into a NormalizedProgram.
pub fn normalize_financial(payload: &Value) -> NormalizedProgram {
    normalize_financial_on(payload, kst_today())
}

/// [`normalize_financial`] as of `today` (KST). Every text comes from the
/// provider's own fields through a fixed template; a field the provider
/// does not give stays empty. A disclosure period (`dcls_strt_day`,
/// `dcls_end_day`) is not an application window: no product gets one, a
/// disclosure whose end day has passed is closed, and an implausible year
/// ("99991231") is no date. Both days stay in the raw payload.
pub(crate) fn normalize_financial_on(
    payload: &Value,
    today: chrono::NaiveDate,
) -> NormalizedProgram {
    let kind = kind_of(payload);
    let kind_tag = kind
        .map(|kind| kind.tag)
        .or_else(|| payload["_fss_kind"].as_str())
        .unwrap_or("금융상품");

    // The institution name the API gives ("애큐온저축은행", "한화생명보험주식회사")
    // tells the sector apart.
    let institution = text_field(payload, "kor_co_nm").unwrap_or_default();
    let product_name = text_field(payload, "fin_prdt_nm").unwrap_or_else(|| "Unknown".into());

    // Title: "은행명 - 상품명 (종류)"
    let title = if institution.is_empty() {
        format!("{} ({})", product_name, kind_tag)
    } else {
        format!("{} - {} ({})", institution, product_name, kind_tag)
    };

    // Summary: the kind's own lines (가입대상 or 대출한도), then rates.
    let options = payload["_fss_options"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let parts: Vec<String> = match kind.map(|kind| kind.presentation) {
        Some(Presentation::Savings) => eligibility_line(payload)
            .into_iter()
            .chain(deposit_rates(options))
            .collect(),
        Some(Presentation::Loan) => loan_limit_line(payload)
            .into_iter()
            .chain(loan_rates(options))
            .collect(),
        None => eligibility_line(payload),
    };
    let summary = (!parts.is_empty()).then(|| parts.join(" · "));
    let disclosure_over =
        parse_fss_date(payload["dcls_end_day"].as_str()).is_some_and(|end| end < today);

    let provider_name = (!institution.is_empty()).then(|| institution.clone());

    // FSS product page URL is not returned by the API; use the portal URL.
    let official_url: Option<String> = Some("https://finlife.fss.or.kr/main/main.do".to_string());

    // 연령은 가입대상(join_member)의 가입 자격 문구에서만 읽는다
    // ([`eligibility_age_range`]). 괄호 안 단서, 우대·금리 조항, 기타
    // 유의사항(etc_note)의 나이는 가입 자격이 아니므로 읽지 않는다. 가입대상이
    // 없는 대출은 나이 제한 없음 (과파싱 금지 — 추측성 나이 채우기 안 함).
    let bounds = match kind {
        Some(kind) if kind.eligibility_field.is_none() => AgeBounds::default(),
        _ => eligibility_age_bounds(payload["join_member"].as_str().unwrap_or("")),
    };
    let (min_age, max_age) = (bounds.min, bounds.max);
    // Alternative bounds: the band is their union, the tightest a named
    // check (review M8).
    let source_rule_conditions: Vec<serde_json::Value> = bounds
        .tightest_max
        .map(crate::eligibility_codes::stated_upper_age_label)
        .into_iter()
        .chain(
            bounds
                .tightest_min
                .map(crate::eligibility_codes::stated_lower_age_label),
        )
        .collect();

    NormalizedProgram {
        program_type: "financial_product".to_string(),
        title,
        summary,
        provider_name,
        company_name: (!institution.is_empty()).then_some(institution),
        benefit_category: Some("finance".to_string()),
        provider_kind: Some("corporate".to_string()),
        official_url,
        application_start_at: None,
        application_end_at: None,
        min_age,
        max_age,
        regions: vec![],
        application_method: crate::pipeline::source_text(payload, "join_way"),
        submission_documents: None,
        screening_method: None,
        source_status: disclosure_over.then_some("closed"),
        source_rule_conditions,
        ..Default::default()
    }
}

// ── Age-range text parsing ─────────────────────────────────────────────────
//
// finlife 가입대상(join_member)/기타유의사항(etc_note) 필드는 자유 텍스트라
// "만 19~34세", "만19세~34세", "19세이상 34세이하", "만 34세 이하" 같은
// 변형이 섞여 나온다. `crates/worker/src/sources/youth_center.rs`와
// `pipeline.rs`는 온통청년 API가 이미 구조화된 숫자 필드(sprtTrgtMinAge/
// sprtTrgtMaxAge)로 내려주기 때문에 텍스트 파싱 로직이 없다 (Search Before
// Building 확인 완료 — 재사용할 기존 정규식/파서가 없어 새로 작성).

/// 두 숫자를 잇는 나이 범위 구분자 문자 (물결표/하이픈/대시 변형 포함).
const AGE_RANGE_SEPARATORS: [char; 5] = ['~', '-', '∼', '–', '—'];

/// 자유 텍스트에서 한국어 나이 표현을 파싱해 (min_age, max_age)를 뽑아낸다.
///
/// 인식하는 표현 (공백 유무 무관):
///   - "만 19~34세", "19-34세"        → (Some(19), Some(34))
///   - "만19세~34세", "만19세~만34세" → (Some(19), Some(34))
///   - "19세이상 34세이하"            → (Some(19), Some(34))
///   - "만 34세 이하"                 → (None, Some(34))
///   - "만 19세 이상"                 → (Some(19), None)
///   - "19세부터 34세까지"            → (Some(19), Some(34))
///
/// 앵커링 규칙: 숫자는 (공백 제거 후) 바로 뒤에 한국어 나이 단위 "세"가
/// 붙어 있을 때만 나이 후보로 인정한다. 단, "<숫자><구분자><숫자>세"
/// 범위 표현에서는 앞쪽 숫자는 "세"가 없어도 뒤쪽 숫자가 "세"로 앵커링되면
/// 함께 인정한다. 이 규칙 덕분에 같은 문자열에 섞여 있는 무관한 숫자
/// (대출한도, 예치금액, 개월수, 전화번호 등)를 나이로 오인하지 않는다.
/// 확신할 수 있는 표현을 찾지 못하면 항상 (None, None)을 반환한다 —
/// 추측성 파싱 금지.
#[cfg(test)]
pub(crate) fn parse_age_range_from_text(text: &str) -> (Option<i32>, Option<i32>) {
    let bounds = age_bounds(text);
    (bounds.min, bounds.max)
}

/// A clause's age band ([`parse_age_range_from_text`]) and, when it states
/// alternative bounds ("만 34세 이하 청년 또는 만 39세 이하 재직자"), the
/// tightest of them: the band admits their union, and the tightest bound is
/// a named check, so a 37-year-old who is not 재직자 is never confirmed
/// (review M8) and never hidden.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AgeBounds {
    pub min: Option<i32>,
    pub max: Option<i32>,
    /// The lowest of several distinct ceilings.
    pub tightest_max: Option<i32>,
    /// The highest of several distinct floors.
    pub tightest_min: Option<i32>,
}

fn age_bounds(text: &str) -> AgeBounds {
    let (min, max, ceilings, floors) = age_bounds_read(text);
    let tightest = |values: &[i32], pick: fn(i32, i32) -> i32| {
        let first = *values.first()?;
        values
            .iter()
            .any(|v| *v != first)
            .then(|| values.iter().copied().fold(first, pick))
    };
    AgeBounds {
        min,
        max,
        tightest_max: tightest(&ceilings, i32::min),
        tightest_min: tightest(&floors, i32::max),
    }
}

/// (min, max, every stated ceiling, every stated floor).
fn age_bounds_read(text: &str) -> (Option<i32>, Option<i32>, Vec<i32>, Vec<i32>) {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();

    // 최소 전제조건: 위에서 인식하는 모든 표현은 "세"를 포함한다.
    // 없으면 파싱을 시도할 필요조차 없다 (예: "1인당 최대 3천만원 이하").
    if !compact.contains('세') {
        return (None, None, Vec::new(), Vec::new());
    }

    let chars: Vec<char> = compact.chars().collect();
    let len = chars.len();

    // 나이 후보 숫자 런: (시작, 끝(제외), 값). 사람 나이로 말이 안 되는
    // 값(연도 "2024" 등)은 애초에 후보 목록에서 제외한다.
    let mut numbers: Vec<(usize, usize, i32)> = Vec::new();
    let mut i = 0;
    while i < len {
        if chars[i].is_ascii_digit() {
            let start = i;
            let mut j = i;
            while j < len && chars[j].is_ascii_digit() {
                j += 1;
            }
            let value: String = chars[start..j].iter().collect();
            if let Ok(v) = value.parse::<i32>() {
                if (0..=120).contains(&v) {
                    numbers.push((start, j, v));
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }

    if numbers.is_empty() {
        return (None, None, Vec::new(), Vec::new());
    }

    let mut ceilings: Vec<i32> = Vec::new();
    let mut floors: Vec<i32> = Vec::new();
    let is_anchored = |end: usize| -> bool { chars.get(end) == Some(&'세') };

    let starts_with_at = |pos: usize, needle: &str| -> bool {
        let needle_chars: Vec<char> = needle.chars().collect();
        if pos + needle_chars.len() > len {
            return false;
        }
        chars[pos..pos + needle_chars.len()] == needle_chars[..]
    };

    let mut min_age: Option<i32> = None;
    let mut max_age: Option<i32> = None;
    let mut consumed = vec![false; numbers.len()];

    for idx in 0..numbers.len() {
        if consumed[idx] {
            continue;
        }
        let (_, end, val) = numbers[idx];
        let anchored = is_anchored(end);
        // "세"가 바로 붙어 있으면 그 다음 글자부터, 아니면 숫자 바로 다음
        // 글자부터 구분자/표지를 찾는다. "만19세~34세"처럼 범위 앞쪽
        // 숫자에도 "세"가 중복으로 붙는 변형을 지원하려면, 앵커링됐다고
        // 해서 무조건 이상/이하 표지로 확정 짓지 않고 실패 시 구분자
        // 확인으로 계속 진행해야 한다.
        let cursor = if anchored { end + 1 } else { end };

        // Alternatives in one clause ("만 34세 이하 청년 또는 만 39세 이하
        // 재직자") admit their union: the lowest floor, the highest ceiling
        // (audit L4).
        if anchored {
            if starts_with_at(cursor, "이상") || starts_with_at(cursor, "부터") {
                min_age = Some(min_age.map_or(val, |m| m.min(val)));
                floors.push(val);
                continue;
            } else if starts_with_at(cursor, "이하") || starts_with_at(cursor, "까지") {
                max_age = Some(max_age.map_or(val, |m| m.max(val)));
                ceilings.push(val);
                continue;
            }
        }

        // "<숫자><구분자><숫자>세" 범위 표현 확인 (앞쪽 숫자가 "세"로
        // 앵커링됐든 안 됐든 동일하게 검사).
        if let Some(sep) = chars.get(cursor) {
            if AGE_RANGE_SEPARATORS.contains(sep) {
                // "만19세~만34세": the 만 prefix may repeat after the
                // separator.
                let next_start = if chars.get(cursor + 1) == Some(&'만') {
                    cursor + 2
                } else {
                    cursor + 1
                };
                if let Some(&(n_start, n_end, n_val)) = numbers.get(idx + 1) {
                    if n_start == next_start && is_anchored(n_end) {
                        min_age = Some(val);
                        max_age = Some(n_val);
                        floors.push(val);
                        ceilings.push(n_val);
                        consumed[idx + 1] = true;
                    }
                }
            }
        }
    }

    (min_age, max_age, ceilings, floors)
}

/// A generation number in a product name read as an age band: "2030" (the
/// 20s and 30s) → 20–39, "1934" (만19~34세) → 19–34. Only a 4-digit number
/// standing alone counts, and not one followed by 년/일/차 or date
/// punctuation, or preceded by 제. A decade pair (both halves multiples of
/// ten, the second ten above the first) is a generation; otherwise the
/// halves are read as lower and upper age when the upper one is a youth
/// band's (24, 29, 34, 39) and the number does not read as a year
/// (1950–2099). Two different readings in one name yield none. A year used
/// as a name ("비전2030", "탄소중립 2030") reads the same as "페퍼스2030" and
/// is admitted; a `name` basis states no eligibility, so no age band is
/// stored for it.
pub(crate) fn generation_band(name: &str) -> (Option<i32>, Option<i32>) {
    let chars: Vec<char> = name.chars().collect();
    let mut found: Option<(i32, i32)> = None;
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i - start != 4 {
            continue;
        }
        // A year ("2030년"), a date ("2030.12.31", "2030-12") or an issue
        // number ("제2030차") is not a generation.
        let after = chars.get(i).copied();
        let dated = matches!(after, Some('년' | '일' | '차'))
            || (matches!(after, Some('.' | '-' | '/'))
                && chars.get(i + 1).is_some_and(char::is_ascii_digit))
            || (start > 0 && chars[start - 1] == '제');
        if dated {
            continue;
        }
        let value: i32 = chars[start..i]
            .iter()
            .collect::<String>()
            .parse()
            .unwrap_or(0);
        let (lo, hi) = (value / 100, value % 100);
        let band = if lo % 10 == 0 && hi == lo + 10 && (10..=40).contains(&lo) {
            Some((lo, hi + 9))
        } else if !(1950..=2099).contains(&value)
            && (14..=29).contains(&lo)
            && [24, 29, 34, 39].contains(&hi)
        {
            // "1934": 만19~34세, upper bounds as youth bands write them.
            Some((lo, hi))
        } else {
            None
        };
        match (found, band) {
            (_, None) => {}
            (None, Some(band)) => found = Some(band),
            (Some(previous), Some(band)) if previous == band => {}
            (Some(_), Some(_)) => return (None, None),
        }
    }
    found.map_or((None, None), |(lo, hi)| (Some(lo), Some(hi)))
}

/// Parse FSS date strings into a calendar date. Accepts "YYYYMMDD" (8 ASCII
/// digits, so byte slicing is safe) or "YYYY.MM.DD", years 2000 to 2100.
fn parse_fss_date(s: Option<&str>) -> Option<chrono::NaiveDate> {
    let s = s?.trim();
    if s.is_empty() {
        return None;
    }

    // "YYYYMMDD"
    if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = s[0..4].parse().ok()?;
        let month: u32 = s[4..6].parse().ok()?;
        let day: u32 = s[6..8].parse().ok()?;
        // "99991231" and other sentinels are no date.
        if !(2000..=2100).contains(&year) {
            return None;
        }
        return chrono::NaiveDate::from_ymd_opt(year, month, day);
    }

    // "YYYY.MM.DD" or "YYYY-MM-DD"
    let parts: Vec<u32> = s
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse().ok())
        .collect();

    match parts.as_slice() {
        [y, m, d] if *y >= 2000 && *y <= 2100 => chrono::NaiveDate::from_ymd_opt(*y as i32, *m, *d),
        _ => None,
    }
}

#[cfg(test)]
mod date_tests {
    use super::*;

    fn on(payload: Value, today: &str) -> NormalizedProgram {
        normalize_financial_on(&payload, parse_fss_date(Some(today)).unwrap())
    }

    /// A disclosure period is the window the institution's filing covers,
    /// not an application window: 광주은행 files its rent loan monthly
    /// (`dcls_end_day` 20261130), and the loan is on continuous offer.
    #[test]
    fn a_disclosure_period_is_not_an_application_window() {
        let rent = serde_json::json!({
            "_source_prefix": "fss_rent_loan", "fin_prdt_nm": "청년맞춤형 전월세보증금 대출",
            "kor_co_nm": "광주은행", "dcls_strt_day": "20261020", "dcls_end_day": "20261130"
        });
        let open = on(rent.clone(), "20261111");
        assert_eq!(
            (open.application_start_at, open.application_end_at),
            (None, None)
        );
        assert_eq!(open.source_status, None);
        // Past its end day the disclosure is over: the row is closed.
        assert_eq!(on(rent, "20261201").source_status, Some("closed"));
        // A sentinel end day is no date: never a deadline, never closed.
        let sentinel = serde_json::json!({
            "_source_prefix": "fss_saving", "fin_prdt_nm": "청년 적금", "dcls_end_day": "99991231"
        });
        let n = on(sentinel, "20261111");
        assert_eq!((n.application_end_at, n.source_status), (None, None));
        assert_eq!(parse_fss_date(Some("99991231")), None);
        assert_eq!(parse_fss_date(Some("9999.12.31")), None);
        assert_eq!(parse_fss_date(Some("상시")), None);
        assert_eq!(
            parse_fss_date(Some("2026.12.31")),
            parse_fss_date(Some("20261231"))
        );
    }
}

#[cfg(test)]
mod age_range_tests {
    use super::*;

    /// Review M8: alternative bounds keep their union as the band and the
    /// tightest as a named check; one bound, or a range, adds no check.
    #[test]
    fn alternative_bounds_name_the_tightest_as_a_check() {
        let bounds = eligibility_age_bounds("만 34세 이하 청년 또는 만 39세 이하 재직자");
        assert_eq!((bounds.max, bounds.tightest_max), (Some(39), Some(34)));
        let floors = eligibility_age_bounds("만 20세 이상 또는 만 19세 이상 대학생");
        assert_eq!((floors.min, floors.tightest_min), (Some(19), Some(20)));
        for text in [
            "만 19~34세",
            "만 34세 이하",
            "만19세~만34세 개인 및 개인사업자",
        ] {
            let bounds = eligibility_age_bounds(text);
            assert_eq!(
                (bounds.tightest_min, bounds.tightest_max),
                (None, None),
                "{text}"
            );
        }
        let raw = serde_json::json!({"fin_prdt_nm": "청년 적금", "kor_co_nm": "A은행",
            "join_member": "만 34세 이하 청년 또는 만 39세 이하 재직자"});
        let norm = normalize_financial(&raw);
        assert_eq!(norm.max_age, Some(39));
        assert!(
            norm.source_rule_conditions
                .iter()
                .any(|c| c["field"] == "age"
                    && c["op"] == "lte"
                    && c["value"] == 34
                    && c["severity"] == "label"),
            "{:?}",
            norm.source_rule_conditions
        );
    }

    /// Audit L4: two alternative bounds in one clause are their union, in
    /// either order, never the one written last.
    #[test]
    fn alternative_bounds_in_one_clause_are_their_union() {
        for text in [
            "만 34세 이하 청년 또는 만 39세 이하 재직자",
            "만 39세 이하 재직자 또는 만 34세 이하 청년",
        ] {
            assert_eq!(parse_age_range_from_text(text), (None, Some(39)), "{text}");
        }
        assert_eq!(
            parse_age_range_from_text("만 20세 이상 또는 만 19세 이상 대학생"),
            (Some(19), None)
        );
    }

    // ── 정상 케이스 ──────────────────────────────────────────────────────

    #[test]
    fn tilde_range_with_man_prefix() {
        assert_eq!(
            parse_age_range_from_text("만 19~34세"),
            (Some(19), Some(34))
        );
    }

    #[test]
    fn ideal_and_iha_range_with_spacing() {
        assert_eq!(
            parse_age_range_from_text("19세이상 34세이하"),
            (Some(19), Some(34))
        );
    }

    #[test]
    fn max_only_with_man_prefix() {
        assert_eq!(parse_age_range_from_text("만 34세 이하"), (None, Some(34)));
    }

    #[test]
    fn min_only() {
        assert_eq!(parse_age_range_from_text("만 19세 이상"), (Some(19), None));
    }

    // ── 변형 케이스 ──────────────────────────────────────────────────────

    #[test]
    fn se_suffix_on_both_sides_of_tilde() {
        assert_eq!(
            parse_age_range_from_text("만19세~34세"),
            (Some(19), Some(34))
        );
    }

    #[test]
    fn man_prefix_repeated_after_the_separator() {
        // NH1934월복리적금's 가입대상 on finlife (2026-10-19).
        assert_eq!(
            parse_age_range_from_text("만19세~만34세 개인 및 개인사업자"),
            (Some(19), Some(34))
        );
        assert_eq!(
            parse_age_range_from_text("만 19세 ~ 만 34세"),
            (Some(19), Some(34))
        );
    }

    #[test]
    fn hyphen_separator_no_spaces() {
        assert_eq!(parse_age_range_from_text("19-34세"), (Some(19), Some(34)));
    }

    #[test]
    fn no_spaces_iha_ideal_combined() {
        assert_eq!(
            parse_age_range_from_text("만19세이상34세이하실명의개인"),
            (Some(19), Some(34))
        );
    }

    #[test]
    fn buteo_kkaji_range() {
        assert_eq!(
            parse_age_range_from_text("19세부터 34세까지"),
            (Some(19), Some(34))
        );
    }

    #[test]
    fn embedded_in_longer_eligibility_sentence() {
        assert_eq!(
            parse_age_range_from_text("만 19세 이상 34세 이하의 실명 개인 (1인 1계좌)"),
            (Some(19), Some(34))
        );
    }

    // ── 파싱 불가 케이스 (과파싱 금지 확인) ────────────────────────────────

    #[test]
    fn no_digits_returns_none() {
        assert_eq!(parse_age_range_from_text("실명의 개인"), (None, None));
    }

    #[test]
    fn explicit_no_restriction_returns_none() {
        assert_eq!(parse_age_range_from_text("제한없음"), (None, None));
    }

    #[test]
    fn bare_number_without_qualifier_returns_none() {
        // "세"로 앵커링은 되지만 이상/이하/부터/까지 표지가 전혀 없으면
        // 추측하지 않고 None을 유지해야 한다.
        assert_eq!(parse_age_range_from_text("만 20세 회원"), (None, None));
    }

    #[test]
    fn unrelated_amount_with_iha_but_no_se_returns_none() {
        // "이하"라는 단어가 있어도 나이 단위 "세"가 전혀 없으면 (대출한도
        // 등 무관한 숫자이므로) 파싱하지 않는다.
        assert_eq!(
            parse_age_range_from_text("1인당 최대 3천만원 이하"),
            (None, None)
        );
    }

    #[test]
    fn household_count_without_qualifier_not_misread_as_age() {
        // "100세대"처럼 "세"로 끝나는 무관한 단어가 있어도 이상/이하 표지가
        // 없으면 나이로 오인하지 않는다 (앵커링만으로는 값 확정 안 함).
        assert_eq!(
            parse_age_range_from_text("선착순 100세대 모집"),
            (None, None)
        );
    }

    #[test]
    fn unrelated_iha_on_non_age_unit_ignored_but_true_age_still_found() {
        // "12개월 이상"은 나이가 아니라 가입기간이므로 무시하고, 뒤에 나오는
        // "만 29세 이하"만 정확히 뽑아야 한다.
        assert_eq!(
            parse_age_range_from_text("가입기간 12개월 이상, 만 29세 이하 가입 가능"),
            (None, Some(29))
        );
    }

    #[test]
    fn a_generation_number_in_a_name_is_an_age_band() {
        assert_eq!(
            generation_band("응답하라2030정기적금"),
            (Some(20), Some(39))
        );
        assert_eq!(
            generation_band("페퍼룰루 2030적금(비대면)"),
            (Some(20), Some(39))
        );
        assert_eq!(generation_band("NH1934월복리적금"), (Some(19), Some(34)));
        // Years, codes and other numbers are not generations.
        for name in [
            "2026 특판 정기예금",
            "e-정기예금",
            "WR0001B",
            "12345적금",
            "3060 적금",
            "MZ 플랜적금",
            // A year, an issue number or a date, not a generation.
            "2030년 만기 특판",
            "제2030차 특판",
            "2030.12.31 만기",
            "2030-12 만기",
            "2030/12 만기",
            // Not a youth band: an upper bound outside the 24/29/34/39
            // convention.
            "1945 광복적금",
            "1544 안심적금",
            "1644 적금",
            "1730 적금",
            "1540 적금",
        ] {
            assert_eq!(generation_band(name), (None, None), "{name}");
        }
        assert_eq!(generation_band("2030 1934 적금"), (None, None));
    }

    #[test]
    fn empty_string_returns_none() {
        assert_eq!(parse_age_range_from_text(""), (None, None));
    }
}

#[cfg(test)]
mod youth_filter_tests {
    use super::*;

    fn product(name: &str, join_member: &str, spcl_cnd: &str) -> FssBaseProduct {
        serde_json::from_value(serde_json::json!({
            "fin_prdt_nm": name, "join_member": join_member, "spcl_cnd": spcl_cnd
        }))
        .unwrap()
    }

    #[test]
    fn only_the_eligibility_phrase_sets_the_age_band() {
        assert_eq!(
            eligibility_age_range("만19세~만34세 개인 및 개인사업자"),
            (Some(19), Some(34))
        );
        // Caveats and perks open to everyone: no band.
        for text in [
            "실명의 개인 (만 34세 이하 우대금리)",
            "실명의 개인(만 34세 이하 우대금리 적용)",
            "만 34세 이하 추가 우대",
            "실명의 개인, 만 34세 이하 추가 우대",
            "개인 · 만 29세 이하 고객 금리 0.5%p 추가",
            "개인 [만 34세 이하 가입 시 혜택]",
        ] {
            assert_eq!(eligibility_age_range(text), (None, None), "{text}");
        }
        // A clause holding a rate, or an age set as a topic ("이하는"),
        // is a perk by its shape, whatever its words.
        for text in [
            "실명의 개인, 만 34세 이하 청년은 연 1%p 가산",
            "실명의 개인. 만 34세 이하는 0.5%p 더 드립니다",
            "개인, 만 29세 이하는 별도 안내",
        ] {
            assert_eq!(eligibility_age_range(text), (None, None), "{text}");
            assert!(!is_youth_product(&product("자유적금", text, "")), "{text}");
        }
        // The eligibility phrase survives a caveat beside it.
        assert_eq!(
            eligibility_age_range("만19세~만34세 개인 (만 29세 이하 우대금리)"),
            (Some(19), Some(34))
        );
        assert_eq!(
            eligibility_age_range("만 19세 이상 만 34세 이하 개인, 만 29세 이하 추가 우대"),
            (Some(19), Some(34))
        );
        // Two eligibility bands disagree: ambiguous, excluded.
        assert_eq!(
            eligibility_age_range("만 34세 이하 개인, 만 39세 이하 개인사업자"),
            (None, None)
        );
    }

    #[test]
    fn a_caveat_neither_admits_nor_age_limits_a_product() {
        for join_member in [
            "실명의 개인 (만 34세 이하 우대금리)",
            "만 34세 이하 추가 우대",
            "개인 및 개인사업자, 만 34세 이하 추가 우대",
        ] {
            assert!(
                !is_youth_product(&product("자유적금", join_member, "")),
                "{join_member}"
            );
        }
        // A 청년-named product with a caveat stays in, but is not stored as
        // limited to 34.
        let normalized = normalize_financial(&serde_json::json!({
            "fin_prdt_nm": "청년 우대 적금", "join_member": "실명의 개인 (만 34세 이하 우대금리)",
            "etc_note": "만 34세 이하 가입 시 우대"
        }));
        assert_eq!((normalized.min_age, normalized.max_age), (None, None));
    }

    #[test]
    fn name_or_youth_age_bound_qualifies_but_a_preferential_condition_does_not() {
        assert!(is_youth_product(&product("청년 우대적금", "개인", "")));
        assert!(is_youth_product(&product(
            "NH1934월복리적금",
            "만19세~만34세 개인 및 개인사업자",
            ""
        )));
        assert!(is_youth_product(&product(
            "무엇이든 적금",
            "만 39세 이하 개인",
            ""
        )));
        // 청년 only in the preferential conditions: open to everyone.
        assert!(!is_youth_product(&product(
            "MZ 플랜적금",
            "개인 및 개인사업자",
            "청년도약계좌 보유 시 우대금리"
        )));
        // A children's product and an adult product are not youth products.
        assert!(!is_youth_product(&product(
            "토스뱅크 아이 적금",
            "토스뱅크 아이 통장을 보유한 15세 이하 실명의 개인",
            ""
        )));
        assert!(!is_youth_product(&product(
            "정기예금",
            "만 14세 이상의 실명의 개인",
            ""
        )));
        // The shared classifier's youth band tops out at 45 (지자체 청년 조례):
        // "만 45세 이하" is a youth bound, "만 46세 이하" is not.
        assert!(is_youth_product(&product(
            "무엇이든 적금",
            "만 45세 이하",
            ""
        )));
        assert!(!is_youth_product(&product("노후 적금", "만 46세 이하", "")));
        assert!(!is_youth_product(&product(
            "틴 적금",
            "만 19세 이하 개인",
            ""
        )));
        assert!(!is_youth_product(&product(
            "자유 적금",
            "실명의 개인(만 19세 이하는 법정대리인 동의)",
            ""
        )));
    }

    /// Youth, and reserved for another group as well, is not a youth
    /// product: the shared classifier's reserved groups (title, and a lead
    /// line that names only groups) are read before its youth reset. A list
    /// that also names people no group models ("기초생활수급자, 장애인, 청년")
    /// is open to them; 수급자 and 현역병 are no reserved group there.
    #[test]
    fn a_product_reserved_for_another_group_is_not_a_youth_product() {
        assert!(!is_youth_product(&product(
            "행복적금",
            "만 19세~34세 장애인",
            ""
        )));
        assert!(!is_youth_product(&product("장애인 청년 적금", "개인", "")));
        assert!(is_youth_product(&product(
            "청년 희망적금",
            "기초생활수급자, 장애인, 청년",
            ""
        )));
        assert!(is_youth_product(&product(
            "수급자 적금",
            "만 34세 이하 기초생활수급자",
            ""
        )));
        assert!(is_youth_product(&product(
            "군장병 적금",
            "만 19세~34세 현역병",
            ""
        )));
    }

    /// Every bank product finlife served on 2026-10-19 (132 = 39 deposits,
    /// 59 savings, 34 loans). The old filter ("청년" in the name or
    /// etc_note) kept none of them; NH1934월복리적금 is the youth product.
    #[test]
    fn live_bank_catalogue_keeps_exactly_the_youth_product() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/fss_bank_products_20261019.json"
        ))
        .unwrap();
        let products: Vec<FssBaseProduct> =
            serde_json::from_value(fixture["products"].clone()).unwrap();
        assert_eq!(products.len(), 132);
        let kept: Vec<&str> = products
            .iter()
            .filter(|p| is_youth_product(p))
            .map(|p| p.fin_prdt_nm.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(kept, ["NH1934월복리적금"]);
        let nh = products
            .iter()
            .find(|p| p.fin_prdt_nm.as_deref() == Some("NH1934월복리적금"))
            .unwrap();
        let normalized = normalize_financial(&serde_json::to_value(nh).unwrap());
        assert_eq!(
            (normalized.min_age, normalized.max_age),
            (Some(19), Some(34))
        );
        assert_eq!(youth_decision(&SAVING, nh), Some(YouthBasis::Band));
    }
}

#[cfg(test)]
#[path = "financial_cells_tests.rs"]
mod cells_tests;
