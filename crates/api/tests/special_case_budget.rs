//! The agent's special-case budget (integrated review L2): the number of
//! regexes and word lists in `src/agent` may only fall. A new list needs a
//! structural reason (a closed grammatical class, tool names) and raises
//! the budget in the same change, where review sees it.

const REGEX_BUDGET: usize = 7;
const WORD_LIST_BUDGET: usize = 184;

fn sources(dir: &std::path::Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(std::fs::read_to_string(&path).unwrap());
        }
    }
}

#[test]
fn special_cases_stay_within_budget() {
    let mut files = Vec::new();
    sources(
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/agent")),
        &mut files,
    );
    let regexes: usize = files.iter().map(|f| f.matches("Regex::new").count()).sum();
    let lists: usize = files
        .iter()
        .flat_map(|f| f.lines())
        .filter(|l| {
            let l = l.trim_start();
            l.starts_with("const ") && l.contains(": &[&str]") && l.contains('=')
        })
        .count();
    assert!(
        regexes <= REGEX_BUDGET,
        "Regex::new {regexes} > {REGEX_BUDGET}"
    );
    assert!(
        lists <= WORD_LIST_BUDGET,
        "word lists {lists} > {WORD_LIST_BUDGET}"
    );
}
