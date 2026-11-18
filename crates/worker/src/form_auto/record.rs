//! The run summary and the `job:form_auto` incident: a failed or skipped run
//! opens it (once, deduplicated by fingerprint); a clean run resolves it.
use crate::operations::{self, EventCommand, Severity};
use anyhow::Result;
use serde_json::{json, Value};
use sqlx::PgPool;

/// How a run ended, as the host runner reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// All steps ran (zero or more templates imported).
    Done,
    /// Skipped before starting (disk below the margin, kill switch, worker unhealthy).
    Skipped(String),
    /// A step failed.
    Failed(String),
}

impl Outcome {
    pub fn parse(status: &str, reason: &str) -> Self {
        match status {
            "done" => Self::Done,
            "skipped" => Self::Skipped(reason.chars().take(200).collect()),
            _ => Self::Failed(reason.chars().take(200).collect()),
        }
    }
}

/// Counts from verdicts.json (document text is never copied into events).
pub fn summary(verdicts: &Value, import: Option<&Value>, converters: Option<&Value>) -> Value {
    json!({
        "counts": verdicts.get("counts").cloned().unwrap_or(Value::Null),
        "mode": verdicts.get("mode").cloned().unwrap_or(Value::Null),
        "shadow": verdicts.get("shadow").map(|s| json!({"n": s.get("n"), "agree": s.get("agree"), "judge_false_passes": s.get("judge_false_passes")})),
        // Reference converter (h2o) and IR (kordoc) failures: they do not decide alone, but count.
        "converters": converters.map(|c| json!({"files": c.get("files"), "h2o_failed": c.get("h2o_failed"), "kordoc_failed": c.get("kordoc_failed")})),
        "import": import.map(|i| json!({"forms": i.get("forms"), "refused": i.get("refused"), "post_check_pending": i.get("post_check_pending"),
                                         "post_check_backlog": i.get("post_check_backlog"), "dry_run": i.get("dry_run"),
                                         "would_import": i.get("would_import")})),
    })
}

/// Imports waiting for their post-check, from a run summary.
pub fn post_check_backlog(summary: &Value) -> u64 {
    summary["import"]["post_check_backlog"]
        .as_u64()
        .unwrap_or(0)
}

/// Judge questions that got no valid answer, from a run summary.
pub fn judge_unanswered(summary: &Value) -> u64 {
    summary["counts"]["judge_unanswered"].as_u64().unwrap_or(0)
}

const BACKLOG: &str = "form_auto:post_check_backlog";

pub async fn record(pool: &PgPool, run: &str, outcome: &Outcome, details: Value) -> Result<()> {
    let unanswered = judge_unanswered(&details);
    let backlog = post_check_backlog(&details);
    let details = json!({"run": run, "details": details});
    match outcome {
        Outcome::Done => {
            operations::record_event(
                pool,
                &EventCommand {
                    fingerprint: &format!("form_auto:run:{run}"),
                    severity: Severity::Info,
                    component: "form_auto",
                    event_type: "form_auto_run",
                    subject_type: None,
                    subject_id: None,
                    message: "Automatic form-template run finished",
                    details,
                },
            )
            .await?;
            operations::resolve_event(pool, super::INCIDENT, None, Some("form auto run finished"))
                .await?;
            // Imports live without their post-check: a Warning until the checks catch up.
            if backlog > 0 {
                operations::record_event(pool, &EventCommand {
                    fingerprint: BACKLOG,
                    severity: Severity::Warning,
                    component: "form_auto",
                    event_type: "form_auto_post_check_backlog",
                    subject_type: None,
                    subject_id: None,
                    message: "Automatic imports are waiting for their post-check (form-auto.sh pending)",
                    details: json!({"run": run, "backlog": backlog}),
                })
                .await?;
            } else {
                operations::resolve_event(pool, BACKLOG, None, Some("no post-check waiting"))
                    .await?;
            }
            // Loud, not fatal: judge questions that got no valid answer failed their templates.
            if unanswered > 0 {
                operations::record_event(
                    pool,
                    &EventCommand {
                        fingerprint: &format!("form_auto:judge_unanswered:{run}"),
                        severity: Severity::Warning,
                        component: "form_auto",
                        event_type: "form_auto_judge_unanswered",
                        subject_type: None,
                        subject_id: None,
                        message: "Judge questions got no valid answer (timeout or malformed reply)",
                        details: json!({"run": run, "unanswered": unanswered}),
                    },
                )
                .await?;
            }
        }
        Outcome::Skipped(why) | Outcome::Failed(why) => {
            let failed = matches!(outcome, Outcome::Failed(_));
            operations::record_event(
                pool,
                &EventCommand {
                    fingerprint: super::INCIDENT,
                    severity: if failed {
                        Severity::Error
                    } else {
                        Severity::Warning
                    },
                    component: "scheduler",
                    event_type: if failed {
                        "form_auto_failed"
                    } else {
                        "form_auto_skipped"
                    },
                    subject_type: None,
                    subject_id: None,
                    message: if failed {
                        "Automatic form-template run failed"
                    } else {
                        "Automatic form-template run skipped"
                    },
                    details: json!({"run": run, "reason": why, "details": details}),
                },
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_carries_counts_only() {
        let v = json!({"counts": {"importable": 1}, "mode": "shadow", "rows": [{"program": "document text"}],
                       "shadow": {"n": 2, "agree": 1, "judge_false_passes": 0, "templates": {"x": {}}}});
        let s = summary(
            &v,
            Some(&json!({"forms": 1, "refused": 0, "post_check_pending": 1})),
            Some(&json!({"files": 2, "h2o_failed": 2, "kordoc_failed": 0})),
        );
        assert!(!s.to_string().contains("document text"));
        assert_eq!(s["import"]["post_check_pending"], 1);
        assert_eq!(s["converters"]["h2o_failed"], 2);
        assert_eq!(
            judge_unanswered(&summary(
                &json!({"counts": {"judge_unanswered": 3}}),
                None,
                None
            )),
            3
        );
        assert_eq!(judge_unanswered(&s), 0);
        assert_eq!(
            post_check_backlog(&summary(&v, Some(&json!({"post_check_backlog": 2})), None)),
            2
        );
        assert_eq!(s["shadow"]["judge_false_passes"], 0);
        assert_eq!(
            Outcome::parse("skipped", "disk"),
            Outcome::Skipped("disk".into())
        );
        assert!(matches!(Outcome::parse("weird", "x"), Outcome::Failed(_)));
    }
}
