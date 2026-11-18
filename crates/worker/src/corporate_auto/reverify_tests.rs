use super::*;

#[test]
fn every_outcome_label_has_one_class() {
    for label in CONFIRMING {
        assert_eq!(class_of(label), Class::Confirmed, "{label}");
    }
    for label in ["fetch_failed", "rate_limited", "unsupported"] {
        assert_eq!(class_of(label), Class::Transient, "{label}");
    }
    for label in [
        "robots_disallowed",
        "blocked",
        "policy_blocked",
        "adoption_expired",
    ] {
        assert_eq!(class_of(label), Class::Blocked, "{label}");
    }
    // Positive evidence about the offer, its page or its permission.
    for label in [
        "ambiguous",
        "quote_missing",
        "window_changed",
        "field_mismatch",
        "no_evidence",
        "gone",
        "retired_gone",
        "redirected",
        "opted_out",
        "url_move_proposed",
    ] {
        assert_eq!(class_of(label), Class::Refuted, "{label}");
    }
    assert!(Class::Transient.waits() && Class::Blocked.waits());
    assert!(!Class::Confirmed.waits() && !Class::Refuted.waits());
}

#[test]
fn grace_runs_72_hours_from_the_last_confirmation_and_never_without_one() {
    let now = Utc::now();
    assert!(!in_grace(None, now), "never confirmed: no grace");
    assert!(in_grace(Some(now - Duration::hours(71)), now));
    assert!(!in_grace(Some(now - Duration::hours(72)), now));
    assert!(!in_grace(Some(now - Duration::days(10)), now));
    assert_eq!(grace_hold(Class::Transient), "source_unreachable");
    assert_eq!(grace_hold(Class::Blocked), "access_revoked");
}

#[test]
fn transient_failures_back_off_from_three_hours_to_a_day() {
    for (failures, base) in [(1, 3), (2, 6), (3, 12), (4, 24), (9, 24)] {
        let delay = retry_delay("fetch_failed", failures, false, Uuid::new_v4()).num_hours();
        assert!(
            (base..=base + base / 4).contains(&delay),
            "failure {failures}: {delay} h, base {base} h"
        );
    }
    // A 429 starts at a day and doubles to four.
    for (failures, base) in [(1, 24), (2, 48), (3, 96), (6, 96)] {
        let delay = retry_delay("rate_limited", failures, false, Uuid::new_v4()).num_hours();
        assert!(
            (base..=base + base / 4).contains(&delay),
            "429 {failures}: {delay} h"
        );
    }
    // Hidden after grace: a day doubling to a week.
    let delay = retry_delay("fetch_failed", 8, true, Uuid::new_v4()).num_hours();
    assert!((168..=168 + 42).contains(&delay), "{delay}");
}

#[test]
fn a_block_is_looked_at_daily_in_grace_and_weekly_once_hidden() {
    for label in ["robots_disallowed", "blocked", "policy_blocked"] {
        let shown = retry_delay(label, 1, false, Uuid::new_v4()).num_hours();
        let hidden = retry_delay(label, 5, true, Uuid::new_v4()).num_hours();
        assert!((24..=30).contains(&shown), "{label}: {shown}");
        assert!((168..=210).contains(&hidden), "{label}: {hidden}");
    }
}

#[test]
fn the_jitter_is_fixed_per_source_and_spreads_sources_across_runs() {
    let id = Uuid::new_v4();
    assert_eq!(
        retry_delay("fetch_failed", 4, false, id),
        retry_delay("fetch_failed", 4, false, id)
    );
    let spread: std::collections::BTreeSet<i64> = (0..64)
        .map(|_| retry_delay("fetch_failed", 4, false, Uuid::new_v4()).num_hours())
        .collect();
    assert!(
        spread.len() >= 4,
        "64 sources land in {} run hours",
        spread.len()
    );
    assert!(spread.iter().all(|h| (24..=30).contains(h)), "{spread:?}");
}

#[test]
fn the_run_alert_reads_the_unreadable_share_over_enough_checks() {
    let mut health = RunHealth::default();
    for class in [Class::Transient, Class::Blocked, Class::Transient] {
        health.count(class);
    }
    assert_eq!(health.alert(), None, "three checks say nothing");
    health.count(Class::Confirmed);
    assert_eq!(health.checks(), 4);
    assert_eq!(health.alert(), Some(true), "3 of 4 unreadable");
    let mut calm = RunHealth::default();
    for class in [
        Class::Confirmed,
        Class::Confirmed,
        Class::Refuted,
        Class::Transient,
    ] {
        calm.count(class);
    }
    assert_eq!(calm.unreadable_share(), 0.25);
    assert_eq!(calm.alert(), Some(false), "at the threshold: no alert");
    // Refutations are decisions, not an outage.
    let mut refuted = RunHealth::default();
    for _ in 0..8 {
        refuted.count(Class::Refuted);
    }
    assert_eq!(refuted.alert(), Some(false));
}

/// The 2026-11-04 drop, outcome by outcome, against the rule: which rows
/// the new taxonomy keeps shown inside grace and which it still hides.
/// The database replay of the same rows is
/// `evidence::tests::the_1104_drop_replays_under_the_outcome_classes`.
#[test]
fn the_1104_drop_rows_classify_as_measured() {
    let rows = [
        // (row, the outcome production recorded, still shown inside grace)
        ("Apple Music: 429 back-off", "rate_limited", true),
        (
            "Apple Music: census pause read as a verdict",
            "policy_blocked",
            true,
        ),
        ("GitKraken: section changed", "ambiguous", false),
        ("Sketch: section changed", "ambiguous", false),
        ("Unity: /kr/ page without the quote", "quote_missing", false),
        ("GitHub Student Pack", "window_changed", false),
        ("할리스", "window_changed", false),
        ("Notion", "quote_missing", false),
        ("SKT 라이트 59", "quote_missing", false),
    ];
    let now = Utc::now();
    let confirmed_yesterday = Some(now - Duration::hours(24));
    for (row, outcome, shown) in rows {
        let class = class_of(outcome);
        assert_eq!(
            class.waits() && in_grace(confirmed_yesterday, now),
            shown,
            "{row}: {outcome} is {}",
            class.as_str()
        );
    }
}
