//! Worker side of the AI benefit agent (docs/mazimi-ai-agent.md):
//!
//! - [`purge`] (always on): conversation retention, overdue confirmations,
//!   runs orphaned by an API restart, outbox and delivery bookkeeping.
//! - Proactive messages (`ASSISTANT_AGENT_PROACTIVE_ENABLED`, default off,
//!   and per-user opt-in in `assistant_proactive_settings`): weekly action
//!   briefing, change impact on saved programmes, resume a stalled
//!   preparation. Only 09:00-20:59 KST, at most one per user per KST day,
//!   only to users whose push channel is on.
//! - [`drain_outbox`]: pushes for agent runs that finished after the client
//!   stopped waiting; same quiet hours, dropped after 6 hours.
//!
//! Functions reserve their delivery rows before returning messages, so a
//! crash between reserving and sending loses a message rather than sending
//! it twice (the deadline alerts make the same choice).
use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, Timelike, Utc, Weekday};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

pub const DEFAULT_RETENTION_DAYS: u32 = 30;
/// Proactive messages per user per KST day, across kinds.
pub const DAILY_PROACTIVE_CAP: i64 = 1;
const WINDOW_HOURS_KST: std::ops::RangeInclusive<u32> = 9..=20;

/// `ASSISTANT_RETENTION_DAYS` (1..=90, default 30).
pub fn retention_days() -> u32 {
    std::env::var("ASSISTANT_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|d| (1..=90).contains(d))
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

pub fn proactive_enabled() -> bool {
    std::env::var("ASSISTANT_AGENT_PROACTIVE_ENABLED").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        )
    })
}

fn kst() -> FixedOffset {
    FixedOffset::east_opt(9 * 3600).expect("valid KST offset")
}

pub fn kst_date(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&kst()).date_naive()
}

/// 09:00-20:59 KST, the only hours a proactive or background push leaves.
pub fn within_window(now: DateTime<Utc>) -> bool {
    WINDOW_HOURS_KST.contains(&now.with_timezone(&kst()).hour())
}

/// A message ready for the push dispatcher.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub user_id: Uuid,
    pub program_id: Option<Uuid>,
    pub kind: &'static str,
    pub title: String,
    pub body: String,
    pub expo_tokens: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PurgeSummary {
    pub confirmations_expired: i64,
    pub runs_abandoned: u64,
    pub conversations_deleted: u64,
    pub outbox_dropped: u64,
    pub bookkeeping_deleted: u64,
}

pub async fn purge(pool: &PgPool, retention_days: u32) -> Result<PurgeSummary> {
    let confirmations_expired: i32 =
        sqlx::query_scalar("SELECT assistant_expire_confirmations(NULL)")
            .fetch_one(pool)
            .await?;
    // A run whose task died with an API process never finishes by itself.
    let runs_abandoned = sqlx::query(
        "UPDATE assistant_runs SET status='failed', error_code='abandoned', updated_at=now(), completed_at=now()
         WHERE status IN ('received','planning','tool_running','executing','validating_response')
           AND updated_at < now() - interval '1 hour'",
    )
    .execute(pool)
    .await?
    .rows_affected();
    let conversations_deleted = sqlx::query(
        "DELETE FROM assistant_conversations WHERE updated_at < now() - make_interval(days => $1)",
    )
    .bind(retention_days as i32)
    .execute(pool)
    .await?
    .rows_affected();
    let outbox_dropped = sqlx::query(
        "UPDATE assistant_notification_outbox SET status='dropped'
         WHERE status='pending' AND created_at < now() - interval '6 hours'",
    )
    .execute(pool)
    .await?
    .rows_affected();
    let mut bookkeeping_deleted = 0;
    for sql in [
        "DELETE FROM assistant_notification_outbox WHERE status <> 'pending' AND created_at < now() - interval '7 days'",
        "DELETE FROM assistant_proactive_deliveries WHERE created_at < now() - interval '120 days'",
        "DELETE FROM assistant_action_audit WHERE created_at < now() - interval '365 days'",
        "DELETE FROM assistant_program_fingerprints f WHERE NOT EXISTS (
            SELECT 1 FROM assistant_proactive_settings s WHERE s.user_id=f.user_id AND s.change_alerts)",
    ] {
        bookkeeping_deleted += sqlx::query(sql).execute(pool).await?.rows_affected();
    }
    Ok(PurgeSummary {
        confirmations_expired: i64::from(confirmations_expired),
        runs_abandoned,
        conversations_deleted,
        outbox_dropped,
        bookkeeping_deleted,
    })
}

/// Push tokens of a user whose push channel is on (missing preferences
/// default to on, as for deadline alerts).
async fn push_tokens(pool: &PgPool, user: Uuid) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar::<_, Vec<String>>(
        "SELECT CASE WHEN COALESCE(pref.channel_push, true) THEN ARRAY(
                SELECT pt.token FROM push_tokens pt WHERE pt.user_id=u.id ORDER BY pt.updated_at DESC)
            ELSE ARRAY[]::text[] END
         FROM users u LEFT JOIN notification_preferences pref ON pref.user_id=u.id WHERE u.id=$1",
    )
    .bind(user)
    .fetch_optional(pool)
    .await?
    .unwrap_or_default())
}

