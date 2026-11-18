//! `add_to_calendar`: the date an event would mark, read from our data.
//!
//! The server proposes the event (title, all-day date, source quote) on a
//! confirmation card; after approval the device writes it to its own
//! calendar (expo-calendar). Nothing about the user goes into the event:
//! the title is the programme's, the note is fixed text.
//!
//! Dates: the deadline (`application_end_at`, else `deadline_at`) and the
//! start (`application_start_at`) come from the programme row; an
//! announcement or interview date comes from exactly one date in the
//! notice sentences that name it. No date, a past date or several
//! candidate dates: no card.
use super::{tools::tool_error, ToolError};
use chrono::{Datelike, NaiveDate, Utc};
use mazimi_core::{agent::CalendarDate, models::Program, notice_dates::find_dates};
use sqlx::PgConnection;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub date: NaiveDate,
    pub event_title: String,
    /// The sentence the date was read from, or the record line.
    pub quote: String,
    pub source_label: String,
}

/// Fixed note on every event; no user data.
pub const EVENT_NOTE: &str =
    "마지미에서 추가한 일정이에요. 일정은 바뀔 수 있으니 공식 공고에서 다시 확인해 주세요.";

pub(crate) fn kst_today() -> NaiveDate {
    super::official_page::kst_date(Utc::now())
}

fn korean(date: NaiveDate) -> String {
    format!("{}. {}. {}.", date.year(), date.month(), date.day())
}

fn keywords(kind: CalendarDate) -> &'static [&'static str] {
    match kind {
        CalendarDate::Deadline => &[
            "마감",
            "접수기간",
            "접수 기간",
            "신청기간",
            "신청 기간",
            "기한",
        ],
        CalendarDate::ApplicationStart => {
            &["시작", "접수기간", "접수 기간", "신청기간", "신청 기간"]
        }
        CalendarDate::Announcement => &["발표", "결과 안내", "선정 결과", "합격자"],
        CalendarDate::Interview => &["면접"],
    }
}

/// Raw notice fields worth reading for dates (the same provenance the
/// grounded answers quote). The passage index splits `2026. 3. 3.` into
/// sentences, so dates are read from the whole field here.
const RAW_DATE_FIELDS: &[(&str, &str)] = &[
    ("aplyYmd", "신청 기간"),
    ("신청기한", "신청 기한"),
    ("plcyAplyMthdCn", "신청방법"),
    ("srngMthdCn", "심사방법"),
    ("etcMttrCn", "기타 사항"),
    ("plcyExplnCn", "정책 설명"),
    ("신청방법", "신청방법"),
];

fn notice_texts(program: &Program) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = [
        (program.application_method.as_deref(), "신청방법"),
        (program.screening_method.as_deref(), "심사방법"),
        (program.summary.as_deref(), "요약"),
    ]
    .into_iter()
    .filter_map(|(text, label)| text.map(|t| (t.to_owned(), label)))
    .collect();
    if let Some(raw) = program.raw_payload.as_ref().filter(|r| r.is_object()) {
        for (key, label) in RAW_DATE_FIELDS {
            if let Some(text) = raw[*key].as_str() {
                out.push((text.to_owned(), label));
            }
        }
    }
    out
}

/// Windows after each keyword of `kind` in the notice texts: (window, label).
fn keyword_windows(program: &Program, kind: CalendarDate) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    for (text, label) in notice_texts(program) {
        let chars: Vec<char> = text.chars().collect();
        for keyword in keywords(kind) {
            let needle: Vec<char> = keyword.chars().collect();
            for at in 0..chars.len().saturating_sub(needle.len() - 1) {
                if chars[at..at + needle.len()] == needle[..] {
                    let from = at.saturating_sub(20);
                    let to = (at + needle.len() + 60).min(chars.len());
                    out.push((chars[from..to].iter().collect(), label));
                }
            }
        }
    }
    out
}

/// The event a programme's date would become, from the programme row and
/// its notice text. Pure; `today` is the Korean date.
pub fn propose(
    program: &Program,
    kind: CalendarDate,
    today: NaiveDate,
) -> Result<Proposal, ToolError> {
    propose_before(program, kind, 0, today)
}

