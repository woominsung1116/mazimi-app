use axum::{http::StatusCode, Json};
use chrono::Utc;
use mazimi_core::models::ProfileInput;
use serde_json::{json, Value};

type Error = (StatusCode, Json<Value>);

/// Minimum age for collecting personal data without guardian consent
/// (개인정보 보호법 제22조의2). Only the birth year is collected, so the age is
/// a range: someone born `current_year - MINIMUM_AGE` may still be a year
/// younger until their birthday. The gate is conservative and rejects while
/// the lower bound of that range is below the minimum.
const MINIMUM_AGE: i32 = 14;

fn bad_request(message: &str) -> Error {
    (StatusCode::BAD_REQUEST, Json(json!({"error": message})))
}

/// A 400 that names the request field and a stable code for clients.
pub(crate) fn field_error(message: &str, field: &str, code: &str) -> Error {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message, "field": field, "code": code})),
    )
}

/// Earliest accepted date of birth (the same floor as `birth_year`).
const EARLIEST_BIRTH_DATE: (i32, u32, u32) = (1920, 1, 1);

/// Settle `birth_year` from `birth_date` when one is given: the date wins,
/// a contradicting year is rejected, and the date must be a real past date
/// of someone at least [`MINIMUM_AGE`] (exact 만 나이). Without a date the
/// birth year is required.
fn settle_birth(input: &mut ProfileInput, today: chrono::NaiveDate) -> Result<(), Error> {
    let Some(date) = input.birth_date else {
        if input.birth_year == 0 {
            return Err(field_error(
                "생년월일을 입력해 주세요.",
                "birth_date",
                "birth_date_required",
            ));
        }
        return Ok(());
    };
    let (y, m, d) = EARLIEST_BIRTH_DATE;
    let earliest = chrono::NaiveDate::from_ymd_opt(y, m, d).expect("valid floor date");
    if date < earliest || date > today {
        return Err(field_error(
            "생년월일을 다시 확인해 주세요.",
            "birth_date",
            "birth_date_invalid",
        ));
    }
    use chrono::Datelike;
    if input.birth_year != 0 && input.birth_year != date.year() {
        return Err(field_error(
            "생년월일과 출생연도가 맞지 않아요.",
            "birth_date",
            "birth_date_mismatch",
        ));
    }
    input.birth_year = date.year();
    if mazimi_core::rule_engine::age_on(date, today) < MINIMUM_AGE {
        return Err(field_error(
            "만 14세 이상만 이용할 수 있어요.",
            "birth_date",
            "under_minimum_age",
        ));
    }
    Ok(())
}

/// Profile storage, preview and catalogue filters must use the same region key.
/// In particular an unrecognised value must not silently produce a nationwide
/// result that the caller interprets as "my region".
pub(crate) fn region_code(value: &str) -> Result<&'static str, Error> {
    mazimi_core::canonical_region_code(value)
        .ok_or_else(|| bad_request("지역을 시·도 단위로 선택해 주세요."))
}

/// Values clients send for "no answer" / "not applicable". They are unknown,
/// never a contradiction.
fn is_blank(value: &str) -> bool {
    matches!(value.trim(), "" | "none" | "해당없음" | "해당 없음")
}

/// Canonical enrollment code for the preview body, the saved profile and the
/// campus catalogue filter. Hard filters compare codes exactly, so a value the
/// server cannot map is rejected instead of silently excluding the user from
/// every program that asks about enrollment.
pub(crate) fn enrollment_status(value: Option<&str>) -> Result<Option<&'static str>, Error> {
    let Some(value) = value.filter(|value| !is_blank(value)) else {
        return Ok(None);
    };
    mazimi_core::canonical_enrollment_status(value)
        .map(Some)
        .ok_or_else(|| bad_request("재학 상태는 재학·휴학·졸업 중에서 선택해 주세요."))
}

