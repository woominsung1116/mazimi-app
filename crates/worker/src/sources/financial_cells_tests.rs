//! Fixture tests for the kind × sector collection.
//!
//! `tests/fixtures/fss_20261110/` holds finlife responses captured from the
//! production host on 2026-11-10 (`{endpoint}_{topFinGrpNo}_p{page}.json`,
//! public disclosure data, no key inside), untrimmed. Only page 1 of the
//! savings-bank deposits (394 products, 4 pages) was captured. The credit
//! loan captures are kept as the evidence for not reading that endpoint.

use super::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fss_20261110");

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{name}")).unwrap()
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn body(kind: &ProductKind, sector: &Sector, page: u32) -> Option<String> {
    let path = format!(
        "{FIXTURES}/{}_{}_p{page}.json",
        kind.endpoint.trim_end_matches(".json"),
        sector.code
    );
    std::fs::read_to_string(path).ok()
}

/// What an empty sector answers (measured: every endpoint × sector without
/// products).
fn empty_sector() -> String {
    fixture("depositProductsSearch_030200_p1.json")
}

/// The provider's web firewall answered `annuitySavingProductsSearch` with
/// an HTML page and HTTP 200. Synthetic stand-in: the real block page was
/// not captured.
const BLOCK_PAGE: &str = "<!DOCTYPE html><html><head><title>요청 차단</title></head>\
<body><h1>웹 방화벽에 의해 요청이 차단되었습니다.</h1></body></html>";

/// The capture day: every disclosure in the fixtures is current on it.
fn capture_day() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 11, 11).unwrap()
}

fn fast() -> Limits {
    Limits {
        pace: Duration::ZERO,
        budget: Duration::from_secs(60),
        breaker: 3,
        today: capture_day(),
    }
}

fn names_kept(kind: &ProductKind, file: &str) -> Vec<(String, YouthBasis)> {
    parse_page(kind, &fixture(file))
        .unwrap()
        .products
        .iter()
        .filter_map(|p| {
            youth_decision(kind, &p.base).map(|basis| (p.base.fin_prdt_nm.clone().unwrap(), basis))
        })
        .collect()
}

fn product_named<'a>(page: &'a Page, name: &str) -> &'a Product {
    page.products
        .iter()
        .find(|p| p.base.fin_prdt_nm.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no product named {name}"))
}

fn record_of(kind: &ProductKind, sector: &Sector, file: &str, name: &str) -> RawRecord {
    let page = parse_page(kind, &fixture(file)).unwrap();
    let product = product_named(&page, name);
    let basis = youth_decision(kind, &product.base).expect("a youth product");
    product_record(kind, sector, product, basis)
}

/// A transport serving the fixtures; a cell without any fixture answers as
/// an empty sector, a page that was not captured of a captured cell fails
/// (it must not pass for the end of the list), and `overrides` replace
/// whole cells.
fn transport(
    overrides: HashMap<(&'static str, &'static str), String>,
    log: Arc<Mutex<Vec<String>>>,
) -> impl FnMut(&'static ProductKind, &'static Sector, u32) -> std::future::Ready<Result<String>> {
    move |kind, sector, page| {
        log.lock()
            .unwrap()
            .push(format!("{}/{}/{page}", kind.key, sector.code));
        let answer = match overrides.get(&(kind.key, sector.code)) {
            Some(answer) => Ok(answer.clone()),
            None => match (body(kind, sector, page), body(kind, sector, 1)) {
                (Some(answer), _) => Ok(answer),
                (None, Some(_)) => Err(anyhow::anyhow!(
                    "fixture transport: page {page} was not captured"
                )),
                (None, None) => Ok(empty_sector()),
            },
        };
        std::future::ready(answer)
    }
}

fn part<'a>(fetch: &'a SourceFetch, prefix: &str) -> &'a PartOutcome {
    fetch
        .parts
        .iter()
        .find(|p| p.scope == PartScope::Prefix(prefix.to_string()))
        .unwrap_or_else(|| panic!("no part {prefix}"))
}

fn code(part: &PartOutcome) -> Option<&'static str> {
    match &part.state {
        PartState::Unsettled(u) => Some(u.code),
        _ => None,
    }
}

// ── Tables ──────────────────────────────────────────────────────────────────

#[test]
fn four_kinds_are_read_in_every_sector() {
    let keys: Vec<&str> = KINDS.iter().map(|k| k.key).collect();
    assert_eq!(
        keys,
        ["deposit", "saving", "mortgage_loan", "rent_house_loan"]
    );
    let codes: Vec<&str> = SECTORS.iter().map(|s| s.code).collect();
    assert_eq!(codes, ["020000", "030200", "030300", "050000", "060000"]);
}

