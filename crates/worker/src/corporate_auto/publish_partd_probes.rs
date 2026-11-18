use super::*;
#[test]
fn probe_bound() {
    for (name, shown, quotes, aud) in [
        ("B0_control", "대학생 10% 할인", vec!["대학생 10% 할인 혜택"], vec!["university_student","student"]),
        ("B1_freq_crossbind", "대학생 매월 5,000원 적립", vec!["대학생 매월 10% 할인, 가입 시 1회 5,000원 적립"], vec!["university_student","student"]),
        ("B2_kind_crossbind_won", "대학생 5,000원 할인", vec!["대학생 10% 할인 및 5,000원 적립"], vec!["university_student","student"]),
        ("B2b_kind_crossbind_pct", "대학생 10% 할인", vec!["대학생 10% 적립 및 5% 할인"], vec!["university_student","student"]),
        ("B3_date_reads_monthly", "대학생 매월 10% 할인", vec!["대학생 10월 1일부터 신규 가입 시 10% 할인"], vec!["university_student","student"]),
        ("B3b_date_reads_monthly_won", "대학생 월 5,000원 할인", vec!["대학생 가입 시 1회 5,000원 할인 (11월 3일까지)"], vec!["university_student","student"]),
        ("B4_negated", "대학생 10% 할인", vec!["대학생은 10% 할인 대상에서 제외됩니다"], vec!["university_student","student"]),
        ("B5_claim_words_split", "대학생 첫 달 무료", vec!["대학생 첫 달 10% 할인, 배송비 무료"], vec!["university_student","student"]),
        ("B6_claim_lifetime", "대학생 평생 10% 할인", vec!["대학생 10% 할인 (평생 1회)"], vec!["university_student","student"]),
        ("B7_youth_vs_student", "청년 10% 할인", vec!["대학생 및 청년 10% 할인"], vec!["youth","university_student","student"]),
        ("B8_two_audience_values", "대학생 20% 할인", vec!["대학생 10% 할인, 청소년 20% 할인"], vec!["university_student","student","teen"]),
    ] {
        println!("BOUND {name} => {:?}", displayed_values_bound(shown, &quotes, &aud));
    }
}