/// Reserve one proactive delivery: unique per (user, kind, key) and at most
/// [`DAILY_PROACTIVE_CAP`] per user per KST day. Serialized per user.
async fn reserve(
    pool: &PgPool,
    user: Uuid,
    kind: &str,
    program: Option<Uuid>,
    key: &str,
    day: NaiveDate,
) -> Result<bool> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR NO KEY UPDATE")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    let today: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM assistant_proactive_deliveries WHERE user_id=$1 AND kst_date=$2 AND kind <> 'run_done'",
    )
    .bind(user)
    .bind(day)
    .fetch_one(&mut *tx)
    .await?;
    if today >= DAILY_PROACTIVE_CAP {
        return Ok(false);
    }
    let inserted = sqlx::query(
        "INSERT INTO assistant_proactive_deliveries(user_id, kind, program_id, dedupe_key, kst_date)
         VALUES($1,$2,$3,$4,$5) ON CONFLICT (user_id, kind, dedupe_key) DO NOTHING",
    )
    .bind(user)
    .bind(kind)
    .bind(program)
    .bind(key)
    .bind(day)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(inserted == 1)
}

const SAVED: &str = "(EXISTS(SELECT 1 FROM user_bookmarks b WHERE b.user_id=$1 AND b.program_id=p.id)
    OR EXISTS(SELECT 1 FROM application_preparations a WHERE a.user_id=$1 AND a.program_id=p.id)
    OR EXISTS(SELECT 1 FROM alert_subscriptions s WHERE s.user_id=$1 AND s.program_id=p.id AND s.enabled))";
const NOT_SETTLED: &str = "NOT EXISTS(SELECT 1 FROM user_program_states u WHERE u.user_id=$1 AND u.program_id=p.id
    AND u.state IN ('applied','waiting','received','abandoned'))
    AND NOT EXISTS(SELECT 1 FROM application_journeys j WHERE j.user_id=$1 AND j.program_id=p.id AND j.outcome='reported_applied')";

async fn opted_in(pool: &PgPool, column: &str) -> Result<Vec<Uuid>> {
    // `column` is one of three fixed names below, never input.
    Ok(sqlx::query_scalar(&format!(
        "SELECT user_id FROM assistant_proactive_settings WHERE {column} ORDER BY user_id"
    ))
    .fetch_all(pool)
    .await?)
}

/// Monday from 10:00 KST: this week's deadlines and preparations, counts
/// only (no titles on the lock screen).
pub async fn weekly_briefings(pool: &PgPool, now: DateTime<Utc>) -> Result<Vec<Outgoing>> {
    let local = now.with_timezone(&kst());
    if !within_window(now) || local.weekday() != Weekday::Mon || local.hour() < 10 {
        return Ok(Vec::new());
    }
    let week = format!(
        "{}-W{:02}",
        local.iso_week().year(),
        local.iso_week().week()
    );
    let available = mazimi_core::currently_available_sql("p.", "$4");
    // "이번 주 마감": the Korean days of this week, as the assistant reads
    // the same words (mazimi_core::calendar), not a rolling seven days.
    let (from, to) = mazimi_core::calendar::rest_of_week(kst_date(now));
    let deadline = mazimi_core::deadline_date_sql("p");
    let mut out = Vec::new();
    for user in opted_in(pool, "weekly_briefing").await? {
        let (due, preparing): (i64, i64) = sqlx::query_as(&format!(
            "SELECT count(*) FILTER (WHERE {deadline} BETWEEN $2 AND $3),
                    count(*) FILTER (WHERE EXISTS(SELECT 1 FROM application_preparations a WHERE a.user_id=$1 AND a.program_id=p.id))
             FROM current_programs p WHERE {available} AND {SAVED} AND {NOT_SETTLED}"
        ))
        .bind(user)
        .bind(from)
        .bind(to)
        .bind(now)
        .fetch_one(pool)
        .await?;
        if due == 0 && preparing == 0 {
            continue;
        }
        let tokens = push_tokens(pool, user).await?;
        if tokens.is_empty()
            || !reserve(pool, user, "weekly_briefing", None, &week, kst_date(now)).await?
        {
            continue;
        }
        out.push(Outgoing {
            user_id: user,
            program_id: None,
            kind: "weekly_briefing",
            title: "이번 주 혜택 할 일".into(),
            body: format!(
                "이번 주 마감 {due}개, 준비 중 {preparing}개예요. 비서 탭에서 할 일을 확인해 보세요."
            ),
            expo_tokens: tokens,
        });
    }
    Ok(out)
}

