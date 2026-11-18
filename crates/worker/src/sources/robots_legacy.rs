//! The robots decision as production ran it before 2026-11-11 (base 46a1a390),
//! kept verbatim for the replay that lists which saved hosts change verdict.
#![allow(dead_code)]

#[derive(Debug, Default)]
struct RobotsGroup {
    agents: Vec<String>,
    rules: Vec<RobotsRule>,
    crawl_delay: Option<f64>,
}

#[derive(Debug)]
struct RobotsRule {
    allow: bool,
    pattern: String,
}

fn parse_robots_groups(body: &str) -> Vec<RobotsGroup> {
    let mut groups = Vec::new();
    let mut current = RobotsGroup::default();

    for raw_line in body.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            if !current.agents.is_empty() && !current.rules.is_empty() {
                groups.push(std::mem::take(&mut current));
            }
            continue;
        }
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        let field = field.trim().to_ascii_lowercase();
        let value = value.trim();
        match field.as_str() {
            "user-agent" => {
                if !current.rules.is_empty() {
                    groups.push(std::mem::take(&mut current));
                }
                if !value.is_empty() {
                    current.agents.push(value.to_ascii_lowercase());
                }
            }
            "allow" | "disallow" if !current.agents.is_empty() => {
                if field == "disallow" && value.is_empty() {
                    continue;
                }
                if !value.is_empty() {
                    current.rules.push(RobotsRule {
                        allow: field == "allow",
                        pattern: value.to_string(),
                    });
                }
            }
            "crawl-delay" if !current.agents.is_empty() => {
                current.crawl_delay = value
                    .parse::<f64>()
                    .ok()
                    .filter(|delay| delay.is_finite() && *delay >= 0.0);
            }
            _ => {}
        }
    }
    if !current.agents.is_empty() {
        groups.push(current);
    }
    groups
}

/// A named-agent allowlist `product_token` is not on, read from the file's
/// structure: no group names the token, there is no `*` group, and at least
/// one named group explicitly allows the site root (`Allow: /`), as LG U+,
/// KT, 현대카드 and 하나카드 list `Yeti`, `Googlebot` and others
/// (2026-11-06). RFC 9309 §2.2.1 applies no rule to an agent such a file
/// does not name, but the file states who may crawl. A file that only
/// restricts named agents (`User-agent: Googlebot-Image` /
/// `Disallow: /images`) states no allowlist.
fn is_named_allowlist(groups: &[RobotsGroup], product_token: &str) -> bool {
    !groups.is_empty()
        && matching_robots_groups(groups, product_token).is_empty()
        && groups
            .iter()
            .flat_map(|group| &group.rules)
            .any(|rule| rule.allow && rule.pattern == "/")
}

/// The groups that apply to `product_token`: its own groups when any exist,
/// otherwise the `*` groups (RFC 9309 §2.2.1).
fn matching_robots_groups<'a>(
    groups: &'a [RobotsGroup],
    product_token: &str,
) -> Vec<&'a RobotsGroup> {
    let token = product_token.to_ascii_lowercase();
    let exact: Vec<&RobotsGroup> = groups
        .iter()
        .filter(|group| group.agents.iter().any(|agent| agent == &token))
        .collect();
    if exact.is_empty() {
        groups
            .iter()
            .filter(|group| group.agents.iter().any(|agent| agent == "*"))
            .collect()
    } else {
        exact
    }
}

/// Crawl-delay in seconds from the groups that apply to `product_token`.
/// The largest stated delay wins when several matching groups state one.
pub(crate) fn robots_crawl_delay(body: &str, product_token: &str) -> Option<f64> {
    let groups = parse_robots_groups(body);
    matching_robots_groups(&groups, product_token)
        .iter()
        .filter_map(|group| group.crawl_delay)
        .fold(None, |best: Option<f64>, delay| {
            Some(best.map_or(delay, |b| b.max(delay)))
        })
}

/// The one robots decision every fetch path makes: whether `product_token`
/// may fetch `target` (path plus query) under `body`. A named-agent
/// allowlist the token is not on refuses every path; otherwise the RFC 9309
/// rules of the groups that apply decide.
pub(crate) fn legacy_robots_allows(body: &str, product_token: &str, target: &str) -> bool {
    let groups = parse_robots_groups(body);
    !is_named_allowlist(&groups, product_token)
        && robots_rules_allow(&groups, product_token, target)
}

/// RFC 9309 §2.2.2: the most specific matching rule wins, `Allow` on a tie;
/// no matching rule allows.
fn robots_rules_allow(groups: &[RobotsGroup], product_token: &str, target: &str) -> bool {
    let matching = matching_robots_groups(groups, product_token);

    let mut best: Option<(usize, bool)> = None;
    for rule in matching.iter().flat_map(|group| &group.rules) {
        if !robots_pattern_matches(&rule.pattern, target) {
            continue;
        }
        let specificity = rule
            .pattern
            .trim_end_matches('$')
            .bytes()
            .filter(|byte| *byte != b'*')
            .count();
        match best {
            None => best = Some((specificity, rule.allow)),
            Some((best_specificity, best_allow))
                if specificity > best_specificity
                    || (specificity == best_specificity && rule.allow && !best_allow) =>
            {
                best = Some((specificity, rule.allow));
            }
            _ => {}
        }
    }
    best.map(|(_, allow)| allow).unwrap_or(true)
}

fn robots_pattern_matches(pattern: &str, target: &str) -> bool {
    let exact_end = pattern.ends_with('$');
    let mut pattern = pattern.trim_end_matches('$').as_bytes().to_vec();
    if !exact_end {
        pattern.push(b'*');
    }
    glob_matches(&pattern, target.as_bytes())
}

fn glob_matches(pattern: &[u8], value: &[u8]) -> bool {
    let (mut p, mut v) = (0usize, 0usize);
    let (mut star, mut retry_value) = (None, 0usize);
    while v < value.len() {
        if p < pattern.len() && pattern[p] == value[v] {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry_value = v;
        } else if let Some(star_index) = star {
            retry_value += 1;
            v = retry_value;
            p = star_index + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}
