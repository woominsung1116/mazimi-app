//! Offline replay of the resolver over the deep-link audit's production
//! export: the same programmes, raw fields, gov24 detail rows and fetch
//! results the audit classified. Writes nothing to any database.
//!
//! `LINK_REPLAY_DIR=/path/to/deeplink [LINK_REPLAY_OUT=/path/out.json]
//!  cargo test -p mazimi-worker --lib link_resolver::replay_tests -- --ignored --nocapture`
//!
//! The directory holds `list.json` (visible programmes), `cand.jsonl` (raw
//! link fields per programme), `gov24_detail.jsonl` (serviceDetail rows),
//! `fetch_merged.jsonl` (one check per URL), `rows.json` (the audit's
//! per-programme classes) and `programs.json` (the audit's candidates).
use super::candidates::{self, Origin, ONE_HOP_FIELD, ONE_HOP_RANK};
use super::classify::{self, Context, Kind, Observation, PageFacts};
use super::resolve::{self, CandidateView, CheckState, Choice, Liveness, Shown, Status};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

fn lines(path: &str) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("JSON line"))
        .collect()
}

fn read(path: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}")))
        .expect("JSON file")
}

/// The audit fetcher's record as an [`Observation`].
fn observation(record: &Value) -> Observation {
    if record.get("blocked").is_some() {
        return Observation::Blocked;
    }
    if let Some(error) = record.get("error").and_then(Value::as_str) {
        let parts: Vec<&str> = error.split(':').collect();
        let code = if error.starts_with("conn") && parts.len() > 1 {
            format!("{}:{}", parts[0], parts[1])
        } else {
            parts[0].to_string()
        };
        return Observation::Error { code };
    }
    let url = record["url"].as_str().unwrap();
    let page = record
        .get("page")
        .filter(|p| p.is_object())
        .map(|p| PageFacts {
            title: p["title"].as_str().unwrap_or("").to_string(),
            forms: p["forms"].as_u64().unwrap_or(0) as u32,
            inputs: p["inputs"].as_u64().unwrap_or(0) as u32,
            textareas: p["textareas"].as_u64().unwrap_or(0) as u32,
            selects: p["selects"].as_u64().unwrap_or(0) as u32,
            password: p["password"].as_bool().unwrap_or(false),
            apply_links: p["apply_links"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|l| {
                            Some((l[0].as_str()?.to_string(), l[1].as_str()?.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        });
    Observation::Fetched {
        status: record["status"].as_u64().unwrap_or(200) as u16,
        final_url: record["final"].as_str().unwrap_or(url).to_string(),
        page,
    }
}

/// The programme's raw payload, rebuilt from the audit's export columns.
fn raw_payload(c: &Value) -> Value {
    let rp = &c["rp"];
    let s = |k: &str| rp.get(k).cloned().unwrap_or(Value::Null);
    match c["src"].as_str().unwrap() {
        "youth_center" => {
            json!({"aplyUrlAddr": s("aply"), "refUrlAddr1": s("ref1"), "refUrlAddr2": s("ref2"),
            "plcyAplyMthdCn": s("mthd"), "etcMttrCn": s("etc"), "plcyNo": s("plcyNo")})
        }
        "gov_benefits" => {
            json!({"상세조회URL": s("dtl"), "접수기관": s("rcv"), "신청기한": s("deadline"), "서비스ID": s("sid")})
        }
        "local_welfare" | "national_welfare" => json!({"servDtlLink": s("dtl")}),
        "scholarship" => json!({"홈페이지 주소": s("home")}),
        _ => json!({}),
    }
}

fn title_key(title: &str) -> String {
    title
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(30)
        .collect()
}

struct Programme {
    id: String,
    src: String,
    title: String,
    cands: Vec<candidates::Candidate>,
}

#[derive(Clone)]
struct Pick {
    letter: char,
    sub: String,
    field: String,
    url: Option<String>,
    soft: bool,
}

fn none_pick() -> Pick {
    Pick {
        letter: 'f',
        sub: "none".into(),
        field: String::new(),
        url: None,
        soft: false,
    }
}

#[test]
#[ignore = "needs LINK_REPLAY_DIR with the audit export"]
fn replay_audit_export() {
    let Ok(dir) = std::env::var("LINK_REPLAY_DIR") else {
        return;
    };
    let path = |name: &str| format!("{dir}/{name}");
    let list = read(&path("list.json"));
    let titles: HashMap<String, String> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["id"].as_str().unwrap().to_string(),
                i["title"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let cand: HashMap<String, Value> = lines(&path("cand.jsonl"))
        .into_iter()
        .map(|c| (c["id"].as_str().unwrap().to_string(), c))
        .collect();
    let details: HashMap<String, Value> = lines(&path("gov24_detail.jsonl"))
        .into_iter()
        .map(|d| (d["서비스ID"].as_str().unwrap_or("").to_string(), d))
        .collect();
    let mut fetches: HashMap<String, Value> = lines(&path("fetch_merged.jsonl"))
        .into_iter()
        .map(|r| (r["url"].as_str().unwrap().to_string(), r))
        .collect();
    let audit_urls: HashSet<String> = fetches.keys().cloned().collect();
    // Checks of one-hop targets made by `check_one_hop_targets`.
    if let Ok(extra) = std::env::var("LINK_REPLAY_EXTRA") {
        for r in lines(&extra) {
            fetches
                .entry(r["url"].as_str().unwrap().to_string())
                .or_insert(r);
        }
    }
    let audit: HashMap<String, Value> = read(&path("rows.json"))
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["id"].as_str().unwrap().to_string(), r.clone()))
        .collect();

    // Navigation links: (request host, href) -> fetched pages carrying it.
    let mut nav: HashMap<(String, String), usize> = HashMap::new();
    for (url, record) in fetches.iter().filter(|(u, _)| audit_urls.contains(*u)) {
        let host = classify::host_of(url);
        // As the audit counted: distinct (text, href) pairs per page.
        let pairs: HashSet<(String, String)> = record["page"]["apply_links"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|l| Some((l[0].as_str()?.to_string(), l[1].as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        for (_, href) in pairs {
            *nav.entry((host.clone(), href)).or_default() += 1;
        }
    }
    // Hosts that only ever timed out from the checker.
    let mut host_outcomes: HashMap<String, (usize, usize)> = HashMap::new();
    for (url, record) in &fetches {
        let e = host_outcomes.entry(classify::host_of(url)).or_default();
        e.0 += 1;
        if record.get("error").and_then(Value::as_str) == Some("timeout") {
            e.1 += 1;
        }
    }
    let timeout_host = |host: &str| {
        host_outcomes
            .get(host)
            .is_some_and(|(n, t)| *n > 0 && n == t)
    };

    let mut programmes = Vec::new();
    for item in list.as_array().unwrap() {
        let id = item["id"].as_str().unwrap().to_string();
        let c = &cand[&id];
        let raw = raw_payload(c);
        let detail = (c["src"] == "gov_benefits").then(|| {
            details
                .get(c["rp"]["sid"].as_str().unwrap_or(""))
                .cloned()
                .unwrap_or(json!({}))
        });
        let extracted = candidates::extract(
            c["src"].as_str().unwrap(),
            c["official"].as_str(),
            &raw,
            detail.as_ref(),
            &[],
        );
        programmes.push(Programme {
            id: id.clone(),
            src: c["src"].as_str().unwrap().to_string(),
            title: titles[&id].clone(),
            cands: extracted.candidates,
        });
    }
    // Shared: two or more differently titled programmes list the URL.
    let owners: std::cell::RefCell<HashMap<String, HashSet<String>>> = Default::default();
    for p in &programmes {
        for c in &p.cands {
            owners
                .borrow_mut()
                .entry(c.url.clone())
                .or_default()
                .insert(title_key(&p.title));
        }
    }

    let simulated = std::cell::Cell::new(0usize);
    // One candidate, checked once as the audit did.
    let evaluate = |url: &str,
                    field: &str,
                    origin: Origin,
                    free_text: bool,
                    programme: &str,
                    notice_title: Option<&str>|
     -> (Choice, Option<Observation>) {
        let view = CandidateView {
            url,
            field,
            origin,
            free_text,
            programme,
            notice_title,
        };
        let host = classify::host_of(url);
        let nav_count = |href: &str| {
            nav.get(&(host.clone(), href.to_string()))
                .copied()
                .unwrap_or(0)
        };
        let ctx = Context {
            shared: owners.borrow().get(url).map_or(0, HashSet::len) >= 2,
            nav_count: &nav_count,
        };
        let skipped = origin == Origin::Rejected || classify::portal_kind(url).is_some();
        let observed = fetches.get(url).map(observation).or_else(|| {
            // A repaired URL on a host that only timed out: the checker will
            // see a timeout too.
            (!skipped && timeout_host(&host)).then(|| {
                simulated.set(simulated.get() + 1);
                Observation::Error {
                    code: "timeout".into(),
                }
            })
        });
        let (status, shown_from) = if skipped {
            (Status::Skipped, None)
        } else {
            match &observed {
                None => (Status::Unchecked, None),
                Some(obs) => {
                    let live = resolve::liveness(view, obs);
                    let state = resolve::observe(&CheckState::default(), &live, chrono::Utc::now());
                    let from = match live {
                        Liveness::Pass => Some(obs.clone()),
                        Liveness::Blocked => Some(Observation::Blocked),
                        _ => None,
                    };
                    (state.status, from)
                }
            }
        };
        let shown = match status {
            Status::Unchecked | Status::Broken | Status::HardFail => None,
            _ => resolve::shown(view, shown_from.as_ref(), &ctx),
        };
        (
            Choice {
                shown,
                field_rank: 0,
                status,
                demoted: false,
                current: false,
                unconfirmed_hard: false,
            },
            if status == Status::Ok { observed } else { None },
        )
    };

    // Application links on the notice page a programme would show.
    let hop_targets = |p: &Programme,
                       choices: &[Choice],
                       obs_of: &[Option<Observation>],
                       notice: Option<usize>| {
        let mut targets: Vec<String> = Vec::new();
        let Some(n) = notice.filter(|n| {
            choices[*n]
                .shown
                .as_ref()
                .is_some_and(|s| s.kind == Kind::Notice)
        }) else {
            return targets;
        };
        let Some(Observation::Fetched {
            final_url,
            page: Some(page),
            ..
        }) = &obs_of[n]
        else {
            return targets;
        };
        // As in production: only a page that stayed on its own site.
        let site = |u: &str| mazimi_core::source_policy::registrable_domain(&classify::host_of(u));
        if site(&p.cands[n].url) != site(final_url) {
            return targets;
        }
        let host = classify::host_of(&p.cands[n].url);
        for (text, href) in &page.apply_links {
            let is_nav = nav.get(&(host.clone(), href.clone())).copied().unwrap_or(0) >= 3;
            if !classify::is_promotable_hop_text(text) || is_nav {
                continue;
            }
            // The checker unescapes hrefs (`page::analyze`); the audit did not.
            if let Some(t) = candidates::one_hop_target(final_url, &href.replace("&amp;", "&")) {
                if !targets.contains(&t) && !p.cands.iter().any(|c| c.url == t) && targets.len() < 3
                {
                    targets.push(t);
                }
            }
        }
        targets
    };
    let assess = |p: &Programme| {
        let mut choices = Vec::new();
        let mut obs_of = Vec::new();
        for c in &p.cands {
            let (mut choice, obs) =
                evaluate(&c.url, c.field, c.origin, c.free_text, &p.title, None);
            choice.field_rank = c.field_rank;
            choices.push(choice);
            obs_of.push(obs);
        }
        (choices, obs_of)
    };
    // One-hop rows are candidates too: they count towards "shared".
    for p in &programmes {
        let (choices, obs_of) = assess(p);
        let (_, notice) = resolve::select(&choices);
        for t in hop_targets(p, &choices, &obs_of, notice) {
            owners
                .borrow_mut()
                .entry(t)
                .or_default()
                .insert(title_key(&p.title));
        }
    }
    simulated.set(0);

    let mut before: BTreeMap<String, BTreeMap<char, usize>> = BTreeMap::new();
    let mut audit_best: BTreeMap<String, BTreeMap<char, usize>> = BTreeMap::new();
    let mut after: BTreeMap<String, BTreeMap<char, usize>> = BTreeMap::new();
    let mut with_hop: BTreeMap<String, BTreeMap<char, usize>> = BTreeMap::new();
    let mut transitions: BTreeMap<(char, char), usize> = BTreeMap::new();
    let mut subs: BTreeMap<(char, String), usize> = BTreeMap::new();
    let mut hop_pending = 0usize;
    let mut pending_targets: Vec<String> = Vec::new();
    let mut hop_promoted = 0usize;
    let mut soft_selected = 0usize;
    let mut out_rows = Vec::new();
    for p in &programmes {
        let (choices, obs_of) = assess(p);
        let pick_of = |choices: &[Choice], i: Option<usize>, extra: &[(String, String)]| -> Pick {
            let Some(i) = i else { return none_pick() };
            let s: &Shown = choices[i].shown.as_ref().unwrap();
            let (field, url) = if i < p.cands.len() {
                (p.cands[i].field.to_string(), p.cands[i].url.clone())
            } else {
                extra[i - p.cands.len()].clone()
            };
            Pick {
                letter: s.kind.letter(),
                sub: s.sub.clone(),
                field,
                url: Some(url),
                soft: choices[i].status == Status::SoftFail,
            }
        };
        let (best, notice) = resolve::select(&choices);
        let first = pick_of(&choices, best, &[]);
        // Design 5.8 on the notice page the programme would show.
        let mut hop_choices = choices.clone();
        let mut extra = Vec::new();
        for (k, t) in hop_targets(p, &choices, &obs_of, notice)
            .into_iter()
            .enumerate()
        {
            if fetches.contains_key(&t) {
                // A one-hop link is vouched for exactly as its notice page is.
                let n = notice.unwrap();
                let parent = &p.cands[n];
                let parent_title = match &obs_of[n] {
                    Some(Observation::Fetched {
                        page: Some(page), ..
                    }) => Some(page.title.as_str()),
                    _ => None,
                };
                let (mut choice, _) = evaluate(
                    &t,
                    ONE_HOP_FIELD,
                    parent.origin,
                    parent.free_text,
                    &p.title,
                    parent_title,
                );
                choice.field_rank = ONE_HOP_RANK + k as i16;
                hop_choices.push(choice);
                extra.push((ONE_HOP_FIELD.to_string(), t));
            } else {
                hop_pending += 1;
                pending_targets.push(t);
            }
        }
        let (hop_best, _) = resolve::select(&hop_choices);
        let second = pick_of(&hop_choices, hop_best, &extra);
        if second.letter != first.letter {
            hop_promoted += 1;
        }
        if first.soft {
            soft_selected += 1;
        }
        let row = &audit[&p.id];
        let cur = row["cur"][0].as_str().unwrap().chars().next().unwrap();
        let best_py = row["best"][0].as_str().unwrap().chars().next().unwrap();
        for (table, letter) in [
            (&mut before, cur),
            (&mut audit_best, best_py),
            (&mut after, first.letter),
            (&mut with_hop, second.letter),
        ] {
            *table
                .entry(p.src.clone())
                .or_default()
                .entry(letter)
                .or_default() += 1;
            *table
                .entry("ALL".into())
                .or_default()
                .entry(letter)
                .or_default() += 1;
        }
        *transitions.entry((cur, second.letter)).or_default() += 1;
        *subs.entry((second.letter, second.sub.clone())).or_default() += 1;
        out_rows.push(json!({"id": p.id, "src": p.src, "title": p.title,
            "before": [cur.to_string(), row["cur"][1], row["cur"][2]],
            "after": [second.letter.to_string(), second.sub, second.field, second.url],
            "soft_failure": second.soft}));
    }

    let print = |name: &str, table: &BTreeMap<String, BTreeMap<char, usize>>| {
        println!("== {name}");
        let mut keys: Vec<&String> = table.keys().collect();
        keys.sort_by_key(|k| std::cmp::Reverse(table[*k].values().sum::<usize>()));
        for k in keys {
            let c = &table[k];
            let n: usize = c.values().sum();
            let pct = |l: char| 100.0 * *c.get(&l).unwrap_or(&0) as f64 / n as f64;
            let cells: Vec<String> = "abcdef"
                .chars()
                .map(|l| format!("{l}={:5}({:5.1}%)", c.get(&l).unwrap_or(&0), pct(l)))
                .collect();
            println!(
                "{k:20} n={n:5} {}  a+b={:5.1}%",
                cells.join(" "),
                pct('a') + pct('b')
            );
        }
    };
    print("before (audit: official_url today)", &before);
    print("audit best (classify.py)", &audit_best);
    print("after (resolver, no one-hop)", &after);
    print(
        "after (resolver + one-hop with a recorded check)",
        &with_hop,
    );
    println!("one-hop: programmes changed by a promotion {hop_promoted}, targets pending a first check {hop_pending}");
    println!("selected with a soft failure (timeout/403/5xx, pattern-only, capped at notice): {soft_selected}");
    println!(
        "repaired URLs given the host's timeout: {}",
        simulated.get()
    );
    println!("== transitions before -> after");
    for ((a, b), n) in &transitions {
        println!("{a}->{b} {n}");
    }
    println!("== after sub");
    let mut subs: Vec<_> = subs.into_iter().collect();
    subs.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for ((l, s), n) in subs {
        println!("{l} {s} {n}");
    }
    if let Ok(out) = std::env::var("LINK_REPLAY_HOPS_OUT") {
        std::fs::write(out, serde_json::to_string(&pending_targets).unwrap()).unwrap();
    }
    if let Ok(out) = std::env::var("LINK_REPLAY_OUT") {
        std::fs::write(out, serde_json::to_string(&out_rows).unwrap()).unwrap();
    }
    let broken_after: usize = with_hop["ALL"].get(&'e').copied().unwrap_or(0);
    assert_eq!(broken_after, 0, "no programme may show a broken link");
}

/// Rule parity with the audit: every candidate URL the audit classified
/// gets the same class and sub-reason from the Rust port (same fetch
/// record, same shared rule).
#[test]
#[ignore = "needs LINK_REPLAY_DIR with the audit export"]
fn classifier_matches_the_audit_per_url() {
    let Ok(dir) = std::env::var("LINK_REPLAY_DIR") else {
        return;
    };
    let path = |name: &str| format!("{dir}/{name}");
    let fetches: HashMap<String, Value> = lines(&path("fetch_merged.jsonl"))
        .into_iter()
        .map(|r| (r["url"].as_str().unwrap().to_string(), r))
        .collect();
    let titles: HashMap<String, String> = read(&path("list.json"))
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["id"].as_str().unwrap().to_string(),
                i["title"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let programs = read(&path("programs.json"));
    let mut owners: HashMap<String, HashSet<String>> = HashMap::new();
    for p in programs.as_array().unwrap() {
        for c in p["cands"].as_array().unwrap() {
            if let Some(u) = c["url"].as_str() {
                owners
                    .entry(u.to_string())
                    .or_default()
                    .insert(title_key(&titles[p["id"].as_str().unwrap()]));
            }
        }
    }
    let mut nav: HashMap<(String, String), usize> = HashMap::new();
    for (url, record) in &fetches {
        let host = classify::host_of(url);
        // As the audit counted: distinct (text, href) pairs per page.
        let pairs: HashSet<(String, String)> = record["page"]["apply_links"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|l| Some((l[0].as_str()?.to_string(), l[1].as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        for (_, href) in pairs {
            *nav.entry((host.clone(), href)).or_default() += 1;
        }
    }
    let (mut same, mut total) = (0usize, 0usize);
    let mut diffs: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen = HashSet::new();
    for row in read(&path("rows.json")).as_array().unwrap() {
        for c in row["cands"].as_array().unwrap() {
            let Some(url) = c[1].as_str() else { continue };
            if !seen.insert(url.to_string()) {
                continue;
            }
            let host = classify::host_of(url);
            let nav_count = |href: &str| {
                nav.get(&(host.clone(), href.to_string()))
                    .copied()
                    .unwrap_or(0)
            };
            let ctx = Context {
                shared: owners.get(url).map_or(0, HashSet::len) >= 2,
                nav_count: &nav_count,
            };
            let obs = fetches.get(url).map(observation);
            let class = classify::classify(url, obs.as_ref(), &ctx);
            let expected = (
                c[2].as_str().unwrap().to_string(),
                c[3].as_str().unwrap().to_string(),
            );
            let got = (class.letter().to_string(), class.sub().to_string());
            total += 1;
            if got == expected {
                same += 1;
            } else {
                *diffs.entry(format!("{expected:?} -> {got:?}")).or_default() += 1;
                if diffs.len() <= 15 && diffs[&format!("{expected:?} -> {got:?}")] == 1 {
                    println!("diff {url}: {expected:?} -> {got:?}");
                }
            }
        }
    }
    println!("parity {same}/{total}");
    for (d, n) in &diffs {
        println!("{n:5} {d}");
    }
    assert!(
        same as f64 >= 0.995 * total as f64,
        "classifier drifted from the audit"
    );
}

/// Live checks of the one-hop targets the replay could not judge, with the
/// production checker (robots, pacing, redirects). Writes fetch records in
/// the audit format for `LINK_REPLAY_EXTRA`.
/// `LINK_REPLAY_HOPS_IN=targets.json LINK_REPLAY_EXTRA_OUT=extra.jsonl TEST_DATABASE_URL=...`
#[tokio::test]
#[ignore = "fetches the public web; needs LINK_REPLAY_HOPS_IN and a test database"]
async fn check_one_hop_targets() {
    let (Ok(input), Ok(output)) = (
        std::env::var("LINK_REPLAY_HOPS_IN"),
        std::env::var("LINK_REPLAY_EXTRA_OUT"),
    ) else {
        return;
    };
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../infra/migrations")
        .run(&pool)
        .await
        .unwrap();
    let targets: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(input).unwrap()).unwrap();
    let user_agent = crate::sources::http::public_api_user_agent();
    let pacer = super::check::Pacer::default();
    let mut out = String::new();
    for target in targets {
        let record = match super::check::check(&pool, &target, &user_agent, &pacer).await {
            Observation::Blocked => json!({"url": target, "blocked": "rules"}),
            Observation::Error { code } => json!({"url": target, "error": code}),
            Observation::Fetched {
                status,
                final_url,
                page,
            } => json!({"url": target, "status": status, "final": final_url,
                "page": page.map(|p| json!({"title": p.title, "forms": p.forms, "inputs": p.inputs, "textareas": p.textareas,
                    "selects": p.selects, "password": p.password, "apply_links": p.apply_links}))}),
        };
        println!("{record}");
        out.push_str(&record.to_string());
        out.push('\n');
    }
    std::fs::write(output, out).unwrap();
}