/// What a saved programme looked like: the fields a change of which needs
/// the user's attention. `conditions` (conditions and documents text in one
/// hash) is the first format, kept so stored fingerprints still compare;
/// `conditions_only` and `submission_documents` hash them apart, so a
/// documents text that appears where none was stored can be told from a
/// change. `documents_side_data`: the text comes from a side read that can
/// fill it in later without the notice changing (보조금24 serviceDetail; see
/// [`changed_fields`]).
const FINGERPRINT_SQL: &str = "jsonb_build_object(
    'deadline', jsonb_build_array(p.deadline_at, p.application_start_at, p.application_end_at, p.benefit_valid_until),
    'amount', CASE WHEN p.program_type = 'corporate_benefit' THEN jsonb_build_array(NULL, NULL, NULL)
                   ELSE jsonb_build_array(p.benefit_amount_monthly, p.benefit_amount_semester, p.benefit_amount_once) END,
    'conditions', md5(COALESCE(p.normalized_payload->>'conditions','') || '|' || COALESCE(p.submission_documents,'')),
    'conditions_only', md5(COALESCE(p.normalized_payload->>'conditions','') || '|'),
    'submission_documents', md5(p.submission_documents),
    'documents_side_data', p.source_type = 'gov_benefits',
    'documents', COALESCE((SELECT jsonb_agg(jsonb_build_array(d.document_name, d.is_required) ORDER BY d.document_name)
                           FROM program_documents d WHERE d.program_id=p.id), '[]'::jsonb),
    'rules', (SELECT max(r.version) FROM eligibility_rules r WHERE r.program_id=p.id),
    'status', p.program_status)";

fn changed_fields(before: &Value, after: &Value) -> Vec<&'static str> {
    let mut fields: Vec<&'static str> = [
        ("deadline", "마감일"),
        ("rules", "자격 조건"),
        ("documents", "제출 서류"),
        ("amount", "지원 금액"),
        ("status", "공고 상태"),
    ]
    .into_iter()
    .filter(|(key, _)| before.get(*key) != after.get(*key))
    .map(|(_, label)| label)
    .collect();
    if conditions_changed(before, after) {
        // After 마감일, where the first format listed it.
        let at = usize::from(fields.first() == Some(&"마감일"));
        fields.insert(at, "지원 조건");
    }
    if documents_text_changed(before, after) && !fields.contains(&"제출 서류") {
        fields.push("제출 서류");
    }
    fields
}

/// Whether the documents text changed. Where it is side data
/// (`documents_side_data`), a text appearing where none was stored is a
/// first observation, not a change of the notice: rows filled in after the
/// fact must not tell every saver their programme changed. Elsewhere a text
/// appearing is a change. A first-format fingerprint hashed the text into
/// `conditions` (absent as ""): equal to the current conditions alone and
/// not to the current joint hash means the text appeared.
fn documents_text_changed(before: &Value, after: &Value) -> bool {
    let side = after.get("documents_side_data") == Some(&Value::Bool(true));
    let now = after.get("submission_documents").unwrap_or(&Value::Null);
    match before.get("submission_documents") {
        Some(stored) => stored != now && !(stored.is_null() && side),
        None => {
            let joint = before.get("conditions");
            !side && joint != after.get("conditions") && joint == after.get("conditions_only")
        }
    }
}

/// Whether the conditions text changed. A fingerprint of the first format
/// has only the joint hash, with an absent documents text hashed as "": it
/// is unchanged when it equals either the current joint hash or the current
/// conditions alone (the documents text appeared, the conditions did not
/// move).
fn conditions_changed(before: &Value, after: &Value) -> bool {
    match before.get("conditions_only") {
        Some(conditions) => Some(conditions) != after.get("conditions_only"),
        None => {
            let joint = before.get("conditions");
            joint != after.get("conditions") && joint != after.get("conditions_only")
        }
    }
}

async fn save_fingerprint(
    pool: &PgPool,
    user: Uuid,
    program: Uuid,
    fingerprint: &Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO assistant_program_fingerprints(user_id, program_id, fingerprint) VALUES($1,$2,$3)
         ON CONFLICT (user_id, program_id) DO UPDATE SET fingerprint=$3, observed_at=now()",
    )
    .bind(user)
    .bind(program)
    .bind(fingerprint)
    .execute(pool)
    .await?;
    Ok(())
}