/// The credit-loan endpoint is not read: every captured row is an
/// institution-level rate table of one of three types, not a product.
#[test]
fn credit_loan_rows_are_institution_rate_tables_not_products() {
    assert!(KINDS.iter().all(|k| !k.endpoint.starts_with("creditLoan")));
    let mut rows = 0;
    for file in [
        "creditLoanProductsSearch_020000_p1.json",
        "creditLoanProductsSearch_030200_p1.json",
    ] {
        for row in fixture_json(file)["result"]["baseList"].as_array().unwrap() {
            rows += 1;
            let kind = row["crdt_prdt_type_nm"].as_str().unwrap();
            assert!(
                ["일반신용대출", "마이너스한도대출", "장기카드대출(카드론)"].contains(&kind),
                "{kind}"
            );
            assert!(!row["fin_prdt_nm"].as_str().unwrap().contains("청년"));
        }
    }
    assert_eq!(rows, 73);
}

/// Lifecycle reconciliation protects an unread cell by its id prefix, so no
/// cell's prefix may begin another cell's ids.
#[test]
fn cell_prefixes_are_prefix_free() {
    let prefixes: Vec<String> = KINDS
        .iter()
        .flat_map(|k| SECTORS.iter().map(move |s| k.cell_prefix(s)))
        .collect();
    assert_eq!(prefixes.len(), 20);
    for (i, a) in prefixes.iter().enumerate() {
        for (j, b) in prefixes.iter().enumerate() {
            assert!(i == j || !b.starts_with(a.as_str()), "{a} prefixes {b}");
        }
    }
    assert_eq!(DEPOSIT.cell_prefix(&BANK), "fss_deposit_");
    assert_eq!(SAVING.cell_prefix(&SAVINGS_BANK), "fss_030300_saving_");
    assert_eq!(MORTGAGE_LOAN.cell_prefix(&BANK), "fss_loan_");
    assert_eq!(RENT_HOUSE_LOAN.cell_prefix(&BANK), "fss_rent_loan_");
}

// ── Ids ─────────────────────────────────────────────────────────────────────

/// The bank products collected before this change keep their ids, so no
/// stored programme (and no user's saved or applied row) is orphaned.
#[test]
fn bank_ids_of_the_three_original_kinds_are_unchanged() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/fss_bank_products_20261019.json"
    ))
    .unwrap();
    let id = |kind: &ProductKind, name: &str| {
        let row = fixture["products"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["fin_prdt_nm"] == name)
            .unwrap()
            .clone();
        product_id(kind, &BANK, &serde_json::from_value(row).unwrap())
    };
    assert_eq!(id(&DEPOSIT, "WON플러스예금"), "fss_deposit_0010001_WR0001B");
    assert_eq!(
        id(&SAVING, "우리SUPER주거래적금"),
        "fss_saving_0010001_WR0001F"
    );
    assert_eq!(
        id(&SAVING, "NH1934월복리적금"),
        "fss_saving_0013175_10-047-1365-0001"
    );
    assert_eq!(id(&MORTGAGE_LOAN, "우리아파트론"), "fss_loan_0010001_1054");
}

/// Across the captured sectors no company appears in two sectors, so the id
/// needs the sector only to keep each cell's ids under its own prefix.
#[test]
fn non_bank_ids_carry_the_sector() {
    let page = parse_page(
        &MORTGAGE_LOAN,
        &fixture("mortgageLoanProductsSearch_050000_p1.json"),
    )
    .unwrap();
    assert_eq!(page.products.len(), 20);
    assert_eq!(
        product_id(&MORTGAGE_LOAN, &INSURANCE, &page.products[0].base),
        "fss_050000_loan_0010593_302301"
    );
}

// ── Youth decision on the real captures ─────────────────────────────────────

/// Every youth product of the captured savings-bank savings (278 rows, all
/// three pages), with what made it one.
#[test]
fn savings_bank_savings_keep_exactly_the_youth_products() {
    let mut kept = Vec::new();
    for page in 1..=3 {
        kept.extend(names_kept(
            &SAVING,
            &format!("savingProductsSearch_030300_p{page}.json"),
        ));
    }
    let kept: Vec<(&str, &str)> = kept
        .iter()
        .map(|(name, basis)| (name.as_str(), basis.as_str()))
        .collect();
    assert_eq!(
        kept,
        [
            ("청년플랜적금", "band"),
            ("응답하라2030정기적금", "band"),
            ("페퍼룰루 2030적금(비대면)", "name"),
            ("페퍼스2030적금 (디지털페퍼앱)", "name"),
            ("청포도 청년적금", "band"),
        ]
    );
}