/// [`propose`] with the event `days_before` days ahead of the date
/// ("마감 하루 전"). The quote still names the source date; the title says
/// the offset so the event is not mistaken for the deadline itself.
pub fn propose_before(
    program: &Program,
    kind: CalendarDate,
    days_before: u8,
    today: NaiveDate,
) -> Result<Proposal, ToolError> {
    let windows = keyword_windows(program, kind);
    let mentions = |date: NaiveDate| {
        windows
            .iter()
            .find(|(text, _)| find_dates(text, date.year()).iter().any(|d| d.date == date))
    };
    let recorded = match kind {
        CalendarDate::Deadline => program
            .application_end_at
            .or(program.deadline_at)
            .map(super::official_page::kst_date),
        CalendarDate::ApplicationStart => program
            .application_start_at
            .map(super::official_page::kst_date),
        CalendarDate::Announcement | CalendarDate::Interview => None,
    };
    let (date, quote, source_label) = match (kind, recorded) {
        (CalendarDate::Deadline | CalendarDate::ApplicationStart, Some(date)) => {
            match mentions(date) {
                Some((text, label)) => (date, text.clone(), (*label).to_owned()),
                None => (
                    date,
                    format!("{}: {}", kind.label(), korean(date)),
                    "마지미 공고 데이터".to_owned(),
                ),
            }
        }
        (CalendarDate::Deadline | CalendarDate::ApplicationStart, None) => {
            return Err(tool_error(
                "date_unknown",
                &format!(
                "공고 데이터에 {} 날짜가 없어 캘린더에 넣을 수 없어요. 공식 공고를 확인해 주세요.",
                kind.label()
            ),
            ))
        }
        (_, _) => {
            let mut found: Vec<(NaiveDate, &String, &str)> = Vec::new();
            for (text, label) in &windows {
                for d in find_dates(text, today.year()) {
                    if !found.iter().any(|(date, ..)| *date == d.date) {
                        found.push((d.date, text, label));
                    }
                }
            }
            match found.as_slice() {
                [(date, text, label)] => (*date, (*text).clone(), (*label).to_owned()),
                [] => {
                    return Err(tool_error(
                        "date_unknown",
                        &format!("공고에서 {} 날짜를 찾지 못해 캘린더에 넣을 수 없어요.", kind.label()),
                    ))
                }
                _ => {
                    return Err(tool_error(
                        "date_ambiguous",
                        &format!("공고에 {} 날짜가 여러 개라 하나로 정할 수 없어요. 공식 공고를 확인해 주세요.", kind.label()),
                    ))
                }
            }
        }
    };
    if date < today {
        return Err(tool_error(
            "date_past",
            &format!("{} 날짜({})가 이미 지났어요.", kind.label(), korean(date)),
        ));
    }
    let days_before = days_before.min(mazimi_core::agent::MAX_DAYS_BEFORE);
    let date = date - chrono::Days::new(u64::from(days_before));
    if date < today {
        return Err(tool_error(
            "date_past",
            &format!(
                "{} {}일 전({})은 이미 지났어요.",
                kind.label(),
                days_before,
                korean(date)
            ),
        ));
    }
    let label = match days_before {
        0 => kind.label().to_owned(),
        n => format!("{} {n}일 전", kind.label()),
    };
    let title: String = program.title.chars().take(40).collect();
    Ok(Proposal {
        date,
        event_title: format!("[{label}] {title}"),
        quote: mazimi_core::official_page::sanitize_quote(&quote),
        source_label,
    })
}

/// Whether `add_to_calendar` would card this programme's deadline today:
/// the same reading [`resolve`] makes (an active row, one date not past).
/// The answer's calendar button is offered on exactly this.
pub fn offers(program: &Program, today: NaiveDate) -> bool {
    program.is_active && propose(program, CalendarDate::Deadline, today).is_ok()
}