/// Saved programmes whose deadline, conditions, documents, amounts or
/// status changed since the opted-in user's last observation. The first
/// observation only records a baseline.
pub async fn change_impacts(pool: &PgPool, now: DateTime<Utc>) -> Result<Vec<Outgoing>> {
    if !within_window(now) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for user in opted_in(pool, "change_alerts").await? {
        let rows: Vec<(Uuid, String, Value, Option<Value>)> = sqlx::query_as(&format!(
            "SELECT p.id, p.title, {FINGERPRINT_SQL}, f.fingerprint
             FROM programs p LEFT JOIN assistant_program_fingerprints f ON f.user_id=$1 AND f.program_id=p.id
             WHERE {SAVED} AND {NOT_SETTLED} ORDER BY p.id LIMIT 50"
        ))
        .bind(user)
        .fetch_all(pool)
        .await?;
        let mut notified = false;
        for (program, _title, current, previous) in rows {
            let Some(previous) = previous else {
                // First observation: a baseline, never an alert.
                save_fingerprint(pool, user, program, &current).await?;
                continue;
            };
            let fields = changed_fields(&previous, &current);
            if fields.is_empty() {
                // Nothing to tell, but the stored form may be older or lack
                // a documents text: keep the current one as the baseline.
                if previous != current {
                    save_fingerprint(pool, user, program, &current).await?;
                }
                continue;
            }
            let digest = format!(
                "{program}:{:x}",
                <sha2::Sha256 as sha2::Digest>::digest(format!("{program}:{current}").as_bytes())
            );
            let already: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM assistant_proactive_deliveries
                   WHERE user_id=$1 AND kind='change_impact' AND dedupe_key=$2)",
            )
            .bind(user)
            .bind(&digest)
            .fetch_one(pool)
            .await?;
            if already {
                save_fingerprint(pool, user, program, &current).await?;
                continue;
            }
            // Keep the old fingerprint until the change is delivered: a
            // second change in the same tick, a full daily cap or a muted
            // push channel retries on a later tick instead of being lost.
            if notified {
                continue;
            }
            let tokens = push_tokens(pool, user).await?;
            if tokens.is_empty()
                || !reserve(
                    pool,
                    user,
                    "change_impact",
                    Some(program),
                    &digest,
                    kst_date(now),
                )
                .await?
            {
                continue;
            }
            save_fingerprint(pool, user, program, &current).await?;
            notified = true;
            // No title on the lock screen: a benefit name can reveal a
            // sensitive situation. The app shows which one changed.
            out.push(Outgoing {
                user_id: user,
                program_id: Some(program),
                kind: "change_impact",
                title: "저장한 혜택 공고 변경".into(),
                body: format!(
                    "저장한 혜택 1건의 {}이(가) 바뀌었어요. 앱에서 준비 상태를 다시 확인해 주세요.",
                    fields.join("·")
                ),
                expo_tokens: tokens,
            });
        }
    }
    Ok(out)
}

/// A preparation untouched for 7-30 days on a programme still open.
pub async fn resume_suggestions(pool: &PgPool, now: DateTime<Utc>) -> Result<Vec<Outgoing>> {
    if !within_window(now) {
        return Ok(Vec::new());
    }
    let available = mazimi_core::currently_available_sql("p.", "now()");
    let mut out = Vec::new();
    for user in opted_in(pool, "resume_suggestions").await? {
        let row: Option<(Uuid, String, DateTime<Utc>)> = sqlx::query_as(&format!(
            "SELECT p.id, p.title, a.updated_at FROM application_preparations a
             JOIN current_programs p ON p.id=a.program_id
             WHERE a.user_id=$1 AND {available} AND {NOT_SETTLED}
               AND a.updated_at BETWEEN now() - interval '30 days' AND now() - interval '7 days'
               AND COALESCE(LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at), 'infinity') > now()
             ORDER BY LEAST(p.benefit_valid_until,p.deadline_at,p.application_end_at) ASC NULLS LAST, p.id LIMIT 1"
        ))
        .bind(user)
        .fetch_optional(pool)
        .await?;
        let Some((program, _title, updated)) = row else {
            continue;
        };
        let tokens = push_tokens(pool, user).await?;
        let key = format!("{program}:{}", kst_date(updated));
        if tokens.is_empty()
            || !reserve(pool, user, "resume", Some(program), &key, kst_date(now)).await?
        {
            continue;
        }
        let days = (now - updated).num_days().max(7);
        out.push(Outgoing {
            user_id: user,
            program_id: Some(program),
            kind: "resume",
            title: "멈춘 신청 준비".into(),
            body: format!(
                "저장한 혜택의 신청 준비가 {days}일째 멈춰 있어요. 마감 전에 남은 항목을 확인해 보세요."
            ),
            expo_tokens: tokens,
        });
    }
    Ok(out)
}