/// Named rows that must stay out, each with why.
#[test]
fn products_for_others_stay_out() {
    let mut rows: Vec<FssBaseProduct> = Vec::new();
    for page in 1..=3 {
        rows.extend(
            parse_page(
                &SAVING,
                &fixture(&format!("savingProductsSearch_030300_p{page}.json")),
            )
            .unwrap()
            .products
            .into_iter()
            .map(|p| p.base),
        );
    }
    let bank: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/fss_bank_products_20261019.json"
    ))
    .unwrap();
    rows.extend(
        bank["products"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| serde_json::from_value(row.clone()).unwrap()),
    );
    let out = [
        // 청년 or a youth band only in a preferential condition.
        ("마이홈 정기적금", "perk only: 청년층 우대금리"),
        ("NH고향사랑기부적금", "perk only: 만 19~34세 우대"),
        ("NH고향사랑기부예금", "perk only: 만 19~34세 우대"),
        ("NH올원e 미니적금", "perk only: MZ세대 우대"),
        // Children and teens.
        ("아이적금", "children: 19세 미만"),
        ("퍼스트유 정기적금", "children: 만 7세 이하"),
        ("꿈나무정기적금", "children: 만13세 미만"),
        ("꿈나무장학적금", "children: 1~19세"),
        ("장학적금자유식", "teens: 만19세 미만"),
        // Parents.
        ("아이행복적금", "parents of children"),
        ("자녀와 행복 PLUS 정기 적금", "parents of children"),
        ("아이사랑 정기적금", "parents of children"),
        ("스마트 아이플러스 정기적금", "parents of children"),
        // Seniors and other groups.
        ("KB착한누리적금", "vulnerable groups, 만65세이상"),
        (
            "희망 정기적금",
            "vulnerable groups; 보호종료 only in brackets",
        ),
        // Adults, and a brand word nothing restricts.
        ("e-정기적금", "adults: 만19세이상"),
        ("MZ 플랜적금", "brand word only: 개인 및 개인사업자"),
    ];
    for (name, why) in out {
        let matching: Vec<&FssBaseProduct> = rows
            .iter()
            .filter(|row| row.fin_prdt_nm.as_deref() == Some(name))
            .collect();
        assert!(!matching.is_empty(), "{name} is not in the captures");
        for row in matching {
            assert_eq!(youth_decision(&SAVING, row), None, "{name} ({why})");
        }
    }
}

/// Loan rows carry no 가입대상: only a youth term in the name admits one.
#[test]
fn loan_rows_are_read_by_name_only() {
    let kept = names_kept(
        &RENT_HOUSE_LOAN,
        "rentHouseLoanProductsSearch_020000_p1.json",
    );
    assert_eq!(
        kept,
        [("청년맞춤형 전월세보증금 대출".to_string(), YouthBasis::Term)]
    );
    assert!(names_kept(&MORTGAGE_LOAN, "mortgageLoanProductsSearch_050000_p1.json").is_empty());
    assert!(names_kept(&DEPOSIT, "depositProductsSearch_030300_p1.json").is_empty());
}

// ── Normalization ───────────────────────────────────────────────────────────

#[test]
fn a_bank_rent_loan_is_shown_as_a_loan() {
    let record = record_of(
        &RENT_HOUSE_LOAN,
        &BANK,
        "rentHouseLoanProductsSearch_020000_p1.json",
        "청년맞춤형 전월세보증금 대출",
    );
    assert_eq!(record.source_id, "fss_rent_loan_0010019_LN11010001072");
    assert_eq!(record.payload["_fss_sector"], "은행");
    assert_eq!(record.payload["_fss_youth_basis"], "term");
    let n = normalize_financial_on(&record.payload, parse_fss_date(Some("20261111")).unwrap());
    assert_eq!(
        n.title,
        "광주은행 - 청년맞춤형 전월세보증금 대출 (전월세대출)"
    );
    assert_eq!(
        n.summary.as_deref(),
        Some("대출한도 2억원 · 금리 연 4.16~4.36% (변동금리, 만기일시상환방식)")
    );
    assert_eq!(n.provider_name.as_deref(), Some("광주은행"));
    assert_eq!(n.program_type, "financial_product");
    assert_eq!(n.application_method.as_deref(), Some("영업점"));
    // No deadline from the disclosure period, no invented age band.
    assert_eq!((n.application_start_at, n.application_end_at), (None, None));
    assert_eq!((n.min_age, n.max_age), (None, None));
    // A loan never carries an age band, even if a field read like one.
    let mut payload = record.payload.clone();
    payload["join_member"] = serde_json::json!("만 19~34세");
    let n = normalize_financial(&payload);
    assert_eq!((n.min_age, n.max_age), (None, None));
}

