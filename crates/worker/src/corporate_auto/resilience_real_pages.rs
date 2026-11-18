//! Real-page ground truth (handoff §18): every stored snapshot of a
//! published or audited-correct offer (`tests/fixtures/real_pages`, made by
//! `corporate-snapshot`) is read at its own fetch moment, at first sight,
//! with each of the offer's audit fragments. Prints one row per page:
//! url | offer | live decision | hide_only decision | reason | matched text.
//! Nothing is labelled here; an independent reviewer labels the rows.

use super::*;
use crate::corporate_auto::evidence::visible_text;
use crate::corporate_auto::snapshot::SnapshotEntry;

fn fixtures() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real_pages")
}

/// The text that made the decision, for the reviewer.
fn matched_text(view: &PageView, quote: &str, key: &str, clock: &Clock) -> String {
    let cut = |text: &str| text.chars().take(160).collect::<String>();
    let fragments = split_fragments(quote);
    if key.starts_with("section_") {
        for fragment in &fragments {
            for i in holders(view, &fold_str(fragment)) {
                let root = view.root(i);
                let keep = view.elements[i].node;
                let texts = [
                    view.elements[i].text.clone(),
                    section_text(root, keep, true),
                    period_text(root, keep),
                ];
                for text in &texts {
                    let rest = squash(text).replace(&squash(fragment), " | ");
                    let found = match key {
                        "section_ended_past" | "section_ended" | "section_not_started" => {
                            clauses(&rest)
                                .into_iter()
                                .find(|c| clause_ending(c).is_some())
                        }
                        "section_ended_confirmed" => sentences_of(&rest).into_iter().find(|s| {
                            OFFER_ENDED_STATEMENT.is_match(s)
                                || s.split_once([':', '：'])
                                    .is_some_and(|(l, v)| own_period_end(l, v).is_some())
                        }),
                        "section_date_past" => sentences_of(&rest)
                            .into_iter()
                            .find(|s| own_period_passed(s, &OfferIdentity::default(), clock)),
                        "section_date_other" => sentences_of(&rest)
                            .into_iter()
                            .find(|s| has_past_end_at(s, clock) || has_future_start_at(s, clock)),
                        _ => clauses(&rest).into_iter().find(|c| {
                            let clause = GENERIC_EXCLUSION.replace_all(c, " # ");
                            let folded = fold_str(&clause);
                            (KO_EXCLUSION_MARKERS.iter().any(|m| folded.contains(m))
                                || says_excluded(&clause))
                                && !EXCLUSION_CONDITION.is_match(&clause)
                        }),
                    };
                    if let Some(found) = found {
                        return cut(found.trim());
                    }
                }
            }
        }
    }
    if key.starts_with("structured") {
        return cut(&format!("{:?}", view.page.stated));
    }
    for block in view.page.blocks.iter().filter(|b| page_wide(b)) {
        let text: String = block.segments.iter().map(|(t, _)| t.as_str()).collect();
        let text = squash(&text);
        let hit = match key {
            "ending_scan" => clauses(&text)
                .into_iter()
                .any(|c| clause_ending(c).is_some()),
            _ => has_past_end_at(&text, clock) || has_future_start_at(&text, clock),
        };
        if hit {
            return cut(&text);
        }
    }
    String::new()
}

