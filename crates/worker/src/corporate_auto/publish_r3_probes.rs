use super::*;
#[test]
fn probe_r3_bound() {
    let a = vec!["university_student", "student"];
    let ay = vec!["university_student", "student", "youth"];
    for (name, shown, quote, aud) in [
        ("C0_control", "대학생 10% 할인", "대학생 10% 할인 혜택", a.clone()),
        ("C3_claim_wrong_audience", "청년 무료", "대학생 무료, 청년 10% 할인", ay.clone()),
        ("C4_neg_an_doem", "대학생 10% 할인", "대학생은 10% 할인이 적용 안 됩니다", a.clone()),
        ("C5_neg_mi", "대학생 10% 할인", "대학생 10% 할인 미적용", a.clone()),
        ("C5b_neg_aninm", "대학생 10% 할인", "대학생 10% 할인 대상 아님", a.clone()),
        ("C5c_neg_haji_anh", "대학생 10% 할인", "대학생에게 10% 할인을 제공하지 않습니다", a.clone()),
        ("C6_kind_doego", "대학생 10% 할인", "대학생은 10% 적립되고 5% 할인됩니다", a.clone()),
        ("C7_kind_e", "대학생 10% 할인", "대학생 10% 적립에 5% 할인까지", a.clone()),
        ("C8_threshold", "대학생 3,000원 할인", "대학생 3,000원 이상 구매 시 10% 할인", a.clone()),
        ("C9_cap_inline", "대학생 3,000원 할인", "대학생 최대 3,000원 10% 할인", a.clone()),
        ("C10_threshold_big", "대학생 50,000원 할인", "대학생 5만원 이상 구매 시 5,000원 할인", a.clone()),
        ("C11_range_eseo", "대학생 20% 할인", "대학생 10%에서 20% 할인", a.clone()),
        ("C12_range_naeji", "대학생 20% 할인", "대학생 10% 내지 20% 할인", a.clone()),
        ("C13_range_buteo", "대학생 2만원 할인", "대학생 1만원부터 2만원까지 할인", a.clone()),
        ("C14_range_fullwidth", "대학생 20% 할인", "대학생 10～20% 할인", a.clone()),
        ("C15_range_emdash", "대학생 20% 할인", "대학생 10—20% 할인", a.clone()),
        ("C16_up_to", "대학생 20% 할인", "대학생 최대 20% 할인", a.clone()),
        ("C17_other_product", "대학생 10% 할인", "대학생 노트북 구매 시 태블릿 10% 할인", a.clone()),
        ("C18_freq_month_once_same", "대학생 매월 5,000원 할인", "대학생 첫 달만 5,000원 할인 후 매월 1,000원 할인", a.clone()),
        ("C19_past_value", "대학생 10% 할인", "대학생 할인율이 10%에서 5%로 변경됩니다", a.clone()),
        ("C20_claim_first_month_paid", "대학생 첫 달 무료", "대학생 첫 달 무료 혜택은 2025년까지였습니다", a.clone()),
    ] {
        println!("R3BOUND {name} => {:?}", displayed_values_bound(shown, &[quote], &aud));
    }
    for t in ["대학생 10%에서 20% 할인", "대학생 10% 내지 20% 할인", "대학생 1만원부터 2만원까지 할인", "대학생 10～20% 할인", "대학생 10—20% 할인", "대학생 10 ~ 20 퍼센트 할인", "대학생 10%, 15%, 20% 단계별 할인"] {
        println!("R3AGREE {t:?} => {}", super::super::resilience::extractors_agree(t));
    }
}