#[test]
fn a_savings_bank_saving_names_its_institution_and_the_term_of_its_top_rate() {
    let file = "savingProductsSearch_030300_p1.json";
    let plan = record_of(&SAVING, &SAVINGS_BANK, file, "청년플랜적금");
    assert_eq!(plan.source_id, "fss_030300_saving_0010345_HK00104");
    assert_eq!(plan.payload["_fss_sector"], "저축은행");
    let n = normalize_financial(&plan.payload);
    assert_eq!(n.title, "애큐온저축은행 - 청년플랜적금 (적금)");
    assert_eq!(
        n.summary.as_deref(),
        Some("만 19세 이상 ~ 만 39세 이하 개인 · 기본금리 연 3~3.5%, 최고우대금리 연 7% (12개월)")
    );
    assert_eq!((n.min_age, n.max_age), (Some(19), Some(39)));

    // 4.5% exists only at 12 months (36 months tops out at 3.7%).
    let banded = normalize_financial(
        &record_of(&SAVING, &SAVINGS_BANK, file, "응답하라2030정기적금").payload,
    );
    assert_eq!((banded.min_age, banded.max_age), (Some(20), Some(39)));
    assert_eq!(
        banded.summary.as_deref(),
        Some("가입시점 만20세~만39세만 가입가능(비거주 외국인 제외) · 기본금리 연 1.5~3%, 최고우대금리 연 4.5% (12개월)")
    );

    // Admitted by its name: no age band is stored.
    let named = record_of(
        &SAVING,
        &SAVINGS_BANK,
        "savingProductsSearch_030300_p2.json",
        "페퍼룰루 2030적금(비대면)",
    );
    assert_eq!(named.payload["_fss_youth_basis"], "name");
    let n = normalize_financial(&named.payload);
    assert_eq!((n.min_age, n.max_age), (None, None));
}

#[test]
fn line_breaks_in_provider_names_do_not_reach_the_title() {
    let page = parse_page(
        &RENT_HOUSE_LOAN,
        &fixture("rentHouseLoanProductsSearch_020000_p1.json"),
    )
    .unwrap();
    let product = product_named(&page, "모바일전세자금대출\n(주택금융공사)");
    let record = product_record(&RENT_HOUSE_LOAN, &BANK, product, YouthBasis::Term);
    let n = normalize_financial(&record.payload);
    assert!(
        n.title.contains("모바일전세자금대출 (주택금융공사)"),
        "{}",
        n.title
    );
    assert!(!n.title.contains('\n'));
}

#[test]
fn insurance_sector_loans_normalize() {
    let page = parse_page(
        &MORTGAGE_LOAN,
        &fixture("mortgageLoanProductsSearch_050000_p1.json"),
    )
    .unwrap();
    let n = normalize_financial(
        &product_record(
            &MORTGAGE_LOAN,
            &INSURANCE,
            &page.products[0],
            YouthBasis::Term,
        )
        .payload,
    );
    assert_eq!(
        n.title,
        "한화생명보험주식회사 - 홈드림모기지론 (주택담보대출)"
    );
    assert_eq!(
        n.summary.as_deref(),
        Some("대출한도 감정가의 최고 70%까지 (담보물소재지, 대출금액, 고객신용, 소득 등에 따라 차등적용) · 금리 연 6.26~7.99% (고정금리/변동금리, 분할상환방식)")
    );
}

/// A product without option rows states no rate; nothing is filled in.
#[test]
fn a_product_without_rate_rows_states_no_rate() {
    let n = normalize_financial(&serde_json::json!({
        "_source_prefix": "fss_rent_loan", "_fss_kind": "전월세대출",
        "kor_co_nm": "은행", "fin_prdt_nm": "청년 전세", "_fss_options": []
    }));
    assert_eq!(n.summary, None);
}

// ── Page contract ───────────────────────────────────────────────────────────

#[test]
fn an_empty_sector_is_an_empty_page() {
    let page = parse_page(&DEPOSIT, &fixture("depositProductsSearch_030200_p1.json")).unwrap();
    assert_eq!((page.total, page.last_page, page.rows), (Some(0), 1, 0));
    assert!(check_empty_page(1, 0, page.total).is_ok());
}

