//! The run's steps as the user sees them: the one source of step labels and
//! step status for the timeline (the stream's events and the stored run
//! view).
//!
//! One planned step can run as several tool calls: a check over the three
//! programmes a search found is three `check_eligibility` calls. The user
//! asked for one thing, so consecutive calls of the same tool are one step,
//! labelled once with how many programmes it covered ("내 조건 확인 · 혜택
//! 3개"), never three rows of the same label.
use mazimi_core::agent::ToolName;
use std::ops::Range;

/// The label of a step: the tool's label, and the count when it ran over
/// several programmes (or several times without one).
pub fn label(tool: ToolName, programmes: usize, calls: usize) -> String {
    match (programmes, calls) {
        (n, _) if n > 1 => format!("{} · 혜택 {n}개", tool.label()),
        (_, n) if n > 1 => format!("{} · {n}번", tool.label()),
        _ => tool.label().to_owned(),
    }
}

/// Consecutive runs of the same tool, as index ranges into `tools`.
pub fn group<T: PartialEq>(tools: &[T]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (i, tool) in tools.iter().enumerate() {
        match out.last_mut() {
            Some(last) if tools[last.start] == *tool => last.end = i + 1,
            _ => out.push(i..i + 1),
        }
    }
    out
}

/// The status a grouped step shows: waiting for approval if any call is,
/// done if any call is, skipped if all were, else the first call's.
pub fn status<'a>(statuses: impl IntoIterator<Item = &'a str>) -> &'a str {
    let all: Vec<&str> = statuses.into_iter().collect();
    for wanted in ["awaiting_confirmation", "ok"] {
        if all.contains(&wanted) {
            return wanted;
        }
    }
    if !all.is_empty() && all.iter().all(|s| *s == "skipped") {
        return "skipped";
    }
    all.first().copied().unwrap_or("ok")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// User report (2026-10-31): "2 3 4가 왜 다 내 조건확인이야" — one search,
    /// then three checks of what it found, were four rows with three
    /// identical labels.
    #[test]
    fn checks_over_found_programmes_are_one_step() {
        let tools = [
            ToolName::SearchBenefits,
            ToolName::CheckEligibility,
            ToolName::CheckEligibility,
            ToolName::CheckEligibility,
        ];
        let groups = group(&tools);
        assert_eq!(groups, vec![0..1, 1..4]);
        let labels: Vec<String> = groups
            .iter()
            .map(|g| label(tools[g.start], g.len(), g.len()))
            .collect();
        assert_eq!(labels, ["조건에 맞는 혜택 찾기", "내 조건 확인 · 혜택 3개"]);
    }

    #[test]
    fn a_step_shows_its_most_telling_status() {
        assert_eq!(status(["ok", "error", "ok"]), "ok");
        assert_eq!(
            status(["error", "awaiting_confirmation"]),
            "awaiting_confirmation"
        );
        assert_eq!(status(["skipped", "skipped"]), "skipped");
        assert_eq!(status(["error", "error"]), "error");
    }
}