/// Pending run-finished pushes inside the window, at most `limit`.
pub async fn drain_outbox(pool: &PgPool, now: DateTime<Utc>, limit: i64) -> Result<Vec<Outgoing>> {
    if !within_window(now) {
        return Ok(Vec::new());
    }
    let rows: Vec<(Uuid, Uuid, String, String)> = sqlx::query_as(
        "UPDATE assistant_notification_outbox o SET status='sent', sent_at=now()
         WHERE o.id IN (SELECT id FROM assistant_notification_outbox
                        WHERE status='pending' AND created_at >= now() - interval '6 hours'
                        ORDER BY created_at LIMIT $1 FOR UPDATE SKIP LOCKED)
         RETURNING o.id, o.user_id, o.title, o.body",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::new();
    for (_, user, title, body) in rows {
        let tokens = push_tokens(pool, user).await?;
        if tokens.is_empty() {
            continue;
        }
        out.push(Outgoing {
            user_id: user,
            program_id: None,
            kind: "run_done",
            title,
            body,
            expo_tokens: tokens,
        });
    }
    Ok(out)
}

/// Payload for logs and tests; tokens are never logged.
pub fn describe(message: &Outgoing) -> Value {
    json!({"kind": message.kind, "program_id": message.program_id, "devices": message.expo_tokens.len()})
}

/// Earliest instant of a KST wall-clock time, for tests and schedules.
pub fn kst_instant(date: NaiveDate, hour: u32, minute: u32) -> DateTime<Utc> {
    date.and_hms_opt(hour, minute, 0)
        .expect("valid time")
        .and_local_timezone(kst())
        .single()
        .expect("fixed offset")
        .with_timezone(&Utc)
}

/// The next Monday 10:30 KST on or after `from`'s KST date.
pub fn next_briefing_slot(from: DateTime<Utc>) -> DateTime<Utc> {
    let mut date = kst_date(from);
    while date.weekday() != Weekday::Mon {
        date += Duration::days(1);
    }
    kst_instant(date, 10, 30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_is_nine_to_twenty_one_kst() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
        assert!(!within_window(kst_instant(day, 8, 59)));
        assert!(within_window(kst_instant(day, 9, 0)));
        assert!(within_window(kst_instant(day, 20, 59)));
        assert!(!within_window(kst_instant(day, 21, 0)));
        assert!(!within_window(kst_instant(day, 2, 0)));
    }

    /// A corporate offer's amounts are no support money: publish clearing
    /// them (or a stray value) is never a "지원 금액" change for the user.
    #[tokio::test]
    async fn a_corporate_amount_is_no_fingerprint_change() {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("TEST_DATABASE_URL unset; skipping");
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("test database");
        let mut tx = pool.begin().await.unwrap();
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,title,benefit_amount_monthly) VALUES($1,'corporate_benefit','manual','지문 테스트',5000)")
            .bind(id)
            .execute(&mut *tx)
            .await
            .unwrap();
        async fn fingerprint(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, id: Uuid) -> Value {
            sqlx::query_scalar::<_, Value>(&format!(
                "SELECT {FINGERPRINT_SQL} FROM programs p WHERE p.id = $1"
            ))
            .bind(id)
            .fetch_one(&mut **tx)
            .await
            .unwrap()
        }
        let with_amount = fingerprint(&mut tx, id).await;
        sqlx::query("UPDATE programs SET benefit_amount_monthly=NULL WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .unwrap();
        let cleared = fingerprint(&mut tx, id).await;
        assert!(changed_fields(&with_amount, &cleared).is_empty());
        // A support programme's amount change still is one.
        sqlx::query(
            "UPDATE programs SET program_type='welfare', benefit_amount_monthly=5000 WHERE id=$1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
        let paid = fingerprint(&mut tx, id).await;
        sqlx::query("UPDATE programs SET benefit_amount_monthly=NULL WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            changed_fields(&paid, &fingerprint(&mut tx, id).await),
            ["지원 금액"]
        );
        tx.rollback().await.unwrap();
        pool.close().await;
    }

    #[test]
    fn changed_fields_name_what_moved() {
        let before = json!({"deadline":[1],"conditions":"a","rules":1,"documents":[],"amount":[1],"status":"active"});
        let mut after = before.clone();
        after["deadline"] = json!([2]);
        after["documents"] = json!([["등본", true]]);
        assert_eq!(changed_fields(&before, &after), ["마감일", "제출 서류"]);
        assert!(changed_fields(&before, &before).is_empty());
    }

    #[test]
    fn a_documents_text_appearing_is_no_change() {
        let base = json!({"deadline":[1],"conditions":"joint-empty","conditions_only":"c","submission_documents":null,"documents_side_data":true,"rules":1,"documents":[],"amount":[1],"status":"active"});
        let mut filled = base.clone();
        filled["conditions"] = json!("joint-filled");
        filled["submission_documents"] = json!("d1");
        assert!(changed_fields(&base, &filled).is_empty());
        // Once a text is stored, editing or removing it is a change.
        let mut edited = filled.clone();
        edited["submission_documents"] = json!("d2");
        assert_eq!(changed_fields(&filled, &edited), ["제출 서류"]);
        let mut removed = filled.clone();
        removed["submission_documents"] = Value::Null;
        assert_eq!(changed_fields(&filled, &removed), ["제출 서류"]);
        // The conditions alone moving is "지원 조건".
        let mut moved = filled.clone();
        moved["conditions_only"] = json!("c2");
        assert_eq!(changed_fields(&filled, &moved), ["지원 조건"]);
        // A first-format fingerprint (joint hash only, the documents text
        // absent and hashed as "") against the current form: the text
        // appearing is no change, the conditions moving still is.
        let legacy = json!({"deadline":[1],"conditions":"c","rules":1,"documents":[],"amount":[1],"status":"active"});
        let mut now = filled.clone();
        now["conditions"] = json!("joint-filled");
        assert!(changed_fields(&legacy, &now).is_empty());
        now["conditions_only"] = json!("c2");
        assert_eq!(changed_fields(&legacy, &now), ["지원 조건"]);
        // Where the text is the notice's own field (온통청년, 장학재단), a
        // text appearing is a change, in either fingerprint format.
        let mut own = base.clone();
        own["documents_side_data"] = json!(false);
        let mut own_filled = filled.clone();
        own_filled["documents_side_data"] = json!(false);
        assert_eq!(changed_fields(&own, &own_filled), ["제출 서류"]);
        let mut own_now = own_filled.clone();
        own_now["conditions_only"] = json!("c");
        assert_eq!(changed_fields(&legacy, &own_now), ["제출 서류"]);
    }

    /// Filling `submission_documents` in for rows collected before the
    /// 보조금24 detail mapping (about 10,000 rows) must not tell users their
    /// saved programmes changed; a later edit of the text does.
    #[tokio::test]
    async fn filling_in_documents_is_no_change_impact() {
        let pool = pool().await;
        let u = user(&pool, true).await;
        let program = saved_program(&pool, u, 20).await;
        let legacy_program = saved_program(&pool, u, 25).await;
        // 보조금24 rows: their documents text is side data.
        sqlx::query("UPDATE programs SET source_type='gov_benefits' WHERE id = ANY($1)")
            .bind(vec![program, legacy_program])
            .execute(&pool)
            .await
            .unwrap();
        opt_in(&pool, u, false, true, false).await;
        let noon = kst_instant(kst_date(Utc::now()), 12, 0);
        let day = |n: i64| noon + Duration::days(n);
        let mine = |messages: Vec<Outgoing>| -> Vec<Outgoing> {
            messages.into_iter().filter(|m| m.user_id == u).collect()
        };
        assert!(mine(change_impacts(&pool, day(0)).await.unwrap()).is_empty());
        // One baseline in the first format, as stored before this change.
        sqlx::query("UPDATE assistant_program_fingerprints SET fingerprint = fingerprint - 'conditions_only' - 'submission_documents' WHERE user_id=$1 AND program_id=$2")
            .bind(u).bind(legacy_program).execute(&pool).await.unwrap();
        for (id, text) in [
            (program, "해당없음"),
            (legacy_program, "○ 신분증\n○ 통장사본"),
        ] {
            sqlx::query("UPDATE programs SET submission_documents=$2 WHERE id=$1")
                .bind(id)
                .bind(text)
                .execute(&pool)
                .await
                .unwrap();
        }
        for n in 1..=3 {
            assert!(
                mine(change_impacts(&pool, day(n)).await.unwrap()).is_empty(),
                "day {n}: a filled-in text is no change"
            );
        }
        sqlx::query("UPDATE programs SET submission_documents='○ 신분증\n○ 통장사본\n○ 재학증명서' WHERE id=$1")
            .bind(legacy_program)
            .execute(&pool)
            .await
            .unwrap();
        let sent = mine(change_impacts(&pool, day(4)).await.unwrap());
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].program_id, Some(legacy_program));
        assert!(sent[0].body.contains("제출 서류"), "{}", sent[0].body);
        cleanup(&pool, &[u], &[program, legacy_program]).await;
    }

    async fn pool() -> PgPool {
        let url = std::env::var("TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://wello:wello@localhost:5432/wello".into());
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(3)
            .connect(&url)
            .await
            .expect("isolated test database is required")
    }

    async fn user(pool: &PgPool, push: bool) -> Uuid {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO users(auth_provider, auth_provider_id, role) VALUES('test',$1,'user') RETURNING id",
        )
        .bind(Uuid::new_v4().to_string())
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO push_tokens(user_id,token,platform) VALUES($1,$2,'android')")
            .bind(id)
            .bind(format!("ExpoPushToken[{}]", Uuid::new_v4().simple()))
            .execute(pool)
            .await
            .unwrap();
        if !push {
            sqlx::query(
                "INSERT INTO notification_preferences(user_id, channel_push) VALUES($1,false)",
            )
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        }
        id
    }

    async fn saved_program(pool: &PgPool, user: Uuid, days: i32) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,deadline_at) VALUES($1,'policy','manual',$2,'능동 비서 테스트','active',true,now()+make_interval(days => $3))")
            .bind(id).bind(id.to_string()).bind(days).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO user_bookmarks(user_id,program_id) VALUES($1,$2)")
            .bind(user)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        id
    }

    async fn opt_in(pool: &PgPool, user: Uuid, weekly: bool, change: bool, resume: bool) {
        sqlx::query("INSERT INTO assistant_proactive_settings(user_id,weekly_briefing,change_alerts,resume_suggestions) VALUES($1,$2,$3,$4)")
            .bind(user).bind(weekly).bind(change).bind(resume).execute(pool).await.unwrap();
    }

    async fn cleanup(pool: &PgPool, users: &[Uuid], programs: &[Uuid]) {
        for u in users {
            sqlx::query("DELETE FROM users WHERE id=$1")
                .bind(u)
                .execute(pool)
                .await
                .unwrap();
        }
        for p in programs {
            sqlx::query("DELETE FROM programs WHERE id=$1")
                .bind(p)
                .execute(pool)
                .await
                .unwrap();
        }
    }

    /// Round 2 (arch M6): "이번 주 마감" counts the Korean days Monday to
    /// Sunday, the week the assistant reads for the same words, not a
    /// rolling seven days.
    #[tokio::test]
    async fn the_weekly_count_is_the_assistants_week() {
        let pool = pool().await;
        let opted = user(&pool, true).await;
        opt_in(&pool, opted, true, false, false).await;
        let monday = next_briefing_slot(Utc::now());
        let day = kst_date(monday);
        let mut programs = Vec::new();
        for due in [
            kst_instant(day + Duration::days(6), 20, 0),
            kst_instant(day + Duration::days(7), 9, 0),
        ] {
            let id = Uuid::new_v4();
            sqlx::query("INSERT INTO programs(id,program_type,source_type,source_id,title,program_status,is_active,deadline_at) VALUES($1,'policy','manual',$2,'능동 비서 주간','active',true,$3)")
                .bind(id).bind(id.to_string()).bind(due).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO user_bookmarks(user_id,program_id) VALUES($1,$2)")
                .bind(opted)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            programs.push(id);
        }
        let sent: Vec<Outgoing> = weekly_briefings(&pool, monday)
            .await
            .unwrap()
            .into_iter()
            .filter(|m| m.user_id == opted)
            .collect();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(
            sent[0].body.contains("이번 주 마감 1개"),
            "{}",
            sent[0].body
        );
        cleanup(&pool, &[opted], &programs).await;
    }

    #[tokio::test]
    async fn proactive_messages_are_opt_in_windowed_capped_and_deduplicated() {
        let pool = pool().await;
        let opted = user(&pool, true).await;
        let silent = user(&pool, true).await;
        let muted = user(&pool, false).await;
        // Due three days after the briefing slot: inside its week, whatever
        // weekday the test runs on (the count reads the slot's week).
        let slot = next_briefing_slot(Utc::now());
        let days = (kst_date(slot) - kst_date(Utc::now())).num_days() as i32 + 3;
        let mut programs = Vec::new();
        for u in [opted, silent, muted] {
            programs.push(saved_program(&pool, u, days).await);
        }
        opt_in(&pool, opted, true, false, true).await;
        opt_in(&pool, muted, true, false, false).await;
        // A stalled preparation for the resume suggestion.
        sqlx::query("INSERT INTO application_preparations(user_id,program_id,plan_revision,updated_at) VALUES($1,$2,'r',now()-interval '10 days')")
            .bind(opted).bind(programs[0]).execute(&pool).await.unwrap();
        let monday = next_briefing_slot(Utc::now());
        let night = kst_instant(kst_date(monday), 22, 0);
        assert!(
            weekly_briefings(&pool, night).await.unwrap().is_empty(),
            "quiet hours"
        );
        let ours = |messages: Vec<Outgoing>| -> Vec<Outgoing> {
            messages
                .into_iter()
                .filter(|m| [opted, silent, muted].contains(&m.user_id))
                .collect()
        };
        let sent = ours(weekly_briefings(&pool, monday).await.unwrap());
        assert_eq!(
            sent.len(),
            1,
            "only the opted-in user with push on: {sent:?}"
        );
        assert_eq!(sent[0].user_id, opted);
        assert!(sent[0].body.contains("이번 주 마감 1개"));
        assert!(
            !sent[0].body.contains("능동 비서 테스트"),
            "no titles in the briefing"
        );
        assert!(
            ours(weekly_briefings(&pool, monday).await.unwrap()).is_empty(),
            "once per week"
        );
        // Same KST day: the cap holds the resume suggestion back.
        assert!(ours(resume_suggestions(&pool, monday).await.unwrap()).is_empty());
        let next_day = monday + Duration::days(1);
        let resumed = ours(resume_suggestions(&pool, next_day).await.unwrap());
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].program_id, Some(programs[0]));
        assert!(
            ours(
                resume_suggestions(&pool, next_day + Duration::days(1))
                    .await
                    .unwrap()
            )
            .is_empty(),
            "the same stall is suggested once"
        );
        cleanup(&pool, &[opted, silent, muted], &programs).await;
    }

    #[tokio::test]
    async fn change_impact_records_a_baseline_then_reports_what_changed() {
        let pool = pool().await;
        let u = user(&pool, true).await;
        let program = saved_program(&pool, u, 20).await;
        opt_in(&pool, u, false, true, false).await;
        let noon = kst_instant(kst_date(Utc::now()), 12, 0);
        let mine = |messages: Vec<Outgoing>| -> Vec<Outgoing> {
            messages.into_iter().filter(|m| m.user_id == u).collect()
        };
        assert!(
            mine(change_impacts(&pool, noon).await.unwrap()).is_empty(),
            "baseline only"
        );
        assert!(
            mine(change_impacts(&pool, noon).await.unwrap()).is_empty(),
            "no change"
        );
        sqlx::query(
            "UPDATE programs SET deadline_at = deadline_at - interval '5 days' WHERE id=$1",
        )
        .bind(program)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO program_documents(program_id,document_name,is_required,sort_order) VALUES($1,'주민등록등본',true,1)")
            .bind(program).execute(&pool).await.unwrap();
        let sent = mine(change_impacts(&pool, noon).await.unwrap());
        assert_eq!(sent.len(), 1);
        assert!(
            sent[0].body.contains("마감일") && sent[0].body.contains("제출 서류"),
            "{}",
            sent[0].body
        );
        assert!(mine(
            change_impacts(&pool, noon + Duration::days(1))
                .await
                .unwrap()
        )
        .is_empty());
        assert!(
            !sent[0].body.contains("능동 비서 테스트"),
            "no titles on the lock screen"
        );
        // Two programmes change in one tick: one push that day (daily cap);
        // the other change is kept and delivered on a later day, not lost.
        let second = saved_program(&pool, u, 30).await;
        let day = |n: i64| noon + Duration::days(n);
        assert!(
            mine(change_impacts(&pool, day(2)).await.unwrap()).is_empty(),
            "baseline"
        );
        for id in [program, second] {
            sqlx::query("UPDATE programs SET benefit_amount_monthly = 100000 WHERE id=$1")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        let first = mine(change_impacts(&pool, day(3)).await.unwrap());
        assert_eq!(first.len(), 1);
        assert!(
            mine(change_impacts(&pool, day(3)).await.unwrap()).is_empty(),
            "daily cap"
        );
        let next = mine(change_impacts(&pool, day(4)).await.unwrap());
        assert_eq!(next.len(), 1, "the second change was kept, not lost");
        assert_ne!(first[0].program_id, next[0].program_id);
        assert!(mine(change_impacts(&pool, day(5)).await.unwrap()).is_empty());
        // Opting out stops tracking.
        sqlx::query("UPDATE assistant_proactive_settings SET change_alerts=false WHERE user_id=$1")
            .bind(u)
            .execute(&pool)
            .await
            .unwrap();
        purge(&pool, DEFAULT_RETENTION_DAYS).await.unwrap();
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM assistant_program_fingerprints WHERE user_id=$1",
        )
        .bind(u)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(left, 0);
        cleanup(&pool, &[u], &[program, second]).await;
    }

    #[tokio::test]
    async fn purge_applies_retention_expiry_and_outbox_rules() {
        let pool = pool().await;
        let u = user(&pool, true).await;
        let conversation = |age_days: i32| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>("INSERT INTO assistant_conversations(user_id, updated_at) VALUES($1, now()-make_interval(days => $2)) RETURNING id")
                    .bind(u).bind(age_days).fetch_one(&pool).await.unwrap()
            }
        };
        let old = conversation(31).await;
        let recent = conversation(1).await;
        let waiting: Uuid = sqlx::query_scalar("INSERT INTO assistant_runs(conversation_id,user_id,status,toolset_version) VALUES($1,$2,'awaiting_confirmation','t') RETURNING id")
            .bind(recent).bind(u).fetch_one(&pool).await.unwrap();
        let call: Uuid = sqlx::query_scalar("INSERT INTO assistant_tool_calls(run_id,seq,tool_name,tool_version,permission,status) VALUES($1,0,'set_alert','v1','confirm_write','awaiting_confirmation') RETURNING id")
            .bind(waiting).fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_confirmations(run_id,user_id,tool_call_id,tool_name,program_id,arguments,precondition,preview,payload_hash,expires_at) VALUES($1,$2,$3,'set_alert',gen_random_uuid(),'{}','{}','{}',repeat('a',64),now()-interval '1 minute')")
            .bind(waiting).bind(u).bind(call).execute(&pool).await.unwrap();
        let stuck_conversation = conversation(0).await;
        let stuck: Uuid = sqlx::query_scalar("INSERT INTO assistant_runs(conversation_id,user_id,status,toolset_version,updated_at) VALUES($1,$2,'tool_running','t',now()-interval '2 hours') RETURNING id")
            .bind(stuck_conversation).bind(u).fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO assistant_notification_outbox(user_id,title,body,created_at) VALUES($1,'t','b',now()-interval '7 hours')")
            .bind(u).execute(&pool).await.unwrap();
        let summary = purge(&pool, 30).await.unwrap();
        assert!(summary.conversations_deleted >= 1);
        let exists = |id: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM assistant_conversations WHERE id=$1)",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        assert!(!exists(old).await, "past retention: deleted");
        assert!(exists(recent).await);
        let status = |id: Uuid| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, String>("SELECT status FROM assistant_runs WHERE id=$1")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        assert_eq!(
            status(waiting).await,
            "completed",
            "nothing left to confirm"
        );
        assert_eq!(status(stuck).await, "failed");
        let call_status: String =
            sqlx::query_scalar("SELECT status FROM assistant_tool_calls WHERE id=$1")
                .bind(call)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(call_status, "expired");
        let dropped: i64 = sqlx::query_scalar("SELECT count(*) FROM assistant_notification_outbox WHERE user_id=$1 AND status='dropped'")
            .bind(u).fetch_one(&pool).await.unwrap();
        assert_eq!(dropped, 1);
        // Fresh outbox rows go out only inside the window.
        sqlx::query("INSERT INTO assistant_notification_outbox(user_id,title,body) VALUES($1,'마지미 비서','끝났어요')")
            .bind(u).execute(&pool).await.unwrap();
        let today = kst_date(Utc::now());
        assert!(drain_outbox(&pool, kst_instant(today, 23, 0), 100)
            .await
            .unwrap()
            .iter()
            .all(|m| m.user_id != u));
        let sent: Vec<Outgoing> = drain_outbox(&pool, kst_instant(today, 12, 0), 1000)
            .await
            .unwrap()
            .into_iter()
            .filter(|m| m.user_id == u)
            .collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(describe(&sent[0])["devices"], 1);
        cleanup(&pool, &[u], &[]).await;
    }
}