/// Canonical employment code; see [`enrollment_status`].
pub(crate) fn employment_status(value: Option<&str>) -> Result<Option<&'static str>, Error> {
    let Some(value) = value.filter(|value| !is_blank(value)) else {
        return Ok(None);
    };
    mazimi_core::canonical_employment_status(value)
        .map(Some)
        .ok_or_else(|| {
            bad_request("취업 상태는 재직·미취업·취업 준비·자영업·학생·기타 중에서 선택해 주세요.")
        })
}

/// The residence 시·군·구: a current 5-digit 시군구 code of the canonical
/// `region`, as `mazimi_core::structural` reads it (강원 42→51, 전북 45→52
/// stored under the current prefix). `None` for a blank answer or anything
/// else.
fn city_code(value: Option<&str>, region: &str) -> Option<String> {
    use mazimi_core::structural::{district_code, district_region, is_known_district};
    let value = value.filter(|value| !is_blank(value))?.trim();
    (value.len() == 5)
        .then_some(value)
        .and_then(district_code)
        .filter(|code| district_region(code) == Some(region) && is_known_district(code))
}

/// HTTP boundary check for a client-sent `city_code` (`POST /profile`,
/// `POST /recommend/preview`): a non-blank value must be a current 시·군·구
/// of the sido, or the request is rejected instead of silently reading as
/// "no district". Internal callers go through [`normalize_profile`], which
/// drops such a value instead, so a stale stored row or a region the
/// assistant overrides never fails a request.
pub(crate) fn check_city_code(input: &ProfileInput) -> Result<(), Error> {
    let Some(region) = mazimi_core::canonical_region_code(&input.region_code) else {
        // normalize_profile rejects the region itself.
        return Ok(());
    };
    match input.city_code.as_deref() {
        Some(value) if !is_blank(value) && city_code(Some(value), region).is_none() => {
            Err(bad_request("시·군·구는 거주 시·도 안에서 선택해 주세요."))
        }
        _ => Ok(()),
    }
}

pub(crate) fn normalize_profile(input: &mut ProfileInput) -> Result<(), Error> {
    input.region_code = region_code(&input.region_code)?.to_owned();
    input.city_code = city_code(input.city_code.as_deref(), &input.region_code);
    settle_birth(input, mazimi_core::seoul_date(Utc::now()))?;
    let current_year = mazimi_core::seoul_year(Utc::now());
    if !(1920..=current_year).contains(&input.birth_year) {
        return Err(field_error(
            &format!("birth_year must be between 1920 and {current_year}"),
            "birth_year",
            "birth_year_invalid",
        ));
    }
    // Youngest possible age for this birth year (birthday not yet reached).
    // A date of birth was checked exactly above.
    if input.birth_date.is_none() && current_year - input.birth_year - 1 < MINIMUM_AGE {
        return Err(field_error(
            "만 14세 이상만 이용할 수 있어요.",
            "birth_year",
            "under_minimum_age",
        ));
    }

    // Same bounds as the agent's profile_patch (`mazimi_core::agent`), so a
    // rule such as "소득분위 ≤ 5" never passes on an impossible value.
    for (value, range, message) in [
        (input.school_year, 1..=6, "학년은 1~6 사이로 입력해 주세요."),
        (
            input.income_bracket,
            0..=10,
            "소득분위는 0~10 사이로 입력해 주세요.",
        ),
        (
            input.kosaf_support_bracket,
            0..=10,
            "학자금 지원구간은 0~10 사이로 입력해 주세요.",
        ),
        (
            input.household_size,
            1..=20,
            "가구원 수는 1~20명으로 입력해 주세요.",
        ),
        (
            input.children_count,
            0..=20,
            "자녀 수는 0~20명으로 입력해 주세요.",
        ),
    ] {
        if value.is_some_and(|v| !range.contains(&v)) {
            return Err(bad_request(message));
        }
    }

    let enrollment = input.enrollment_status.take();
    let mut employment_from_enrollment = None;
    input.enrollment_status = match enrollment_status(enrollment.as_deref()) {
        Ok(code) => code.map(str::to_owned),
        Err(error) => {
            // The web onboarding step lists "취업준비생" among its enrollment
            // options. That is an employment answer; keep it as one instead of
            // failing the whole profile.
            employment_from_enrollment = enrollment
                .as_deref()
                .and_then(mazimi_core::canonical_employment_status);
            if employment_from_enrollment.is_none() {
                return Err(error);
            }
            None
        }
    };
    input.employment_status = employment_status(input.employment_status.as_deref())?
        .or(employment_from_enrollment)
        .map(str::to_owned);

    normalize_optional_answers(input)?;

    input.school_name = input
        .school_name
        .take()
        .map(|school| school.trim().to_owned())
        .filter(|school| !school.is_empty());
    if input
        .school_name
        .as_ref()
        .is_some_and(|school| school.chars().count() > 200)
    {
        return Err(bad_request("학교 이름은 200자 이내로 입력해 주세요."));
    }
    Ok(())
}