/// [`propose`] for an active programme. Not the "currently available"
/// filter: a future start makes a programme unavailable, and interview or
/// announcement dates matter after the deadline (code review H4). The
/// date itself must still be today or later.
pub async fn resolve(
    connection: &mut PgConnection,
    program: Uuid,
    kind: CalendarDate,
    days_before: u8,
) -> Result<Proposal, ToolError> {
    let row: Option<Program> = sqlx::query_as("SELECT * FROM programs WHERE id=$1")
        .bind(program)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, "calendar date query failed");
            tool_error("tool_failed", "현재 상태를 확인하지 못했어요.")
        })?;
    let program = row.filter(|p| p.is_active).ok_or_else(|| {
        tool_error(
            "program_unavailable",
            "지금 안내할 수 있는 혜택이 아니에요. 공고 상태를 확인해 주세요.",
        )
    })?;
    propose_before(&program, kind, days_before, kst_today())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn program(end: Option<(u32, u32)>, method: &str) -> Program {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(), "program_type": "policy", "source_type": "manual",
            "title": "청년 면접 지원", "program_status": "active", "is_active": true,
            "created_at": Utc::now(), "updated_at": Utc::now(),
            "application_method": method,
            "application_end_at": end.map(|(m, d)| Utc.with_ymd_and_hms(2026, m, d, 14, 59, 0).unwrap()),
        }))
        .unwrap()
    }
    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()
    }

    #[test]
    fn deadline_from_the_record_with_the_notice_sentence_as_source() {
        let p = program(
            Some((11, 27)),
            "접수기간: 2026. 3. 3.(화) ~ 11. 27.(금) 온라인 접수",
        );
        let proposal = propose(&p, CalendarDate::Deadline, today()).unwrap();
        assert_eq!(
            proposal.date,
            NaiveDate::from_ymd_opt(2026, 11, 27).unwrap()
        );
        assert_eq!(proposal.event_title, "[신청 마감] 청년 면접 지원");
        assert!(proposal.quote.contains("11. 27."), "{}", proposal.quote);
        // No sentence names the date: the record line is the source.
        let bare = propose(
            &program(Some((11, 27)), "온라인 접수"),
            CalendarDate::Deadline,
            today(),
        )
        .unwrap();
        assert_eq!(bare.quote, "신청 마감: 2026. 11. 27.");
        assert_eq!(bare.source_label, "마지미 공고 데이터");
    }

    #[test]
    fn no_card_without_one_future_date() {
        assert_eq!(
            propose(
                &program(None, "온라인 접수"),
                CalendarDate::Deadline,
                today()
            )
            .unwrap_err()
            .code,
            "date_unknown"
        );
        assert_eq!(
            propose(&program(Some((9, 1)), "x"), CalendarDate::Deadline, today())
                .unwrap_err()
                .code,
            "date_past"
        );
        let interview = program(Some((11, 27)), "서류 심사 후 면접 2026. 12. 3.(목) 진행");
        assert_eq!(
            propose(&interview, CalendarDate::Interview, today())
                .unwrap()
                .date,
            NaiveDate::from_ymd_opt(2026, 12, 3).unwrap()
        );
        let two = program(
            Some((11, 27)),
            "1차 면접 2026. 12. 3. 2차 면접 2026. 12. 10.",
        );
        assert_eq!(
            propose(&two, CalendarDate::Interview, today())
                .unwrap_err()
                .code,
            "date_ambiguous"
        );
        assert_eq!(
            propose(&interview, CalendarDate::Announcement, today())
                .unwrap_err()
                .code,
            "date_unknown"
        );
    }

    /// Review LOW: "하루 전" moves the event one day before the date.
    #[test]
    fn offsets_move_the_event_before_the_date() {
        let p = program(Some((11, 27)), "온라인 접수");
        let proposal = propose_before(&p, CalendarDate::Deadline, 1, today()).unwrap();
        assert_eq!(
            proposal.date,
            NaiveDate::from_ymd_opt(2026, 11, 26).unwrap()
        );
        assert_eq!(proposal.event_title, "[신청 마감 1일 전] 청년 면접 지원");
        assert_eq!(
            propose_before(
                &program(Some((10, 17)), "x"),
                CalendarDate::Deadline,
                3,
                today()
            )
            .unwrap_err()
            .code,
            "date_past"
        );
    }
}
