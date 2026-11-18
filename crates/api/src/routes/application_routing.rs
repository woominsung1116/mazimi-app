//! Operator metric for link quality: where the public catalogue sends people
//! and how often the application method is still unreadable. Counts only; no
//! user data and no per-program URLs leave the server.
use crate::{
    auth::AdminUser,
    services::{application_destination, application_workflow},
};
use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::collections::BTreeMap;

/// Hosts outside the known platforms form a long tail; keep the top entries
/// readable and fold the rest into `other`.
const MAX_PLATFORMS: usize = 25;

/// GET /api/v1/admin/application-routing-stats
pub async fn stats(
    _admin: AdminUser,
    State(pool): State<PgPool>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let rows: Vec<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT official_url, application_method FROM current_programs")
            .fetch_all(&pool)
            .await
            .map_err(crate::errors::internal_error)?;
    Ok(Json(summarize(&rows)))
}

fn summarize(rows: &[(Option<String>, Option<String>)]) -> Value {
    let total = rows.len();
    let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut by_kind_platform: BTreeMap<(&'static str, String), usize> = BTreeMap::new();
    let mut without_destination = 0usize;
    let mut by_channel: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut unknown_programs = 0usize;
    let mut needs_confirmation = 0usize;
    let mut no_application_required = 0usize;
    let mut without_method = 0usize;
    for (official_url, method) in rows {
        match application_destination::resolve(official_url.as_deref()) {
            Some(link) => {
                *by_kind.entry(link.kind).or_default() += 1;
                *by_kind_platform
                    .entry((link.kind, link.platform))
                    .or_default() += 1;
            }
            None => without_destination += 1,
        }
        if method.as_deref().is_none_or(|m| m.trim().is_empty()) {
            without_method += 1;
        }
        let workflow = application_workflow::classify(method.as_deref());
        for channel in &workflow.channels {
            *by_channel.entry(channel.id).or_default() += 1;
        }
        if workflow.channels.iter().any(|c| c.id == "unknown") {
            unknown_programs += 1;
        }
        if workflow.needs_confirmation {
            needs_confirmation += 1;
        }
        if workflow.no_application_required {
            no_application_required += 1;
        }
    }
    let mut platforms: Vec<((&'static str, String), usize)> =
        by_kind_platform.into_iter().collect();
    platforms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut folded: Vec<Value> = vec![];
    let mut other: BTreeMap<&'static str, usize> = BTreeMap::new();
    for ((kind, platform), count) in platforms {
        if folded.len() < MAX_PLATFORMS {
            folded.push(json!({"kind": kind, "platform": platform, "count": count}));
        } else {
            *other.entry(kind).or_default() += count;
        }
    }
    for (kind, count) in other {
        folded.push(json!({"kind": kind, "platform": "other", "count": count}));
    }
    let ratio = |count: usize| {
        if total == 0 {
            0.0
        } else {
            count as f64 / total as f64
        }
    };
    json!({
        "generated_at": chrono::Utc::now(),
        "programs": total,
        "destination": {
            "without_destination": without_destination,
            "by_kind": by_kind,
            "by_kind_platform": folded,
            "information_ratio": ratio(by_kind.get("information").copied().unwrap_or(0)),
        },
        "workflow": {
            "without_method": without_method,
            "by_channel": by_channel,
            "unknown_programs": unknown_programs,
            "unknown_ratio": ratio(unknown_programs),
            "needs_confirmation": needs_confirmation,
            "no_application_required": no_application_required,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::summarize;

    #[test]
    fn counts_destination_kinds_platforms_and_unknown_channels() {
        let rows = vec![
            (
                Some("https://www.gov.kr/portal/rcvfvrSvc/dtlEx/1".to_string()),
                Some("정부24온라인신청||방문신청".to_string()),
            ),
            (
                Some("https://www.gov.kr/portal/rcvfvrSvc/dtlEx/2".to_string()),
                Some("홈페이지 또는 QR코드".to_string()),
            ),
            (Some("https://provider.example.org/apply".to_string()), None),
            (
                Some("http://insecure.example.org/".to_string()),
                Some("".to_string()),
            ),
        ];
        let summary = summarize(&rows);
        assert_eq!(summary["programs"], 4);
        assert_eq!(summary["destination"]["without_destination"], 1);
        assert_eq!(summary["destination"]["by_kind"]["information"], 2);
        assert_eq!(summary["destination"]["by_kind"]["unverified_page"], 1);
        assert_eq!(summary["destination"]["information_ratio"], 0.5);
        let platforms = summary["destination"]["by_kind_platform"]
            .as_array()
            .unwrap();
        assert_eq!(platforms[0]["platform"], "government24");
        assert_eq!(platforms[0]["count"], 2);
        assert_eq!(summary["workflow"]["without_method"], 2);
        assert_eq!(summary["workflow"]["by_channel"]["online"], 1);
        assert_eq!(summary["workflow"]["by_channel"]["visit"], 1);
        assert_eq!(summary["workflow"]["unknown_programs"], 3);
        assert_eq!(summary["workflow"]["unknown_ratio"], 0.75);
        assert_eq!(summarize(&[])["workflow"]["unknown_ratio"], 0.0);
    }
}