/// The optional 무주택 / 혼인·자녀 / 중위소득 answers. A blank value is
/// unanswered; 신혼 is kept only for a married answer.
fn normalize_optional_answers(input: &mut ProfileInput) -> Result<(), Error> {
    input.marital_status = match input.marital_status.take() {
        Some(value) if !is_blank(&value) => Some(
            match value.trim() {
                "unmarried" | "미혼" => "unmarried",
                "married" | "기혼" => "married",
                _ => return Err(bad_request("혼인 여부는 미혼·기혼 중에서 선택해 주세요.")),
            }
            .to_owned(),
        ),
        _ => None,
    };
    if input.marital_status.as_deref() != Some("married") {
        input.newlywed = None;
    }
    input.income_median_band = match input.income_median_band.take() {
        Some(value) if !is_blank(&value) => {
            let value = value.trim();
            let known = value == mazimi_core::rule_engine::INCOME_MEDIAN_UNKNOWN
                || mazimi_core::rule_engine::INCOME_MEDIAN_BANDS
                    .iter()
                    .any(|(code, _, _)| *code == value);
            if !known {
                return Err(bad_request("기준 중위소득 구간은 목록에서 선택해 주세요."));
            }
            Some(value.to_owned())
        }
        _ => None,
    };
    check_holder_answers(input)?;
    Ok(())
}

/// The holder answers ([`mazimi_core::holder`]): every code from its
/// field's domain, no code twice, one carrier. `None` stays unanswered.
fn check_holder_answers(input: &ProfileInput) -> Result<(), Error> {
    use mazimi_core::holder::{validate_answer, HolderField};
    let carrier = input.telecom_carrier.as_ref().map(|c| vec![c.clone()]);
    for (field, codes) in [
        (HolderField::TelecomCarrier, carrier.as_deref()),
        (HolderField::CardIssuers, input.card_issuers.as_deref()),
        (HolderField::PayApps, input.pay_apps.as_deref()),
    ] {
        if let Some(codes) = codes {
            if let Err(code) = validate_answer(field, codes) {
                let message = match field {
                    HolderField::TelecomCarrier => "통신사는 목록에서 선택해 주세요.",
                    HolderField::CardIssuers => "보유 카드는 목록에서 선택해 주세요.",
                    HolderField::PayApps => "간편결제는 목록에서 선택해 주세요.",
                };
                return Err(field_error(message, field.name(), code));
            }
        }
    }
    Ok(())
}

