use super::*;
#[test]
fn probe_r2_bound() {
    let a = vec!["university_student", "student"];
    for (name, shown, quote) in [
        ("B9_kind_gwa", "대학생 10% 할인", "대학생 10% 적립과 5% 할인"),
        ("B10_freq_myeo", "대학생 매월 5,000원 적립", "대학생 매월 10% 할인되며 가입 시 1회 5,000원 적립"),
        ("B11_claim_negated", "대학생 무료 이용", "대학생은 무료 이용 대상에서 제외됩니다"),
        ("B11b_claim_negated2", "대학생 무료 이용", "대학생은 무료 이용이 종료되었습니다"),
        ("B12_claim_bare", "대학생 무료", "대학생 10% 할인, 배송비 무료"),
        ("B13_negated_unlisted", "대학생 10% 할인", "대학생은 10% 할인을 받으실 수 없습니다"),
        ("B14_range_pct", "대학생 20% 할인", "대학생 10%~20% 할인"),
        ("B15_amp", "대학생 5,000원 할인", "대학생 10% 할인 & 5,000원 적립"),
    ] {
        println!("R2BOUND {name} => {:?}", displayed_values_bound(shown, &[quote], &a));
    }
    for t in ["대학생 10%~20% 할인", "대학생 1만~2만원 할인", "대학생 5천원~1만원 할인", "대학생 10% ~ 20% 할인"] {
        println!("R2AGREE {t:?} => {}", super::super::resilience::extractors_agree(t));
    }
}
