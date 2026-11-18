//! Issuer + redeem offers (corp-kr-routes 1단계): SK텔레콤's 청년 혜택 page
//! read the way the evidence job reads it, the seeded programmes, and the
//! database rules that keep "CGV 50%" from showing without its SKT plan.
use super::*;
use crate::corporate_auto::resilience::{resolve, Anchors, PageView, Resolution, StoredFact};

/// The structure of SK텔레콤's 청년 혜택 page (NA00009829) on 2026-11-06, in
/// our own words where the page's do not matter: the visible summary that
/// states each youth item, a detail panel that opens only on a button
/// (`aria-hidden` until then), and the conditions block whose age rule ends
/// the benefit automatically ("… 자동 종료/해지") without ending the offer.
fn skt_youth_page(extra: &str) -> String {
    format!(
        "<html><head><title>청년 혜택 &lt; 상품 &lt; T world</title></head><body><main>\
         <h1>청년 혜택</h1><p>만 34세 이하 청소년/청년 고객님에게 추가 데이터 및 커피/영화/로밍 혜택 제공</p>\
         <section><h2>청년 혜택이란?</h2>\
         <p>만 34세 이하 베스트/ 라이트 요금제 가입 고객님께 제공됩니다.</p>\
         <p>(연령에 따라 청년 혜택 자동 적용)</p>\
         <ul><li><strong>영화 50% 할인</strong><span>CGV 영화 관람권 월 1장</span><span>50% 할인</span></li>\
         <li><strong>로밍 50% 할인</strong><span>baro 요금제 50% 할인</span><span>(연 2회)</span></li></ul>\
         <p>T 멤버십 앱의 청년 혜택 신청 페이지에서 커피 쿠폰 다운로드 및 영화 예매 가능</p></section>\
         <button aria-haspopup=\"true\" aria-expanded=\"false\">혜택 자세히 보기</button>\
         <div class=\"none\" aria-hidden=\"true\" data-title=\"혜택 자세히 보기\">\
         <p>CGV 영화 관람권 50% 할인(월 1회)</p></div>\
         <section><h2>유의 사항</h2>{extra}\
         <p>청년 혜택은 명의당 1회선만 이용 가능하며, 같은 명의 다른 회선으로는 받을 수 없습니다.</p></section>\
         </main><footer>SK텔레콤(주)</footer></body></html>"
    )
}

fn first_sight(html: &str, quote: &str, max_age: Option<i32>) -> Resolution {
    let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 6)
        .unwrap()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    let view = PageView::at(html, &visible_text(html), now)
        .with_url("https://m.tworld.co.kr/product/callplan?prod_id=NA00009829");
    let anchors = Anchors::default();
    resolve(
        &view,
        &StoredFact {
            quote,
            baseline: None,
            ages: Ages {
                min: None,
                max: max_age,
            },
            adoption_open: true,
            anchors: &anchors,
        },
    )
}

/// The known SKT false ending: "연령 기준을 초과하면 … 자동 종료" is the age
/// rule of the benefit, a condition, not the offer ending. Every seeded
/// statement confirms on the page that carries it.
#[test]
fn the_age_rule_ending_the_youth_benefit_is_a_condition_not_an_ending() {
    let html = skt_youth_page(
        "<p>청년 혜택 이용 중 연령 기준을 초과하면, 만 35세 생일의 다음 달 1일에 청년 혜택이 자동 종료됩니다.</p>\
         <p>- 연령 초과 시 만 35세 생일이 되는 다음 달 1일에 청년 혜택이 자동 해지됩니다.</p>",
    );
    for item in &issuer_seeds().items {
        for fragment in split_fragments(&item.quote) {
            let age = read_quote(&fragment).0 == "age";
            let resolution = first_sight(&html, &fragment, age.then_some(34));
            assert!(
                matches!(resolution, Resolution::Exact(FactOutcome::Confirmed { .. })),
                "{}: {fragment}: {resolution:?}",
                item.title
            );
        }
    }
}

/// The seeds rest on the page's visible statements. A copy that sits only
/// in a panel hidden until a button opens it is not what the page shows, so
/// it is never evidence: the prod SKT 라이트 59 fact quoted such a panel and
/// reads as missing (the "hidden tab" defect), so the seeds quote the
/// visible summary instead.
#[test]
fn a_statement_only_in_a_hidden_detail_panel_is_not_evidence() {
    let html = skt_youth_page("");
    assert!(matches!(
        first_sight(&html, "CGV 영화 관람권 50% 할인(월 1회)", None),
        Resolution::Exact(FactOutcome::QuoteMissing)
    ));
    assert!(matches!(
        first_sight(&html, "CGV 영화 관람권 월 1장 50% 할인", None),
        Resolution::Exact(FactOutcome::Confirmed { .. })
    ));
}