/// Page 4 of the savings-bank savings (`max_page_no` 3) answers an empty
/// list with the same total: the end of the list once the total was
/// delivered, a short read otherwise.
#[test]
fn a_page_past_the_end_ends_the_list_only_after_the_total() {
    let page = parse_page(&SAVING, &fixture("savingProductsSearch_030300_p4.json")).unwrap();
    assert_eq!((page.total, page.rows), (Some(278), 0));
    assert!(check_empty_page(4, 278, page.total).is_ok());
    assert!(check_empty_page(4, 32, page.total).is_err());
}

#[test]
fn a_changed_envelope_or_field_shape_fails_the_cell() {
    let rent = fixture("rentHouseLoanProductsSearch_020000_p1.json");
    let cases = [
        (BLOCK_PAGE.to_string(), "non_json_body"),
        (String::new(), "non_json_body"),
        (
            r#"{"result":{"err_cd":"900","err_msg":"정의되지 않은 오류"}}"#.to_string(),
            "upstream_rejected",
        ),
        (
            r#"{"result":{"err_cd":"020","err_msg":"일일검색 허용횟수 초과"}}"#.to_string(),
            "source_rejected",
        ),
        (
            r#"{"result":{"err_cd":"101","err_msg":"부적절한 값"}}"#.to_string(),
            "request_rejected",
        ),
        (
            r#"{"result":{"err_cd":"000"}}"#.to_string(),
            "schema_changed",
        ),
        (r#"[1,2]"#.to_string(), "schema_changed"),
        // A base field the kind always carries was renamed on every row.
        (
            rent.replace("\"loan_lmt\"", "\"loan_limit\""),
            "schema_changed",
        ),
        // An option field the rates are read from was renamed everywhere.
        (
            rent.replace("\"lend_rate_min\"", "\"rate_min\""),
            "schema_changed",
        ),
    ];
    for (body, expected) in cases {
        let error = parse_page(&RENT_HOUSE_LOAN, &body).unwrap_err();
        assert_eq!(error.code(), expected, "{}", &body[..body.len().min(80)]);
    }
}

/// A defect in one row sets that row aside, counted; the rest is read.
#[test]
fn a_defective_row_is_set_aside_not_the_cell() {
    let mut rent = fixture_json("rentHouseLoanProductsSearch_020000_p1.json");
    let rows = rent["result"]["baseList"].as_array_mut().unwrap();
    rows[0]["fin_prdt_nm"] = serde_json::json!(" ");
    rows[1]["fin_co_no"] = serde_json::json!("00A1");
    rows[2]["loan_inci_expn"] = serde_json::json!("가".repeat(MAX_TEXT_CHARS + 1));
    rows[3].as_object_mut().unwrap().remove("loan_lmt");
    rows[4]["kor_co_nm"] = serde_json::json!({});
    rows[6]["fin_prdt_cd"] = serde_json::json!("X".repeat(61));
    rows[7]["fin_prdt_cd"] = serde_json::json!("A B");
    let flooded = (rows[5]["fin_co_no"].clone(), rows[5]["fin_prdt_cd"].clone());
    let options = rent["result"]["optionList"].as_array_mut().unwrap();
    let template = options[0].clone();
    for _ in 0..=MAX_OPTION_ROWS {
        let mut row = template.clone();
        row["fin_co_no"] = flooded.0.clone();
        row["fin_prdt_cd"] = flooded.1.clone();
        options.push(row);
    }
    let page = parse_page(&RENT_HOUSE_LOAN, &rent.to_string()).unwrap();
    assert_eq!(page.rows, 41);
    assert_eq!(page.set_aside, 8);
    assert_eq!(page.products.len(), 33);
    assert!(page
        .products
        .iter()
        .any(|p| p.base.fin_prdt_nm.as_deref() == Some("청년맞춤형 전월세보증금 대출")));
}

// ── Whole run ───────────────────────────────────────────────────────────────

/// One cell answers with the firewall's block page: every other cell is
/// read, including the 3-page savings-bank savings to its total, the run
/// reports which cell it could not settle, and reads it once more.
#[tokio::test]
async fn a_blocked_cell_leaves_the_other_cells_running() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let overrides = HashMap::from([(("deposit", "030300"), BLOCK_PAGE.to_string())]);
    let fetch = collect_cells(transport(overrides, log.clone()), fast())
        .await
        .unwrap();

    let names: Vec<&str> = fetch
        .records
        .iter()
        .map(|r| r.payload["fin_prdt_nm"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "청년플랜적금",
            "응답하라2030정기적금",
            "페퍼룰루 2030적금(비대면)",
            "페퍼스2030적금 (디지털페퍼앱)",
            "청포도 청년적금",
            "청년맞춤형 전월세보증금 대출",
        ]
    );
    let blocked = part(&fetch, "fss_030300_deposit_");
    assert_eq!(code(blocked), Some("non_json_body"));
    // A real multi-page cell settles.
    let savings = part(&fetch, "fss_030300_saving_");
    assert_eq!(savings.state, PartState::Settled);
    assert_eq!((savings.seen, savings.kept), (278, 5));
    assert_eq!(part(&fetch, "fss_rent_loan_").state, PartState::Settled);
    assert_eq!(part(&fetch, "fss_030200_deposit_").state, PartState::Empty);
    let counts =
        |state: fn(&PartState) -> bool| fetch.parts.iter().filter(|p| state(&p.state)).count();
    assert_eq!(counts(|s| *s == PartState::Settled), 3);
    assert_eq!(counts(|s| *s == PartState::Empty), 16);
    assert_eq!(counts(|s| matches!(s, PartState::Unsettled(_))), 1);
    assert_eq!(fetch.upstream_seen, 278 + 20 + 41);
    // The filtered-out rows are named, so a product that stops matching is
    // visible.
    assert_eq!(fetch.filtered_out.len(), 278 + 20 + 41 - 6);

    // Sequential: 20 cells, two more savings pages, one second read of the
    // blocked cell; never page 4 of the savings.
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 20 + 2 + 1, "{log:?}");
    assert_eq!(log.last().unwrap(), "deposit/030300/1");
    assert!(!log.contains(&"saving/030300/4".to_string()));
}

