//! Interpret the retained API's application-method field, never page structure.
//! A contact mention or a portal URL is not proof of an application channel.
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Channel {
    pub id: &'static str,
    pub label: &'static str,
    /// Wire compatibility only, always `[]`. App bundles shipped before the
    /// 2026-11 copy cleanup render `channel.instructions.map(…)` and crash
    /// without the field. Drop it once that OTA has reached every install and
    /// no older runtime is served (pinned by
    /// `channels_still_send_an_empty_instructions_list`).
    pub instructions: [&'static str; 0],
    pub evidence: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Workflow {
    pub version: u8,
    pub channels: Vec<Channel>,
    pub needs_confirmation: bool,
    pub no_application_required: bool,
    pub method_text: Option<String>,
}
/// A channel and its label. The method field says how to apply; steps that read
/// the same for every programme ("신청 메뉴를 찾아주세요") are not said.
fn channel(id: &'static str, evidence: String) -> Channel {
    let label = match id {
        "online" => "온라인 접수",
        "mobile" => "기관 앱 접수",
        "visit" => "방문 접수",
        "postal" => "우편 접수",
        "email" => "이메일 접수",
        "phone" => "전화 접수",
        "fax" => "팩스 접수",
        "not_required" => "별도 신청 없음",
        _ => "접수 방식 확인",
    };
    Channel {
        id,
        label,
        instructions: [],
        evidence,
    }
}
fn compact(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}
fn enumerated(token: &str) -> Option<&'static str> {
    match compact(token).as_str() {
        "정부24온라인신청"
        | "기타온라인신청"
        | "온라인신청"
        | "온라인"
        | "인터넷"
        | "인터넷신청" => Some("online"),
        "방문" | "방문신청" | "방문접수" => Some("visit"),
        "우편" | "우편신청" | "우편접수" => Some("postal"),
        "이메일" | "전자우편" | "e-mail" | "email" => Some("email"),
        "전화" | "전화신청" | "전화접수" => Some("phone"),
        "팩스" | "fax" => Some("fax"),
        "모바일" | "모바일앱" | "앱" | "앱신청" => Some("mobile"),
        "신청불필요" | "별도신청없음" => Some("not_required"),
        // "직접입력" is an upstream field placeholder, not a user workflow.
        "직접입력" | "기타" | "해당없음" | "-" | "" => Some("unknown"),
        _ => None,
    }
}
fn negated(text: &str, marker: &str) -> bool {
    text.match_indices(marker).any(|(at, _)| {
        let after: String = text[at + marker.len()..].chars().take(10).collect();
        [
            "불가",
            "불가능",
            "불허",
            "미접수",
            "받지",
            "안됨",
            "안되",
            "않",
            "제외",
            "없이",
            "불필요",
            "중단",
            "불인정",
            "아님",
            "아니",
        ]
        .iter()
        .any(|word| after.contains(word))
    })
}
/// The method field split at its list separators.
fn method_pieces(method: &str) -> Vec<String> {
    method
        .replace("\\r", "\n")
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .split(['|', ',', ';', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
/// The field is only a list of channel codes ("기타 온라인신청||방문신청",
/// "방문, 인터넷"), with no procedure text.
pub fn enumerated_only(method: &str) -> bool {
    let pieces = method_pieces(method);
    pieces.iter().all(|s| enumerated(s).is_some())
}
impl Workflow {
    /// An application step on the web may exist: an application is needed
    /// and the method names an online channel or none at all (unknown). The
    /// API offers an application link only then, and the worker derives one
    /// only then; a visit/post/phone/fax/e-mail/app-only programme has none.
    pub fn accepts_online(&self) -> bool {
        !self.no_application_required
            && self
                .channels
                .iter()
                .any(|c| matches!(c.id, "online" | "unknown"))
    }
}

pub fn classify(method: Option<&str>) -> Workflow {
    let method = method.map(str::trim).filter(|s| !s.is_empty());
    let mut result = Workflow {
        version: 1,
        channels: vec![],
        needs_confirmation: false,
        no_application_required: false,
        method_text: method.map(str::to_owned),
    };
    let Some(method) = method else {
        result.channels.push(channel("unknown", String::new()));
        result.needs_confirmation = true;
        return result;
    };
    let pieces = method_pieces(method);
    let exact = pieces.iter().all(|s| enumerated(s).is_some());
    let mut add = |id, evidence: &str| {
        if !result.channels.iter().any(|c| c.id == id) {
            result
                .channels
                .push(channel(id, evidence.chars().take(500).collect()));
        }
    };
    if exact {
        for piece in &pieces {
            add(enumerated(piece).unwrap_or("unknown"), piece);
        }
    } else {
        for piece in &pieces {
            let text = compact(piece);
            if let Some(id) = enumerated(piece) {
                add(id, piece);
                continue;
            }
            let none = [
                "신청불필요",
                "별도신청절차없",
                "별도신청없이",
                "별도신청절차없이",
            ];
            if none
                .iter()
                .any(|word| text.contains(word) && !negated(&text, word))
            {
                add("not_required", piece);
            }
            let action = ["신청", "접수", "제출"]
                .iter()
                .any(|word| text.contains(word));
            let contact_only = text.contains("문의")
                && !text.contains("신청방법")
                && !text.contains("접수방법")
                && !text.contains("접수처");
            for (id, markers) in [
                ("email", &["이메일", "전자우편", "e-mail", "email"][..]),
                ("visit", &["방문", "현장접수"][..]),
                ("postal", &["우편"][..]),
                ("phone", &["전화", "유선"][..]),
                ("fax", &["팩스", "fax"][..]),
                ("mobile", &["모바일", "앱에서", "앱신청", "앱접수"][..]),
                (
                    "online",
                    &["온라인", "인터넷", "홈페이지에서신청", "홈페이지신청"][..],
                ),
            ] {
                // Email described as "온라인 접수(이메일...)" is not a web form.
                if id == "online"
                    && ["이메일", "e-mail", "email", "전자우편"]
                        .iter()
                        .any(|s| text.contains(s))
                {
                    continue;
                }
                if id == "postal" && text.contains("전자우편") {
                    continue;
                }
                let listed_channel = ["신청방법", "접수방법"]
                    .iter()
                    .any(|heading| method.contains(heading))
                    && markers.iter().any(|marker| {
                        let start = text.trim_start_matches(['-', '*', '○', '·', '•']);
                        start.starts_with(&format!("{marker}:"))
                            || start.starts_with(&format!("{marker}："))
                    });
                if (action || listed_channel)
                    && !contact_only
                    && markers
                        .iter()
                        .any(|m| text.contains(m) && !negated(&text, m))
                {
                    add(id, piece);
                }
            }
        }
        result.needs_confirmation = true;
    }
    if result.channels.is_empty() {
        result
            .channels
            .push(channel("unknown", method.chars().take(500).collect()));
    }
    let has_none = result.channels.iter().any(|c| c.id == "not_required");
    if has_none && result.channels.len() > 1 {
        // Contradictory alternatives must never suppress an actual application.
        result.channels.retain(|c| c.id != "not_required");
        result.needs_confirmation = true;
        if !result.channels.iter().any(|c| c.id == "unknown") {
            result.channels.push(channel(
                "unknown",
                "별도 신청 여부가 다른 안내와 함께 기재되어 있어요.".into(),
            ));
        }
    }
    result.no_application_required =
        result.channels.len() == 1 && result.channels[0].id == "not_required";
    result.needs_confirmation |= result.channels.iter().any(|c| c.id == "unknown");
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ids(text: &str) -> Vec<&'static str> {
        classify(Some(text)).channels.iter().map(|c| c.id).collect()
    }
    #[test]
    fn only_online_or_unknown_methods_accept_a_web_step() {
        // Production method texts (2026-10-31 sample).
        for online in [
            "정부24온라인신청||방문신청",
            "기타 온라인신청",
            "방문, 인터넷",
            "청년몽땅정보통(youth.seoul.go.kr) 온라인",
            "직접입력",
            "",
        ] {
            assert!(classify(Some(online)).accepts_online(), "{online}");
        }
        assert!(classify(None).accepts_online());
        for offline in [
            "방문신청",
            "방문, 전화",
            "전화",
            "방문, 우편",
            "방문, E-mail",
            "신청불필요",
            "모바일",
        ] {
            assert!(!classify(Some(offline)).accepts_online(), "{offline}");
        }
    }

    #[test]
    fn source_enums_are_not_web_form_assumptions() {
        assert_eq!(ids("정부24온라인신청||방문신청"), vec!["online", "visit"]);
        assert_eq!(ids("방문, E-mail"), vec!["visit", "email"]);
        assert_eq!(ids("직접입력"), vec!["unknown"]);
        assert_eq!(ids("해당 없음"), vec!["unknown"]);
        assert!(classify(Some("신청불필요")).no_application_required);
        assert!(!classify(Some("신청불필요||방문신청")).no_application_required);
        assert_eq!(ids("인터넷, 모바일"), vec!["online", "mobile"]);
    }
    #[test]
    fn contacts_negations_and_mail_are_not_online_applications() {
        assert_eq!(ids("○ 신청방법\n- 방문: 청년라운지 이스트 및 웨스트\n- 온라인: 홈페이지 > 네이버폼 제출"),vec!["visit","online"]);
        assert_eq!(
            ids("○ 신청방법\n- 방문: 접수 불가\n- 온라인: 홈페이지에서 신청"),
            vec!["online"]
        );
        assert_eq!(ids("신청 문의는 이메일 또는 전화"), vec!["unknown"]);
        assert_eq!(ids("온라인 신청 불가, 방문신청"), vec!["visit"]);
        assert_eq!(
            ids("온라인 접수(이메일 test@example.org) 제출"),
            vec!["email"]
        );
        assert_eq!(ids("방문 없이 온라인 신청"), vec!["online"]);
        assert_eq!(ids("우편 접수는 받지 않습니다"), vec!["unknown"]);
        assert_eq!(ids("전자우편 제출"), vec!["email"]);
        assert_eq!(ids("별도 신청 절차 없이 참여 가능"), vec!["not_required"]);
        assert_eq!(ids("https://provider.example.org/apply"), vec!["unknown"]);
        assert_eq!(ids("신청서를 제출하면 자동 지급"), vec!["unknown"]);
        assert_eq!(ids("신청 불필요가 아님"), vec!["unknown"]);
    }
    #[test]
    fn channels_still_send_an_empty_instructions_list() {
        // Older app bundles read `channel.instructions`; the key must stay.
        for channel in classify(Some("온라인 또는 방문 신청")).channels {
            let wire = serde_json::to_value(&channel).unwrap();
            assert_eq!(wire["instructions"], serde_json::json!([]), "{wire}");
        }
    }
    #[test]
    fn channels_preserve_evidence_and_do_not_grant_submission() {
        let flow = classify(Some("이메일 또는 방문 신청"));
        assert_eq!(flow.channels.len(), 2);
        assert!(flow.needs_confirmation);
        assert!(flow
            .channels
            .iter()
            .all(|c| c.evidence == "이메일 또는 방문 신청"));
        assert_eq!(classify(None).channels[0].id, "unknown");
    }
}