pub(crate) async fn resolve_university<'a>(
    executor: impl sqlx::Executor<'a, Database = sqlx::Postgres>,
    school: &str,
) -> Result<Option<(String, String)>, Error> {
    sqlx::query_as(
        r#"
        SELECT id, name
        FROM universities
        WHERE is_active AND (id = $1 OR name = $1 OR $1 = ANY(aliases))
        ORDER BY (id = $1) DESC, id
        LIMIT 1
        "#,
    )
    .bind(school.trim())
    .fetch_optional(executor)
    .await
    .map_err(crate::errors::internal_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(extra: Value) -> ProfileInput {
        let mut payload = json!({"birth_year": 2000, "region_code": "busan"});
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(payload).unwrap()
    }

    #[test]
    fn numeric_profile_answers_must_be_in_range() {
        for extra in [
            json!({"income_bracket": -1}),
            json!({"income_bracket": 11}),
            json!({"kosaf_support_bracket": 11}),
            json!({"household_size": 0}),
            json!({"household_size": 21}),
            json!({"school_year": 0}),
            json!({"school_year": 7}),
        ] {
            let mut profile = input(extra.clone());
            assert_eq!(
                normalize_profile(&mut profile).unwrap_err().0,
                StatusCode::BAD_REQUEST,
                "{extra}"
            );
        }
        let mut ok = input(json!({
            "income_bracket": 0, "kosaf_support_bracket": 10,
            "household_size": 1, "school_year": 6
        }));
        assert!(normalize_profile(&mut ok).is_ok());
    }

    #[test]
    fn normalizes_supported_region_aliases_and_rejects_unknown_scope() {
        for value in ["busan", " BUSAN ", "부산", "부산광역시"] {
            assert_eq!(region_code(value).unwrap(), "busan");
        }
        for value in ["", "unknown", "전국", "용인시", "typo"] {
            assert_eq!(region_code(value).unwrap_err().0, StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn preview_and_saved_profile_share_validation() {
        let mut input: ProfileInput = serde_json::from_value(json!({
            "birth_year": 2000, "region_code": "부산광역시", "school_name": "  "
        }))
        .unwrap();
        normalize_profile(&mut input).unwrap();
        assert_eq!(input.region_code, "busan");
        assert_eq!(input.school_name, None);
        input.birth_year = mazimi_core::seoul_year(Utc::now()) + 1;
        assert_eq!(
            normalize_profile(&mut input).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn normalizes_client_status_vocabulary() {
        for (enrollment, employment) in [
            ("대학생", "재직"),
            ("재학", "재직자"),
            (" ENROLLED ", "EMPLOYED"),
            ("재학중", "직장인"),
        ] {
            let mut profile = input(json!({
                "enrollment_status": enrollment, "employment_status": employment
            }));
            normalize_profile(&mut profile).unwrap();
            assert_eq!(profile.enrollment_status.as_deref(), Some("enrolled"));
            assert_eq!(profile.employment_status.as_deref(), Some("employed"));
        }
        let mut profile = input(json!({
            "enrollment_status": "휴학생", "employment_status": "취업준비생"
        }));
        normalize_profile(&mut profile).unwrap();
        assert_eq!(profile.enrollment_status.as_deref(), Some("on_leave"));
        assert_eq!(profile.employment_status.as_deref(), Some("preparing"));
    }

    #[test]
    fn blank_status_answers_stay_unknown() {
        for (enrollment, employment) in [("", ""), ("none", "해당없음"), ("  ", "해당 없음")]
        {
            let mut profile = input(json!({
                "enrollment_status": enrollment, "employment_status": employment
            }));
            normalize_profile(&mut profile).unwrap();
            assert_eq!(profile.enrollment_status, None, "{enrollment:?}");
            assert_eq!(profile.employment_status, None, "{employment:?}");
        }
    }

    #[test]
    fn web_job_seeker_enrollment_answer_becomes_an_employment_answer() {
        let mut profile = input(json!({"enrollment_status": "취업준비생"}));
        normalize_profile(&mut profile).unwrap();
        assert_eq!(profile.enrollment_status, None);
        assert_eq!(profile.employment_status.as_deref(), Some("preparing"));

        let mut profile = input(json!({
            "enrollment_status": "취업준비생", "employment_status": "재직"
        }));
        normalize_profile(&mut profile).unwrap();
        assert_eq!(profile.enrollment_status, None);
        assert_eq!(
            profile.employment_status.as_deref(),
            Some("employed"),
            "an explicit employment answer wins"
        );
    }

    #[test]
    fn unrecognised_status_answers_are_rejected_in_korean() {
        for payload in [
            json!({"enrollment_status": "학생아님"}),
            json!({"employment_status": "typo"}),
            json!({"enrollment_status": "재학", "employment_status": "재학"}),
        ] {
            let mut profile = input(payload.clone());
            let (status, Json(body)) = normalize_profile(&mut profile).unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
            assert!(
                body["error"].as_str().unwrap().contains("선택해 주세요"),
                "{payload}: {body}"
            );
        }
    }

    #[test]
    fn city_code_is_a_district_of_the_profile_sido() {
        for (region, city, stored) in [
            ("busan", "26500", "26500"),
            ("부산광역시", " 26500 ", "26500"),
            ("gyeonggi", "41110", "41110"),
            ("gyeonggi", "41117", "41117"),
            // Renumbered provinces are stored under the current prefix.
            ("gangwon", "42110", "51110"),
            ("jeonbuk", "52110", "52110"),
            // 광주·전남 are sent under the code the matcher reads.
            ("gwangju", "29170", "29170"),
            ("jeonnam", "46150", "46150"),
        ] {
            let mut profile = input(json!({"region_code": region, "city_code": city}));
            check_city_code(&profile).unwrap();
            normalize_profile(&mut profile).unwrap();
            assert_eq!(
                profile.city_code.as_deref(),
                Some(stored),
                "{region} {city}"
            );
        }
        for city in [None, Some(""), Some("  "), Some("none")] {
            let mut profile = input(json!({"city_code": city}));
            check_city_code(&profile).unwrap();
            normalize_profile(&mut profile).unwrap();
            assert_eq!(profile.city_code, None, "{city:?}");
        }
        for (region, city) in [
            ("busan", "11110"),
            ("busan", "gangnam"),
            ("busan", "2650"),
            ("busan", "265000"),
            ("busan", "2650000000"),
            ("busan", "26500abc"),
            ("busan", "２６５００"),
            // The core does not read the 2026 전남광주 numbering.
            ("gwangju", "12300"),
            ("jeonnam", "29170"),
            // Well-formed, inside the sido, but not a current 시·군·구.
            ("busan", "26999"),
            ("gangwon", "42999"),
        ] {
            let mut profile = input(json!({"region_code": region, "city_code": city}));
            // Rejected at the HTTP boundary…
            let (status, Json(body)) = check_city_code(&profile).unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{region} {city}");
            assert!(
                body["error"].as_str().unwrap().contains("선택해 주세요"),
                "{body}"
            );
            // …and dropped for internal callers (stored rows, assistant
            // region overrides), which must not fail on it.
            normalize_profile(&mut profile).unwrap();
            assert_eq!(profile.city_code, None, "{region} {city}");
        }
    }

    fn code(result: Result<(), Error>) -> String {
        let (status, Json(body)) = result.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        body["code"].as_str().unwrap().to_owned()
    }

    fn dated(extra: Value) -> ProfileInput {
        let mut payload = json!({"region_code": "busan"});
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(payload).unwrap()
    }

    #[test]
    fn birth_date_sets_the_birth_year_and_wins_over_a_missing_one() {
        let mut profile = dated(json!({"birth_date": "2001-03-15"}));
        normalize_profile(&mut profile).unwrap();
        assert_eq!(profile.birth_year, 2001);
        assert_eq!(profile.birth_date.unwrap().to_string(), "2001-03-15");
        let mut same = dated(json!({"birth_date": "2001-03-15", "birth_year": 2001}));
        normalize_profile(&mut same).unwrap();
        assert_eq!(same.birth_year, 2001);
    }

    #[test]
    fn birth_date_errors_name_the_field() {
        assert_eq!(
            code(normalize_profile(&mut dated(json!({})))),
            "birth_date_required"
        );
        assert_eq!(
            code(normalize_profile(&mut dated(
                json!({"birth_date": "2001-03-15", "birth_year": 2000})
            ))),
            "birth_date_mismatch"
        );
        for bad in ["1919-12-31", "2999-01-01"] {
            assert_eq!(
                code(normalize_profile(&mut dated(json!({"birth_date": bad})))),
                "birth_date_invalid",
                "{bad}"
            );
        }
        let (_, Json(body)) = normalize_profile(&mut dated(json!({}))).unwrap_err();
        assert_eq!(body["field"], "birth_date");
        assert_eq!(body["error"], "생년월일을 입력해 주세요.");
    }

    #[test]
    fn birth_date_applies_the_exact_minimum_age() {
        let today = mazimi_core::seoul_date(Utc::now());
        use chrono::Datelike;
        // Turns 14 today: allowed. Turns 14 tomorrow: still 13.
        let fourteen_today = today.with_year(today.year() - 14).unwrap_or_else(|| {
            // 29 February today: 14 years ago has no such day.
            chrono::NaiveDate::from_ymd_opt(today.year() - 14, 2, 28).unwrap()
        });
        let mut ok = dated(json!({"birth_date": fourteen_today.to_string()}));
        normalize_profile(&mut ok).unwrap();
        let tomorrow_fourteen = fourteen_today + chrono::Duration::days(1);
        assert_eq!(
            code(normalize_profile(&mut dated(
                json!({"birth_date": tomorrow_fourteen.to_string()})
            ))),
            "under_minimum_age"
        );
        // The same calendar year by birth_year alone is still refused: the
        // birthday may not have passed.
        assert_eq!(
            code(normalize_profile(&mut input(
                json!({"birth_year": fourteen_today.year()})
            ))),
            "under_minimum_age"
        );
    }

    #[test]
    fn rejects_users_who_may_still_be_under_fourteen() {
        let year = mazimi_core::seoul_year(Utc::now());
        // Born 14 calendar years ago: 13 until this year's birthday, so the
        // birth year alone cannot prove 만 14세.
        for birth_year in [year - 13, year - 14] {
            let mut profile = input(json!({"birth_year": birth_year}));
            let (status, Json(body)) = normalize_profile(&mut profile).unwrap_err();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{birth_year}");
            assert_eq!(body["error"], "만 14세 이상만 이용할 수 있어요.");
        }

        let mut profile = input(json!({"birth_year": year - 15}));
        normalize_profile(&mut profile).unwrap();
    }

    #[test]
    fn optional_answers_are_validated_and_newlywed_needs_married() {
        let mut ok = input(json!({"marital_status": "기혼", "newlywed": true,
            "children_count": 2, "income_median_band": "le100", "homeless_household": true}));
        normalize_profile(&mut ok).unwrap();
        assert_eq!(ok.marital_status.as_deref(), Some("married"));
        assert_eq!(ok.newlywed, Some(true));
        assert_eq!(ok.children_count, Some(2));
        assert_eq!(ok.income_median_band.as_deref(), Some("le100"));
        assert_eq!(ok.homeless_household, Some(true));

        let mut single = input(json!({"marital_status": "unmarried", "newlywed": true}));
        normalize_profile(&mut single).unwrap();
        assert_eq!(single.newlywed, None);

        let mut blank = input(json!({"marital_status": "", "income_median_band": " "}));
        normalize_profile(&mut blank).unwrap();
        assert_eq!(
            (blank.marital_status, blank.income_median_band),
            (None, None)
        );

        let mut unknown = input(json!({"income_median_band": "unknown"}));
        normalize_profile(&mut unknown).unwrap();
        assert_eq!(unknown.income_median_band.as_deref(), Some("unknown"));

        for bad in [
            json!({"marital_status": "divorced"}),
            json!({"income_median_band": "le70"}),
            json!({"children_count": 21}),
            json!({"children_count": -1}),
        ] {
            assert!(normalize_profile(&mut input(bad.clone())).is_err(), "{bad}");
        }
    }
}