/// A whole endpoint answering the firewall's block page in every sector
/// (the provider's recorded failure style) stops only that kind: the other
/// kinds are all read and settled.
#[tokio::test]
async fn a_blocked_endpoint_stops_only_its_own_kind() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let overrides: HashMap<(&'static str, &'static str), String> = SECTORS
        .iter()
        .map(|sector| (("deposit", sector.code), BLOCK_PAGE.to_string()))
        .collect();
    let fetch = collect_cells(transport(overrides, log.clone()), fast())
        .await
        .unwrap();
    let deposit_codes: Vec<Option<&str>> = SECTORS
        .iter()
        .map(|sector| code(part(&fetch, &DEPOSIT.cell_prefix(sector))))
        .collect();
    assert_eq!(
        deposit_codes,
        [
            Some("non_json_body"),
            Some("non_json_body"),
            Some("non_json_body"),
            Some("not_attempted"),
            Some("not_attempted"),
        ]
    );
    for kind in [&SAVING, &MORTGAGE_LOAN, &RENT_HOUSE_LOAN] {
        for sector in SECTORS {
            let p = part(&fetch, &kind.cell_prefix(sector));
            assert!(
                matches!(p.state, PartState::Settled | PartState::Empty),
                "{} {}: {:?}",
                kind.key,
                sector.code,
                p.state
            );
        }
    }
    assert_eq!(fetch.records.len(), 6);
    // 3 deposit cells, then 15 cells of the other kinds and the two extra
    // savings pages; the tripped kind is not read again.
    assert_eq!(log.lock().unwrap().len(), 3 + 15 + 2);
}

/// A later page that answers "empty, total 0" disagrees with the total page
/// 1 gave: the cell read short, it is not an empty end of the list.
#[tokio::test]
async fn an_empty_later_page_that_contradicts_the_total_is_a_short_read() {
    let fetch = collect_cells(
        |kind: &'static ProductKind, sector: &'static Sector, page| {
            std::future::ready(Ok::<_, anyhow::Error>(
                if (kind.key, sector.code, page) == ("saving", "030300", 2) {
                    empty_sector()
                } else {
                    body(kind, sector, page).unwrap_or_else(empty_sector)
                },
            ))
        },
        fast(),
    )
    .await
    .unwrap();
    let savings = part(&fetch, "fss_030300_saving_");
    assert_eq!(code(savings), Some("short_read"), "{savings:?}");
    assert_eq!(savings.seen, 100);
}

/// A product whose disclosure has ended is not kept, decided where every
/// run looks (the fetch), not where only changed rows pass: listed after
/// its end day, it goes the way of a product the filter drops.
#[tokio::test]
async fn a_product_past_its_disclosure_end_is_not_kept() {
    let limits = Limits {
        today: chrono::NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
        ..fast()
    };
    let fetch = collect_cells(transport(HashMap::new(), Arc::default()), limits)
        .await
        .unwrap();
    let gwangju = "fss_rent_loan_0010019_LN11010001072";
    assert!(fetch.records.iter().all(|r| r.source_id != gwangju));
    assert!(fetch.filtered_out.iter().any(|id| id == gwangju));
    // On the capture day it is current and kept.
    let fetch = collect_cells(transport(HashMap::new(), Arc::default()), fast())
        .await
        .unwrap();
    assert!(fetch.records.iter().any(|r| r.source_id == gwangju));
}