/// The seed file is the reviewed list: every item names its programme by id
/// on the issuer's own page, and every fragment is a statement of at most
/// 300 characters.
#[test]
fn issuer_seeds_are_statements_on_the_issuers_own_page() {
    let seeds = issuer_seeds();
    assert!(!seeds.items.is_empty());
    for item in &seeds.items {
        assert!(
            item.official_url.starts_with("https://m.tworld.co.kr/"),
            "{}",
            item.title
        );
        assert!(item.title.starts_with("SKT "), "{}", item.title);
        for fragment in split_fragments(&item.quote) {
            assert!((MIN_FRAGMENT_CHARS..=300).contains(&compact(&fragment).chars().count()));
        }
    }
}

/// One seeded programme: title, shown, status, condition, `brand:relation`s.
type SeedRow = (String, bool, String, Option<String>, Vec<String>);

async fn issuer_db() -> Option<PgPool> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    Some(
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap(),
    )
}

/// The seeded programmes are hidden until evidence confirms them, carry their
/// issuer as sponsor and the brand where they are used as redeemable_at,
/// state their condition, and get their evidence once (idempotent).
#[tokio::test]
async fn issuer_seeds_are_hidden_issuer_offers_with_their_condition_and_evidence() {
    let Some(pool) = issuer_db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let rows: Vec<SeedRow> = sqlx::query_as(
        "SELECT p.title, p.is_active, p.program_status, p.eligibility_condition,
                array_agg(b.name || ':' || pb.relationship_type ORDER BY pb.relationship_type)
         FROM programs p JOIN program_brands pb ON pb.program_id = p.id JOIN brands b ON b.id = pb.brand_id
         WHERE p.source_id LIKE 'issuer:20261106:%'
         GROUP BY p.id ORDER BY p.title",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for (title, active, status, condition, brands) in &rows {
        assert!(
            !active && status == "review_required",
            "{title}: hidden until confirmed"
        );
        assert_eq!(
            condition.as_deref(),
            Some("만 34세 이하 베스트/ 라이트 요금제 가입 고객님께 제공됩니다.")
        );
        assert!(
            brands.contains(&"SK텔레콤:sponsor".to_string()),
            "{brands:?}"
        );
        assert!(!title.starts_with("CGV"), "{title}");
    }
    assert!(rows
        .iter()
        .any(|(_, _, _, _, b)| b.contains(&"CGV:redeemable_at".to_string())));
    let first = seed_issuer_evidence(&pool).await.unwrap();
    let again = seed_issuer_evidence(&pool).await.unwrap();
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM corporate_offer_facts WHERE evidence_source = 'issuer_20261106'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(facts, 4, "two statements for each of the two offers");
    assert!(first <= 4 && again == 0, "first {first}, again {again}");
}

/// The database keeps the model honest: an offer whose redeem brand differs
/// from its issuer cannot lose its condition, nor be titled by the redeem
/// brand ("CGV 대학생 할인").
#[tokio::test]
async fn an_issuer_offer_needs_its_condition_and_is_never_titled_by_its_redeem_brand() {
    let Some(pool) = issuer_db().await else {
        eprintln!("TEST_DATABASE_URL unset; skipping");
        return;
    };
    let id: Uuid = "7a1c2e30-6b1f-4f7e-9d61-0c3e9a5b1101".parse().unwrap();
    for (sql, what) in [
        (
            "UPDATE programs SET eligibility_condition = NULL WHERE id = $1",
            "no condition",
        ),
        (
            "UPDATE programs SET title = 'CGV 청년 50% 할인' WHERE id = $1",
            "redeem title",
        ),
    ] {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(sql).bind(id).execute(&mut *tx).await.unwrap();
        let committed = tx.commit().await;
        assert!(committed.is_err(), "{what} must be refused");
    }
    // The roaming offer is SK텔레콤's own product: no redeem brand, so its
    // condition is not required by the rule (it still carries one).
    let roaming: Uuid = "7a1c2e30-6b1f-4f7e-9d61-0c3e9a5b1102".parse().unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE programs SET eligibility_condition = NULL WHERE id = $1")
        .bind(roaming)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
}