#[test]
fn real_pages_are_read_for_ground_truth() {
    let Ok(index) = std::fs::read_to_string(fixtures().join("index.json")) else {
        eprintln!("no real-page snapshots; run corporate-snapshot");
        return;
    };
    let entries: Vec<SnapshotEntry> = serde_json::from_str(&index).expect("index parses");
    let kst = chrono::FixedOffset::east_opt(9 * 3_600).unwrap();
    let mut rows = Vec::new();
    let (mut live_hides, mut hide_only_hides) = (0usize, 0usize);
    for entry in entries.iter().filter(|e| e.outcome == "stored") {
        let file = entry.file.as_deref().expect("stored file");
        let html = std::fs::read_to_string(fixtures().join(file)).expect("snapshot");
        let digest: String = {
            use sha2::Digest;
            sha2::Sha256::digest(html.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        assert_eq!(
            Some(digest.as_str()),
            entry.sha256.as_deref(),
            "{file} unchanged"
        );
        let now = entry.fetched_at.with_timezone(&kst).naive_local();
        let view = PageView::at(&html, &visible_text(&html), now)
            .with_url(entry.final_url.as_deref().unwrap_or(&entry.url));
        let clock = view.clock(Some(view.today), true);
        let ages = crate::corporate_auto::evidence::audit()
            .items
            .iter()
            .find(|item| item.id.to_string() == entry.program_id)
            .map_or(
                Ages {
                    min: None,
                    max: None,
                },
                |item| Ages {
                    min: item.min_age,
                    max: item.max_age,
                },
            );
        // Live hides on any failing fragment; hide_only, as production does,
        // on any fragment whose reason is allowlisted (every fragment read).
        let mut decision: Option<(String, String)> = None;
        let mut trusted: Option<(String, String)> = None;
        let mut any_quote = false;
        for fragment in &entry.fragments {
            let resolution = resolve(
                &view,
                &StoredFact {
                    quote: fragment,
                    baseline: None,
                    // The programme's stored ages, as production reads them.
                    ages,
                    adoption_open: true,
                    anchors: &Anchors::default(),
                },
            );
            any_quote |= !matches!(resolution, Resolution::Exact(FactOutcome::QuoteMissing));
            if resolution.confirms() {
                continue;
            }
            let key = match &resolution {
                Resolution::Ambiguous(reason) => reason_key(reason).unwrap_or("other").to_string(),
                other => other.label().to_string(),
            };
            let text = matched_text(&view, fragment, &key, &clock);
            if trusted.is_none()
                && crate::corporate_auto::DEFAULT_HIDE_REASONS.contains(&key.as_str())
            {
                trusted = Some((key.clone(), text.clone()));
            }
            if decision.is_none() {
                decision = Some((key, text));
            }
        }
        let (live, hide_only, reason, text) = match (&trusted, &decision) {
            (Some((key, text)), _) => ("hidden", "hidden", key.clone(), text.clone()),
            (None, Some((key, text))) => ("hidden", "shown", key.clone(), text.clone()),
            (None, None) => ("shown", "shown", String::new(), String::new()),
        };
        live_hides += usize::from(live == "hidden");
        hide_only_hides += usize::from(hide_only == "hidden");
        rows.push(format!(
            "| {} | {} | {live} | {hide_only} | {reason} | {} |{}",
            entry.final_url.as_deref().unwrap_or(&entry.url),
            entry.title,
            text.replace('|', "/"),
            if any_quote || entry.fragments.is_empty() {
                ""
            } else {
                " (quote not on the page)"
            }
        ));
    }
    println!("| url | offer | live | hide_only | reason | matched text |");
    println!("|---|---|---|---|---|---|");
    for row in &rows {
        println!("{row}");
    }
    println!(
        "real pages: {} read, live hides {live_hides}, hide_only hides {hide_only_hides}",
        rows.len()
    );
    // Target before any hide_only enable: no published offer hidden by the
    // default allowlist on its real page.
    assert_eq!(hide_only_hides, 0, "hide_only would hide a real offer");
    // Live (lead decision §23): the reader is never loosened to show an
    // offer; these 8 are recovered by fresh extraction (re-derive and
    // propose). A change to this list is deliberate.
    let hidden: Vec<&str> = rows
        .iter()
        .filter(|row| row.contains("| hidden |"))
        .filter_map(|row| row.split(" | ").next())
        .map(|cell| cell.trim_start_matches("| "))
        .collect();
    assert_eq!(
        hidden,
        [
            "https://aws.amazon.com/ko/education/awseducate/",
            "https://help.figma.com/hc/en-us/articles/360041061214-Figma-for-Education",
            "https://www.lge.co.kr/benefits/exhibitions/detail-PE00518001",
            "https://linear.app/docs/billing-and-plans",
            "https://www.samsungebiz.com/event/galaxycampus/about/?anc=condition",
            "https://www.lguplus.com/benefit-membership/rank-info",
            "https://m.tworld.co.kr/product/callplan?prod_id=NA00009785",
            "https://m.hollys.co.kr/customer/faq.do",
        ],
        "live hides {live_hides}"
    );
}