/// A defective row that still names its product is reported by id, so a
/// stored programme it would retire is visible.
#[tokio::test]
async fn a_set_aside_row_is_reported_by_id() {
    let mut rent = fixture_json("rentHouseLoanProductsSearch_020000_p1.json");
    let rows = rent["result"]["baseList"].as_array_mut().unwrap();
    let gwangju = rows
        .iter_mut()
        .find(|row| row["fin_prdt_cd"] == "LN11010001072")
        .unwrap();
    gwangju["loan_inci_expn"] = serde_json::json!("가".repeat(MAX_TEXT_CHARS + 1));
    let overrides = HashMap::from([(("rent_house_loan", "020000"), rent.to_string())]);
    let fetch = collect_cells(transport(overrides, Arc::default()), fast())
        .await
        .unwrap();
    assert_eq!(fetch.set_aside_ids, ["fss_rent_loan_0010019_LN11010001072"]);
    assert_eq!(part(&fetch, "fss_rent_loan_").set_aside, 1);
}

/// A cell that fails once is read again at the end of the run and settles.
#[tokio::test]
async fn a_transient_failure_is_read_again_and_settles() {
    let calls = Arc::new(Mutex::new(0usize));
    let counter = calls.clone();
    let fetch = collect_cells(
        move |kind: &'static ProductKind, sector: &'static Sector, page| {
            let first_rent = kind.key == "rent_house_loan" && sector.code == "020000" && {
                let mut n = counter.lock().unwrap();
                *n += 1;
                *n == 1
            };
            std::future::ready(if first_rent {
                Err(anyhow::anyhow!(
                    "fss_financial: upstream request failed: timed out"
                ))
            } else {
                Ok(body(kind, sector, page).unwrap_or_else(empty_sector))
            })
        },
        fast(),
    )
    .await
    .unwrap();
    assert_eq!(part(&fetch, "fss_rent_loan_").state, PartState::Settled);
    assert!(fetch
        .records
        .iter()
        .any(|r| r.source_id == "fss_rent_loan_0010019_LN11010001072"));
    assert_eq!(*calls.lock().unwrap(), 2);
}

/// Three cells of a kind in a row failing the same way with nothing read
/// stop that kind: a provider that answers nothing is asked three times per
/// endpoint (12), not 20 × 3 attempts; the run's budget bounds a hang.
#[tokio::test]
async fn a_provider_that_does_not_answer_is_asked_three_times() {
    let calls = Arc::new(Mutex::new(0usize));
    let counter = calls.clone();
    let error = collect_cells(
        move |_: &'static ProductKind, _: &'static Sector, _| {
            *counter.lock().unwrap() += 1;
            std::future::ready(Err::<String, _>(anyhow::anyhow!(
                "fss_financial: upstream request failed: timed out"
            )))
        },
        fast(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("no cell answered"), "{error}");
    assert_eq!(*calls.lock().unwrap(), 3 * KINDS.len());
}

/// The key, the quota or the IP rejected: the run stops asking at once; the
/// cells read before are kept, the rest are `not_attempted`.
#[tokio::test]
async fn a_source_wide_rejection_stops_the_run() {
    let calls = Arc::new(Mutex::new(0usize));
    let counter = calls.clone();
    let quota = r#"{"result":{"err_cd":"020","err_msg":"일일검색 허용횟수 초과"}}"#;
    let fetch = collect_cells(
        move |kind: &'static ProductKind, sector: &'static Sector, page| {
            let n = {
                let mut n = counter.lock().unwrap();
                *n += 1;
                *n
            };
            std::future::ready(Ok(if n == 1 {
                body(kind, sector, page).unwrap_or_else(empty_sector)
            } else {
                quota.to_string()
            }))
        },
        fast(),
    )
    .await
    .unwrap();
    assert_eq!(*calls.lock().unwrap(), 2);
    assert_eq!(part(&fetch, "fss_deposit_").state, PartState::Empty);
    assert_eq!(
        code(part(&fetch, "fss_030200_deposit_")),
        Some("source_rejected")
    );
    let not_attempted = fetch
        .parts
        .iter()
        .filter(|p| code(p) == Some("not_attempted"))
        .count();
    assert_eq!(not_attempted, 18);
    // Nothing answered at all fails the run.
    let error = collect_cells(
        move |_: &'static ProductKind, _: &'static Sector, _| {
            std::future::ready(Ok::<_, anyhow::Error>(quota.to_string()))
        },
        fast(),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("source_rejected"), "{error}");
}

/// A provider slower than the run's budget: what was read is returned, the
/// cells not reached are `not_attempted`, and nothing is read again.
#[tokio::test]
async fn the_run_budget_bounds_a_slow_provider() {
    let calls = Arc::new(Mutex::new(0usize));
    let counter = calls.clone();
    let limits = Limits {
        budget: Duration::from_millis(120),
        ..fast()
    };
    let fetch = collect_cells(
        move |kind: &'static ProductKind, sector: &'static Sector, page| {
            *counter.lock().unwrap() += 1;
            let answer = body(kind, sector, page).unwrap_or_else(empty_sector);
            async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(answer)
            }
        },
        limits,
    )
    .await
    .unwrap();
    let calls = *calls.lock().unwrap();
    assert!((2..=4).contains(&calls), "{calls}");
    let not_attempted = fetch
        .parts
        .iter()
        .filter(|p| code(p) == Some("not_attempted"))
        .count();
    // The cell under way when the budget ran out is not_attempted too.
    assert!(
        (20 - calls..=21 - calls).contains(&not_attempted),
        "{not_attempted}"
    );
}

/// A field the filter reads, empty on every row of a cell with enough rows,
/// means the provider moved it: the cell is `schema_changed`, so its band
/// products are not quietly retired.
#[tokio::test]
async fn a_filter_field_emptied_on_every_row_is_a_schema_change() {
    let mut page = fixture_json("savingProductsSearch_030300_p1.json");
    page["result"]["total_count"] = serde_json::json!(100);
    page["result"]["max_page_no"] = serde_json::json!(1);
    for row in page["result"]["baseList"].as_array_mut().unwrap() {
        row["join_member"] = serde_json::json!("");
    }
    let overrides = HashMap::from([(("saving", "030300"), page.to_string())]);
    let fetch = collect_cells(transport(overrides, Arc::default()), fast())
        .await
        .unwrap();
    let savings = part(&fetch, "fss_030300_saving_");
    assert_eq!(code(savings), Some("schema_changed"));
    match &savings.state {
        PartState::Unsettled(u) => assert!(u.detail.contains("join_member"), "{}", u.detail),
        other => panic!("{other:?}"),
    }
}

/// A repeated id inside one cell is set aside and counted.
#[tokio::test]
async fn a_repeated_id_inside_a_cell_is_set_aside() {
    let mut page = fixture_json("rentHouseLoanProductsSearch_020000_p1.json");
    let rows = page["result"]["baseList"].as_array_mut().unwrap();
    let copy = rows[0].clone();
    rows.push(copy);
    page["result"]["total_count"] = serde_json::json!(42);
    let overrides = HashMap::from([(("rent_house_loan", "020000"), page.to_string())]);
    let fetch = collect_cells(transport(overrides, Arc::default()), fast())
        .await
        .unwrap();
    let rent = part(&fetch, "fss_rent_loan_");
    assert_eq!(rent.state, PartState::Settled);
    assert_eq!((rent.seen, rent.kept, rent.set_aside), (42, 1, 1));
}

/// A transport error from the real client never carries the request URL,
/// which holds the API key.
#[tokio::test]
async fn a_request_error_never_echoes_the_key() {
    let source = FssFinancialSource::new("SECRET-KEY-123".into())
        .with_base_url("http://127.0.0.1:9/finlifeapi/");
    let error = source
        .fetch_body(&DEPOSIT, &BANK, 1)
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains("SECRET-KEY-123"), "{error}");
    assert!(!error.contains("auth="), "{error}");
    assert!(!error.contains("127.0.0.1"), "{error}");
}

/// A redirect is never followed (the key would go with it, in the URL and
/// the Referer): it is a cell failure of its own.
#[tokio::test]
async fn a_redirect_is_not_followed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = requests.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = vec![0u8; 4096];
            let n = socket.read(&mut buffer).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buffer[..n]).to_string();
            seen.lock()
                .unwrap()
                .push(head.lines().next().unwrap_or("").to_string());
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{address}/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    let source = FssFinancialSource::new("SECRET-KEY-123".into())
        .with_base_url(&format!("http://{address}/finlifeapi/"));
    let error = collect_cells(|k, s, p| source.fetch_body(k, s, p), fast())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("redirected"), "{error}");
    assert!(!error.contains("SECRET-KEY-123"), "{error}");
    let requests = requests.lock().unwrap();
    // Three cells of each kind asked (then its breaker), none at the
    // redirect target.
    assert_eq!(requests.len(), 3 * KINDS.len(), "{requests:?}");
    assert!(requests.iter().all(|line| !line.contains("/elsewhere")));
}
