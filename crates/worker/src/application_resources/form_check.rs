//! Is a downloaded file the programme's blank application form? The
//! download-only tier ("공식 양식 받기") publishes a template candidate on
//! this decision alone: no field overlay, no prefill, so only "is this the
//! programme's form" has to be right. It runs inside the worker, on the
//! bytes the probe already fetched and hashed, without a converter:
//!
//! - **form** — HWP/HWPX only: the document's own opening text (the `PrvText`
//!   preview the editor stores) starts with a form title (a 서식 marker such
//!   as `[별지 제1호서식]` may come first) and shows a blank table cell; a
//!   file that opens with a notice (공고) qualifies only through a short body
//!   heading that ends in a form word and names the programme (the form the
//!   notice carries), not a row of a numbered document list.
//!   PDF: the first page's text layer, read by poppler's `pdftotext` in a
//!   child process ([`pdf_first_page_text`]), opens with a form title by the
//!   same [`opening_title`] rule. A PDF without extractable text (a scan) is
//!   refused; PDF text carries no table-cell marks, so the blank-cell test
//!   is HWP-only, and a notice PDF's form on a later page is not looked for.
//! - **year** — no earlier year in that opening: 20XX년, or on a form's own
//!   opening also 20XX. / 20XX- (its signature date, its 접수번호); for a
//!   notice packet, 20XX년 in the notice's opening and the form's heading.
//! - **programme** — files a 복지로 detail API lists for the service are its
//!   files by provenance. Any other file's form title is matched by
//!   [`programme_match`]: accepted when it names at least half of the
//!   programme's own part (its words less the qualifiers the catalogue of
//!   current titles marks, [`Vocabulary`]: 저소득, 장애인, 신혼부부 name who,
//!   not what); refused when it names none of it (at most a shared target
//!   group); anything between is ambiguous and goes to the local model
//!   (`form_judge`), or stays unpublished.
//! - **applicant marks (PDF)** — a PDF's first page must also carry what an
//!   applicant's form has and a plan or report does not: a signature or seal
//!   line, an applicant block or a check box ([`APPLICANT_MARKS`]).
//!
//! The form and notice words are the forms pipeline's page classifier's
//! (`scripts/forms/auto/fidelity_gate.py` `FORM_TITLE`, `NOT_FORM`), not a new
//! list. Measured against eye checks in `.reports/forms-server.md`.

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

use super::discovery;

/// The pipeline's form-title words (`fidelity_gate.FORM_TITLE`).
static FORM_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("(신청서|동의서|확인서|서약서|신고서|계획서|소개서|지원서|명세서|청구서|각서|이행서|확약서)").unwrap()
});
/// The pipeline's notice/instruction headings (`fidelity_gate.NOT_FORM`).
static NOT_FORM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(공고|작성요령|작성방법|유의사항|안내문|제출서류목록)").unwrap());
/// What may precede a form's title: brackets, bullets, an empty cell, a
/// 서식 or 붙임 number, a page side.
static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[<>\[\]【】()（）■□◆○※]|별지제?\d*호?서식|서식\d*|붙임\d*|첨부\d*|앞쪽|뒤쪽|\d+[.)])+").unwrap()
});
static EARLIER_YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(20\d\d)년").unwrap());
/// A form's own dates also print as '2024. . .' (the signature line) or
/// '(2024- )' (the 접수번호); in a notice those are its 공고 number, so they
/// count only on a form's own opening.
static FORM_YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(20\d\d)(?:년|[.\-])").unwrap());
static TRAILING_BRACKET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[(（][^)）]*[)）]$").unwrap());

/// Bounds on what a file may make the worker read.
const MAX_PREVIEW_BYTES: u64 = 64 * 1024;
const MAX_SECTIONS: usize = 64;
const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PARAGRAPHS: usize = 20_000;
const MAX_PDF_TEXT_BYTES: u64 = 256 * 1024;
/// Address space a `pdftotext` child may map (Linux): a PDF bomb fails, it
/// does not take the worker host's memory.
#[cfg(target_os = "linux")]
const PDF_TEXT_MAX_ADDRESS_SPACE: u64 = 512 * 1024 * 1024;
/// What an applicant's form carries on its first page and a filled plan or
/// report does not: a signature or seal line, an applicant block. A box
/// counts only in its role as a tick box ([`tick_boxes`]): plans use `□` as
/// a section bullet.
pub const APPLICANT_MARKS: [&str; 3] = ["(서명", "(인)", "신청인"];
/// Text after a tick box: a short option ('아동복지시설', '재참여(이전 참여횟수').
const TICK_OPTION_CHARS: usize = 15;
/// A line between two sibling boxes may hold only this much punctuation
/// ('(', ')', '/'): a table cell's bracket, never content.
const TICK_FILLER_CHARS: usize = 3;
/// First-page text kept for the local model's question, before quoting.
const EXCERPT_CHARS: usize = 600;
/// Declaration text read for the form's year, from '위와 같이'.
const DECLARATION_CHARS: usize = 80;
/// First-page text searched for the place a form names: the whole excerpt,
/// so the addressee line at its foot ('홍천군수 귀하') counts.
const EXCERPT_PLACE_CHARS: usize = EXCERPT_CHARS;
const PDF_TEXT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub publish: bool,
    /// `form` (the file opens with its form), `packet` (a notice carrying
    /// its form), or why it is not published.
    pub reason: &'static str,
    /// The form title the decision rests on (shown to reviewers).
    pub title: String,
    /// The opening text the decision read (for the local model's question
    /// when the programme match is ambiguous; never shown).
    pub excerpt: String,
    /// The local model's answer when it was asked (`form_judge`).
    pub model: Option<bool>,
}

fn refuse(reason: &'static str, title: impl Into<String>) -> Decision {
    Decision {
        publish: false,
        reason,
        title: title.into(),
        excerpt: String::new(),
        model: None,
    }
}

fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The form title a text opens with, if it does: within 40 characters of
/// the start once markers are skipped, with no notice heading before it.
pub fn opening_title(text: &str) -> Option<String> {
    // Spaces go, line breaks stay: a title ends its line or its table cell.
    let text: String = text
        .chars()
        .filter(|c| !c.is_whitespace() || *c == '\n')
        .collect();
    let rest = MARKER.replace(text.trim_start_matches('\n'), "");
    let rest = rest.trim_start_matches('\n');
    let window: String = rest.chars().take(60).collect();
    let m = FORM_TITLE.find(&window)?;
    let before = &window[..m.start()];
    let ends = window[m.end()..]
        .chars()
        .next()
        .is_none_or(|c| "\n<>()[]【】※".contains(c));
    (before.chars().count() <= 40 && !before.contains('\n') && ends && !NOT_FORM.is_match(before))
        .then(|| window[..m.end()].to_string())
}

/// The programme match for a file not listed by the programme's own API:
/// a shared distinctive word of three or more syllables, two shared words,
/// or four shared syllable pairs. A two-syllable word alone ('울산', '드림')
/// is a place or a fragment, not the programme.
pub fn strict_match(title: &str, program: &str) -> bool {
    let words = discovery::distinctive_words(title);
    let program_words = discovery::distinctive_words(program);
    let shared: std::collections::HashSet<&String> = words
        .iter()
        .flat_map(|w| {
            program_words
                .iter()
                .filter(move |p| w.contains(p.as_str()) || p.contains(w.as_str()))
                .map(move |p| {
                    if w.chars().count() <= p.chars().count() {
                        w
                    } else {
                        p
                    }
                })
        })
        .collect();
    shared.iter().any(|w| w.chars().count() >= 3)
        || shared.len() >= 2
        || discovery::bigrams(title)
            .intersection(&discovery::bigrams(program))
            .count()
            >= 4
}

/// A word in at least this many current programme titles that ends fewer
/// than a fifth of them is a qualifier.
const QUALIFIER_MIN_TITLES: u32 = 10;

/// How the catalogue of current programme titles uses each programme word:
/// in how many titles it occurs, and in how many it is the last word (the
/// thing given). Built from the titles at the start of a run, so it follows
/// the catalogue instead of a word list.
#[derive(Debug, Default, Clone)]
pub struct Vocabulary {
    words: HashMap<String, (u32, u32)>,
    /// Place names the catalogue's providers use (a 시/군/구, a province),
    /// each with the region codes most of its programmes carry, so a renamed
    /// or merged province (전라남도, 전남광주통합특별시) still compares by code.
    places: HashMap<String, HashSet<String>>,
    /// The district codes each 시/군/구 name carries in the catalogue (a
    /// renamed district keeps its code: 인천 동구 and 제물포구 are both 28125;
    /// a name several cities share, 동구, carries all of theirs).
    districts: HashMap<String, HashSet<String>>,
}

/// One programme of the catalogue, or the one a file is decided for.
#[derive(Debug, Clone, Copy)]
pub struct Programme<'a> {
    pub title: &'a str,
    pub provider: &'a str,
    pub regions: &'a [String],
    pub districts: &'a [String],
}

static MUNICIPALITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("^[가-힣]{1,4}[시군구]$").unwrap());
static PROVINCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        "^(?:[가-힣]{2,4}도|[가-힣]{2,6}(?:특별시|광역시|특별자치시|특별자치도|통합특별시))$",
    )
    .unwrap()
});

fn hangul_runs(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !('가'..='힣').contains(&c))
        .filter(|t| !t.is_empty())
}

/// Every place that is not a province (서울특별시, 부산광역시 end in 시 too).
fn is_municipality(place: &str) -> bool {
    !PROVINCE.is_match(place)
}

impl Vocabulary {
    pub fn from_catalogue<'a>(programmes: impl IntoIterator<Item = Programme<'a>>) -> Self {
        let mut words: HashMap<String, (u32, u32)> = HashMap::new();
        let mut seen: HashMap<String, HashMap<String, u32>> = HashMap::new();
        let mut districts: HashMap<String, HashSet<String>> = HashMap::new();
        for p in programmes {
            let found = programme_words(p.title);
            for word in &found {
                words.entry(word.clone()).or_default().0 += 1;
            }
            if let Some(last) = found.last() {
                words.entry(last.clone()).or_default().1 += 1;
            }
            let names: HashSet<&str> = hangul_runs(p.provider)
                .filter(|t| {
                    (t.chars().count() >= 2 && MUNICIPALITY.is_match(t)) || PROVINCE.is_match(t)
                })
                .collect();
            for name in names.iter().filter(|n| is_municipality(n)) {
                districts
                    .entry(name.to_string())
                    .or_default()
                    .extend(p.districts.iter().cloned());
            }
            for name in names {
                let codes = seen.entry(name.to_string()).or_default();
                for region in p.regions {
                    *codes.entry(region.clone()).or_default() += 1;
                }
            }
        }
        let places = seen
            .into_iter()
            .filter(|(_, codes)| codes.values().sum::<u32>() >= 2)
            .map(|(name, codes)| {
                let top = codes.values().copied().max().unwrap_or(0);
                let kept = codes
                    .into_iter()
                    .filter(|(_, n)| n * 2 >= top)
                    .map(|(code, _)| code)
                    .collect();
                (name, kept)
            })
            .collect();
        Self {
            words,
            places,
            districts,
        }
    }

    /// Places a text names: whole Hangul runs, or, in a compacted title,
    /// any occurrence, a longer name winning over one inside it (강남구 over 남구).
    fn places_in(&self, text: &str, whole_runs: bool) -> HashSet<&str> {
        if whole_runs {
            return hangul_runs(text)
                .filter_map(|t| self.places.get_key_value(t).map(|(k, _)| k.as_str()))
                .collect();
        }
        let text = compact(text);
        let hits: Vec<(usize, usize, &str)> = self
            .places
            .keys()
            .flat_map(|name| {
                text.match_indices(name.as_str())
                    .map(move |(at, m)| (at, at + m.len(), name.as_str()))
            })
            .collect();
        hits.iter()
            .filter(|(s, e, _)| {
                !hits
                    .iter()
                    .any(|(s2, e2, _)| s2 <= s && e <= e2 && e2 - s2 > e - s)
            })
            .map(|(_, _, name)| *name)
            .collect()
    }

    /// Where the file says it belongs against where the programme is:
    /// `Some(true)` when they differ for certain (another province by region
    /// code, or another 시/군/구 by name), `Some(false)` when a municipal form
    /// meets a province-wide programme (not certain either way), `None` when
    /// the file names no place or the places agree.
    pub fn other_place(
        &self,
        form_title: &str,
        excerpt: &str,
        programme: &Programme,
    ) -> Option<bool> {
        let head: String = excerpt.chars().take(EXCERPT_PLACE_CHARS).collect();
        let form: HashSet<&str> = self
            .places_in(form_title, false)
            .into_iter()
            .chain(self.places_in(&head, true))
            .collect();
        if form.is_empty() {
            return None;
        }
        let own: HashSet<&str> = self
            .places_in(programme.provider, true)
            .into_iter()
            .chain(self.places_in(programme.title, true))
            .collect();
        let form_codes: HashSet<&String> = form
            .iter()
            .filter(|p| !is_municipality(p))
            .flat_map(|p| self.places[*p].iter())
            .collect();
        let own_codes: HashSet<&String> = programme.regions.iter().collect();
        if !form_codes.is_empty() && !own_codes.is_empty() && form_codes.is_disjoint(&own_codes) {
            return Some(true);
        }
        let form_municipal: HashSet<&&str> = form.iter().filter(|p| is_municipality(p)).collect();
        let own_municipal: HashSet<&&str> = own.iter().filter(|p| is_municipality(p)).collect();
        // A 시/군/구 compares by district code where the programme has one (a
        // renamed district keeps its code), else by name.
        let form_districts: HashSet<&String> = form_municipal
            .iter()
            .filter_map(|p| self.districts.get(**p))
            .flatten()
            .collect();
        let own_districts: HashSet<&String> = programme.districts.iter().collect();
        let other_municipality = if !own_districts.is_empty() && !form_districts.is_empty() {
            form_districts.is_disjoint(&own_districts)
        } else {
            !form_municipal.is_empty()
                && !own_municipal.is_empty()
                && form_municipal.is_disjoint(&own_municipal)
        };
        if other_municipality {
            return Some(true);
        }
        let own_is_municipal = !own_municipal.is_empty() || !own_districts.is_empty();
        (!form_municipal.is_empty() && !own_is_municipal && !own_codes.is_empty()).then_some(false)
    }

    /// Common across titles but rarely the last word: it says who or what
    /// kind (저소득, 장애인, 신혼부부), and on its own it matches unrelated
    /// programmes.
    pub fn qualifier(&self, word: &str) -> bool {
        self.words
            .get(word)
            .is_some_and(|&(n, head)| n >= QUALIFIER_MIN_TITLES && head * 5 < n)
    }
}

/// A programme title's words in order, without places, institutions and
/// generic words. Unlike [`discovery::distinctive_words`], a place is
/// recognised on the word as written ('시험응시료' is no 시), and a suffix is
/// stripped only when two syllables remain ('연금' stays 연금).
pub fn programme_words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for token in discovery::tokens(text) {
        let place = (token.chars().count() >= 3
            && ["시", "군", "구", "시청", "군청", "구청"]
                .iter()
                .any(|s| token.ends_with(s)))
            || ["센터", "재단"].iter().any(|s| token.ends_with(s))
            || discovery::SHORT_DISTRICTS.contains(&token.as_str());
        if place {
            continue;
        }
        let mut word = token.clone();
        if let Some(suffix) = discovery::WORD_SUFFIXES
            .iter()
            .find(|s| token.ends_with(**s))
        {
            let rest = &token[..token.len() - suffix.len()];
            if rest.chars().count() >= 2 {
                word = rest.to_string();
            }
        }
        if let Some(rest) = word.strip_prefix("청년") {
            if rest.chars().count() >= 2 {
                word = rest.to_string();
            }
        }
        if !discovery::GENERIC_WORDS.contains(&word.as_str()) && !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

/// The rules' answer to "is this file this programme's form" for a file the
/// programme's own API does not list: another place is certain; a municipal
/// form for a province-wide programme cannot be accepted by words alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    OtherPlace,
    Words(ProgrammeMatch),
}

pub fn programme_verdict(
    form_title: &str,
    excerpt: &str,
    programme: &Programme,
    vocabulary: &Vocabulary,
) -> Verdict {
    let place = vocabulary.other_place(form_title, excerpt, programme);
    if place == Some(true) {
        return Verdict::OtherPlace;
    }
    Verdict::Words(
        match programme_match(form_title, programme.title, vocabulary) {
            ProgrammeMatch::Accept if place == Some(false) => ProgrammeMatch::Ambiguous,
            words => words,
        },
    )
}

/// What the rules can say about a form title and a programme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgrammeMatch {
    /// The title names at least half of the programme's own part.
    Accept,
    /// The title names none of it: at most a shared target group.
    Reject,
    /// Between the two; only the local model may decide, else unpublished.
    Ambiguous,
}

/// The programme's own part is its words less the qualifiers ([`Vocabulary`]);
/// a word that follows a qualifier and forms one with it ('한부모' + '가족')
/// is a qualifier too. Coverage counts the syllables of the own-part words
/// the compacted title contains.
pub fn programme_match(title: &str, program: &str, vocabulary: &Vocabulary) -> ProgrammeMatch {
    let words = programme_words(program);
    let mut qualifier: Vec<bool> = words.iter().map(|w| vocabulary.qualifier(w)).collect();
    for i in 1..words.len() {
        if qualifier[i - 1] && vocabulary.qualifier(&format!("{}{}", words[i - 1], words[i])) {
            qualifier[i] = true;
        }
    }
    let title = compact(title);
    let own: Vec<&String> = words
        .iter()
        .zip(&qualifier)
        .filter(|(_, q)| !**q)
        .map(|(w, _)| w)
        .collect();
    let total: usize = own.iter().map(|w| w.chars().count()).sum();
    let named: usize = own
        .iter()
        .filter(|w| title.contains(w.as_str()))
        .map(|w| w.chars().count())
        .sum();
    if total > 0 && named * 2 >= total && strict_match(&title, program) {
        ProgrammeMatch::Accept
    } else if total > 0 && named == 0 {
        ProgrammeMatch::Reject
    } else {
        ProgrammeMatch::Ambiguous
    }
}

/// The editor's preview text: HWP5 `PrvText` (UTF-16LE), HWPX
/// `Preview/PrvText.txt` (UTF-8). `None` when the file has none.
pub fn preview_text(bytes: &[u8], format: &str) -> Option<String> {
    match format {
        "hwp" => {
            let mut file = cfb::CompoundFile::open(Cursor::new(bytes)).ok()?;
            let mut stream = file.open_stream("/PrvText").ok()?;
            let mut raw = Vec::new();
            (&mut stream)
                .take(MAX_PREVIEW_BYTES)
                .read_to_end(&mut raw)
                .ok()?;
            Some(utf16le(&raw))
        }
        "hwpx" => {
            let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
            let mut entry = zip.by_name("Preview/PrvText.txt").ok()?;
            let mut raw = Vec::new();
            (&mut entry)
                .take(MAX_PREVIEW_BYTES)
                .read_to_end(&mut raw)
                .ok()?;
            Some(String::from_utf8_lossy(&raw).into_owned())
        }
        _ => None,
    }
}

fn utf16le(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The first page's text of a PDF, read by poppler's `pdftotext` in a child
/// process: an untrusted PDF cannot crash or stall the worker (the child is
/// killed after [`PDF_TEXT_TIMEOUT`]), the bytes go through stdin and never
/// to disk, and the child gets no environment beyond `PATH` (the worker's
/// holds credentials). `None` when the tool is missing or fails, or the page
/// has no Hangul text layer (a scan).
pub async fn pdf_first_page_text(bytes: &[u8]) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut command = tokio::process::Command::new("pdftotext");
    command
        .args(["-q", "-enc", "UTF-8", "-f", "1", "-l", "1", "fd://0", "-"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    #[cfg(target_os = "linux")]
    // SAFETY: the closure runs in the forked child before exec and calls only
    // setrlimit, which is async-signal-safe; it touches no parent state.
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: PDF_TEXT_MAX_ADDRESS_SPACE,
                rlim_max: PDF_TEXT_MAX_ADDRESS_SPACE,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let run = async {
        let write = async {
            // pdftotext may stop reading early on a broken file; its exit
            // status decides, not the write.
            let _ = stdin.write_all(bytes).await;
            drop(stdin);
        };
        let mut out = Vec::new();
        let mut limited = stdout.take(MAX_PDF_TEXT_BYTES);
        let read = limited.read_to_end(&mut out);
        let ((), read) = tokio::join!(write, read);
        read.ok()?;
        child.wait().await.ok()?.success().then_some(out)
    };
    let out = tokio::time::timeout(PDF_TEXT_TIMEOUT, run).await.ok()??;
    let text = String::from_utf8_lossy(&out).into_owned();
    text.chars()
        .any(|c| ('가'..='힣').contains(&c))
        .then_some(text)
}

/// Whether a page's boxes are tick boxes: two or more sibling options, each
/// box followed by a short option, either on one line ('□ 예 □ 아니오') or
/// on adjacent lines with nothing between them but empty lines or a
/// bracket ('□ 아동복지시설 / ( / □ 가정위탁 / □ 기타'). A plan's '□ 추진
/// 배경' is followed by content (prose, '1.', '가.', '○ …') before its next
/// heading: its boxes are never adjacent, whatever its sub-points look like.
pub fn tick_boxes(text: &str) -> bool {
    let short = |option: &str| (1..=TICK_OPTION_CHARS).contains(&option.trim().chars().count());
    let filler = |line: &str| {
        line.chars().count() <= TICK_FILLER_CHARS
            && line
                .chars()
                .all(|c| c.is_whitespace() || "()[]/·,.:".contains(c))
    };
    let mut previous = false;
    for line in text.lines().map(str::trim) {
        let options: Vec<&str> = line.split('□').skip(1).collect();
        if options.len() >= 2 && options.iter().all(|o| short(o)) {
            return true;
        }
        match line.strip_prefix('□') {
            Some(option) if options.len() == 1 && short(option) => {
                if previous {
                    return true;
                }
                previous = true;
            }
            Some(_) => previous = false,
            None if filler(line) => {}
            None => previous = false,
        }
    }
    false
}

/// Body paragraphs. HWP5: `BodyText/SectionN` (raw deflate when the file
/// header says so), `PARA_TEXT` records (tag 67) with control characters
/// skipped. HWPX: `<hp:t>` runs of each `<hp:p>` in `Contents/sectionN.xml`.
pub fn body_paragraphs(bytes: &[u8], format: &str) -> Vec<String> {
    match format {
        "hwp" => hwp_paragraphs(bytes).unwrap_or_default(),
        "hwpx" => hwpx_paragraphs(bytes).unwrap_or_default(),
        _ => vec![],
    }
}

fn hwp_paragraphs(bytes: &[u8]) -> Option<Vec<String>> {
    let mut file = cfb::CompoundFile::open(Cursor::new(bytes)).ok()?;
    let mut header = Vec::new();
    file.open_stream("/FileHeader")
        .ok()?
        .take(256)
        .read_to_end(&mut header)
        .ok()?;
    let compressed = header
        .get(36..40)
        .map(|f| u32::from_le_bytes([f[0], f[1], f[2], f[3]]) & 1 == 1)?;
    let mut out = vec![];
    let mut budget = MAX_BODY_BYTES;
    for n in 0..MAX_SECTIONS {
        let Ok(stream) = file.open_stream(format!("/BodyText/Section{n}")) else {
            break;
        };
        let mut data = Vec::new();
        if compressed {
            flate2::read::DeflateDecoder::new(stream.take(MAX_BODY_BYTES))
                .take(budget)
                .read_to_end(&mut data)
                .ok()?;
        } else {
            stream.take(budget).read_to_end(&mut data).ok()?;
        }
        budget = budget.saturating_sub(data.len() as u64);
        let mut i = 0;
        while i + 4 <= data.len() && out.len() < MAX_PARAGRAPHS {
            let h = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
            i += 4;
            let (tag, mut size) = (h & 0x3ff, ((h >> 20) & 0xfff) as usize);
            if size == 0xfff {
                let s = data.get(i..i + 4)?;
                size = u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize;
                i += 4;
            }
            let record = data.get(i..i.checked_add(size)?)?;
            i += size;
            if tag == 67 {
                out.push(para_text(record));
            }
        }
        if budget == 0 {
            break;
        }
    }
    Some(out)
}

/// `PARA_TEXT`: UTF-16LE where codes below 32 are controls. 0, 10, 13 and
/// 24–31 take one code unit; the others (inline and extended controls) take
/// eight.
fn para_text(record: &[u8]) -> String {
    let units: Vec<u16> = record
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let mut out = String::new();
    let mut k = 0;
    while k < units.len() {
        let u = units[k];
        if u < 32 {
            if u == 10 || u == 13 {
                out.push('\n');
            }
            k += if matches!(u, 0 | 10 | 13 | 24..=31) {
                1
            } else {
                8
            };
            continue;
        }
        out.extend(char::from_u32(u32::from(u)));
        k += 1;
    }
    out
}

fn hwpx_paragraphs(bytes: &[u8]) -> Option<Vec<String>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    let mut names: Vec<String> = zip
        .file_names()
        .filter_map(Result::ok)
        .filter(|n| n.starts_with("Contents/section") && n.ends_with(".xml"))
        .map(|n| n.into_owned())
        .collect();
    names.sort();
    let mut out = vec![];
    let mut budget = MAX_BODY_BYTES;
    for name in names.into_iter().take(MAX_SECTIONS) {
        let mut xml = String::new();
        zip.by_name(&name)
            .ok()?
            .take(budget)
            .read_to_string(&mut xml)
            .ok()?;
        budget = budget.saturating_sub(xml.len() as u64);
        let mut reader = quick_xml::Reader::from_str(&xml);
        let (mut para, mut in_text) = (String::new(), false);
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Start(e)) if e.local_name().as_ref() == b"t" => {
                    in_text = true
                }
                Ok(quick_xml::events::Event::End(e)) if e.local_name().as_ref() == b"t" => {
                    in_text = false
                }
                Ok(quick_xml::events::Event::End(e)) if e.local_name().as_ref() == b"p" => {
                    out.push(std::mem::take(&mut para));
                }
                Ok(quick_xml::events::Event::Text(t)) if in_text => {
                    let raw = t.decode().unwrap_or_default();
                    para.push_str(&quick_xml::escape::unescape(&raw).unwrap_or(raw.clone()));
                }
                Ok(quick_xml::events::Event::Eof) | Err(_) => break,
                _ => {}
            }
            if out.len() >= MAX_PARAGRAPHS {
                break;
            }
        }
    }
    Some(out)
}

/// A notice packet's own form: a short paragraph headed as a form title
/// that names the programme. A list item ('중소기업확인서 1부'), a step
/// ('신청서 제출') or a bare '신청서' is not one.
pub fn packet_form(
    paragraphs: &[String],
    program: &str,
    vocabulary: &Vocabulary,
) -> Option<String> {
    paragraphs.iter().enumerate().find_map(|(i, p)| {
        // A row of a numbered list of documents ('1 | 청년 … 신청서 | - | 홈페이지 작성')
        // names a form the applicant fills elsewhere; a heading does not follow a bare number.
        if i > 0 && {
            let previous = compact(&paragraphs[i - 1]);
            !previous.is_empty() && previous.chars().all(|c| c.is_ascii_digit())
        } {
            return None;
        }
        let text = compact(p);
        // So is a row that carries its own enumerator ('② 육아휴직 대상자 확인서').
        if text.starts_with(|c: char| ('\u{2460}'..='\u{2473}').contains(&c)) {
            return None;
        }
        let text = MARKER.replace(&text, "");
        let text = TRAILING_BRACKET.replace(&text, "").into_owned();
        let m = FORM_TITLE.find(&text)?;
        let named = text[..m.start()]
            .chars()
            .filter(|c| ('가'..='힣').contains(c))
            .count()
            >= 3;
        (m.end() == text.len()
            && text.chars().count() <= 40
            && named
            && !text.contains(':')
            && !NOT_FORM.is_match(&text)
            && strict_match(&text, program)
            && programme_match(&text, program, vocabulary) != ProgrammeMatch::Reject)
            .then_some(text)
    })
}

/// Where a form prints its round: its opening (the title line, 접수번호) and
/// the applicant's declaration ('위와 같이 … 신청합니다. 2017년 월 일', '위와
/// 같이 「2023년 …」 참가를 신청합니다'). A data field elsewhere on the page
/// ('매출액 2024년') is not the round.
fn form_opening(text: &str) -> String {
    let text = compact(text);
    let mut opening: String = text.chars().take(200).collect();
    if let Some(at) = ["위와같이", "상기와같이"]
        .iter()
        .filter_map(|m| text.rfind(m))
        .max()
    {
        opening.push('|');
        opening.extend(text[at..].chars().take(DECLARATION_CHARS));
    }
    opening
}

/// The decision for one file the probe fetched and verified by magic bytes.
/// `listed` is true when the programme's own official API lists the file;
/// `pdf_text` is [`pdf_first_page_text`] of a PDF; `vocabulary` is built
/// from the current programme titles. A file whose programme match is
/// ambiguous is not published here (`ambiguous_programme`): only the local
/// model may move it (`form_judge`).
pub fn decide(
    bytes: &[u8],
    format: &str,
    pdf_text: Option<&str>,
    programme: &Programme,
    listed: bool,
    this_year: i32,
    vocabulary: &Vocabulary,
) -> Decision {
    let program = programme.title;
    let preview = preview_text(bytes, format);
    let excerpt: String = preview
        .as_deref()
        .or(pdf_text)
        .unwrap_or("")
        .chars()
        .take(EXCERPT_CHARS)
        .collect();
    let (title, tier, opening) = match format {
        "hwp" | "hwpx" => {
            let Some(text) = preview else {
                return refuse("no_preview", "");
            };
            match opening_title(&text) {
                Some(title) => {
                    // A blank form shows empty cells; a filled document (last year's
                    // 사업계획서 of an event) does not.
                    if !compact(&text)
                        .chars()
                        .take(600)
                        .collect::<String>()
                        .contains("<>")
                    {
                        return refuse("no_blank_cell", title);
                    }
                    (title, "form", form_opening(&text))
                }
                None => {
                    let opened = MARKER
                        .replace(&compact(&text), "")
                        .chars()
                        .take(40)
                        .collect::<String>();
                    if !opened.contains("공고") {
                        return refuse("not_a_form", opened);
                    }
                    // The year is read on the notice's own opening too: a 2024 notice
                    // carries the 2024 round's form, whatever the form's title says.
                    match packet_form(&body_paragraphs(bytes, format), program, vocabulary) {
                        Some(title) => (title.clone(), "packet", format!("{opened}{title}")),
                        None => return refuse("notice_without_its_form", opened),
                    }
                }
            }
        }
        // The text, never the file name: a name alone was right on one of two
        // eye-checked PDFs (an agency plan named 사업계획서 slipped through).
        "pdf" => {
            let Some(text) = pdf_text else {
                return refuse("pdf_no_text", "");
            };
            match opening_title(text) {
                // A plan or report can open with 계획서 too; an applicant's form
                // also carries a signature line, an applicant block or a box.
                Some(title)
                    if !APPLICANT_MARKS.iter().any(|m| compact(text).contains(m))
                        && !tick_boxes(text) =>
                {
                    return refuse("no_applicant_marks", title)
                }
                Some(title) => (title, "form", form_opening(text)),
                None => {
                    return refuse(
                        "not_a_form",
                        MARKER
                            .replace(&compact(text), "")
                            .chars()
                            .take(40)
                            .collect::<String>(),
                    )
                }
            }
        }
        _ => return refuse("unsupported_format", ""),
    };
    let birth_year = |m: &regex::Match| opening[m.end()..].starts_with('생');
    let years = if tier == "form" {
        &*FORM_YEAR
    } else {
        &*EARLIER_YEAR
    };
    if years
        .captures_iter(&opening)
        .filter_map(|c| c.get(1).zip(c.get(0)))
        .any(|(y, whole)| {
            !birth_year(&whole) && y.as_str().parse::<i32>().is_ok_and(|y| y < this_year)
        })
    {
        return refuse("earlier_year", title);
    }
    if !listed {
        match programme_verdict(&title, &excerpt, programme, vocabulary) {
            Verdict::OtherPlace => return refuse("other_place", title),
            Verdict::Words(ProgrammeMatch::Accept) => {}
            Verdict::Words(ProgrammeMatch::Reject) => return refuse("other_programme", title),
            Verdict::Words(ProgrammeMatch::Ambiguous) => {
                return Decision {
                    publish: false,
                    reason: "ambiguous_programme",
                    title,
                    excerpt,
                    model: None,
                }
            }
        }
    }
    Decision {
        publish: true,
        reason: tier,
        title,
        excerpt,
        model: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `decide` for a programme known only by its title, with an empty
    /// catalogue (no qualifiers, no places).
    fn decide_for(
        bytes: &[u8],
        format: &str,
        pdf_text: Option<&str>,
        title: &str,
        listed: bool,
        this_year: i32,
    ) -> Decision {
        let programme = Programme {
            title,
            provider: "",
            regions: &[],
            districts: &[],
        };
        decide(
            bytes,
            format,
            pdf_text,
            &programme,
            listed,
            this_year,
            &Vocabulary::default(),
        )
    }

    #[test]
    fn a_form_title_must_open_the_text() {
        assert_eq!(
            opening_title("[별지 제1호서식]<><보훈명예수당 지급신청서><처리기간>").as_deref(),
            Some("보훈명예수당지급신청서")
        );
        assert_eq!(
            opening_title("<서식 1>\n2026년 ( )월, 청년 취업수당 지원 신청서").as_deref(),
            Some("2026년()월,청년취업수당지원신청서")
        );
        assert_eq!(
            opening_title("장례용품 지원 신청서 <신청자>"),
            Some("장례용품지원신청서".into())
        );
        // Notices, ordinances, manuals and plans do not open with a form title.
        for text in [
            "원주시 공고 제2026-147호 청년 우울증 치료비 지원 대상 모집 공고 ... 신청서 1부",
            "구미시 청년 기본 조례 (제정) 2018.10.19 조례 제1327호",
            // edbe (10-31): a manual whose opening mentions a 확인서 further on.
            "<ㅣ><><><임산부 교통비 지원사업 업무 매뉴얼(1차 수정_2)><><><2023>2026.7.<><【영유아정책과】><순서><Ⅰ. 임산부 교통비 지원 확인서 발급>",
            // A title must end its line or cell: '확인서 발급' is a sentence, not a form's title.
            "<임산부 교통비 지원 확인서 발급 절차>",
            "2026년도 전문기술인재 장학사업 시행계획(안)",
        ] {
            assert_eq!(opening_title(text), None, "{text}");
        }
    }

    #[test]
    fn the_programme_must_share_more_than_a_place_name() {
        assert!(strict_match("장례용품지원신청서", "저소득 장례용품 지급"));
        assert!(strict_match(
            "AI면접및역량검사체험지원신청서",
            "AI 면접 및 역량 검사 체험 지원"
        ));
        // 10-31 dry run: a two-syllable place or fragment is not the programme.
        assert!(!strict_match(
            "2026년울산창업U-시리즈지원신청서",
            "울산 청년 웰스테이 지원"
        ));
        assert!(!strict_match(
            "「드림For청년통장」근로시간확인서",
            "i + 맺어드림(나만의 결혼식)"
        ));
    }

    #[test]
    fn a_notice_packet_counts_only_through_a_form_heading_that_names_the_programme() {
        let paras = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let program = "청년쉐어하우스 조성 및 운영";
        let v = Vocabulary::default();
        assert_eq!(
            packet_form(
                &paras(&[
                    "① 건강보험자격확인서 1부",
                    "신청서 제출",
                    "중소기업확인서",
                    "청년 쉐어하우스 입주 신청서"
                ]),
                program,
                &v
            )
            .as_deref(),
            Some("청년쉐어하우스입주신청서")
        );
        assert_eq!(
            packet_form(
                &paras(&["메일 제목 및 파일명: 청년 쉐어하우스 입주 신청서"]),
                program,
                &v
            ),
            None
        );
        assert_eq!(
            packet_form(
                &paras(&["나. 신청서 1부", "개인정보 제공 동의서"]),
                program,
                &v
            ),
            None
        );
    }

    #[test]
    fn decisions_on_files_without_a_preview_or_with_an_old_year() {
        assert_eq!(
            decide_for(
                b"%PDF-1.7",
                "pdf",
                None,
                "청년 취업수당 지원사업",
                true,
                2026
            )
            .reason,
            "pdf_no_text"
        );
        assert_eq!(
            decide_for(b"not an ole file", "hwp", None, "p", true, 2026).reason,
            "no_preview"
        );
        // A form whose opening prints an earlier year is last round's form; a birth year is not.
        assert!(EARLIER_YEAR.is_match("2022년"));
        for printed in [
            "신청합니다.2020...신청인:(서명)",
            "지원신청서(2024-)",
            "2024년",
        ] {
            assert!(FORM_YEAR.is_match(printed), "{printed}");
        }
    }

    /// Real files from the 10-31 samples (blank official forms, an ordinance and a
    /// notice that carries its form), decided as the eye check did.
    #[test]
    fn real_files_are_decided_as_the_eye_check_did() {
        let f = |name: &str| {
            std::fs::read(format!(
                "{}/tests/fixtures/download_forms/{name}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap()
        };
        let d = decide_for(
            &f("funeral_supplies_application.hwp"),
            "hwp",
            None,
            "저소득 장례용품 지급",
            true,
            2026,
        );
        assert_eq!(
            (d.publish, d.reason, d.title.as_str()),
            (true, "form", "장례용품지원신청서")
        );
        let d = decide_for(
            &f("youth_job_allowance_application.hwpx"),
            "hwpx",
            None,
            "고성 평생살기, 청년 취업수당 지원사업",
            true,
            2026,
        );
        assert_eq!((d.publish, d.reason), (true, "form"));
        // The round is in the declaration ('신청합니다. 2017년 월 일'), past the opening.
        let d = decide_for(
            &f("pregnancy_screening_2017_application.hwp"),
            "hwp",
            None,
            "임산부 태아기형아 검사 쿠폰 지원",
            true,
            2026,
        );
        assert_eq!((d.publish, d.reason), (false, "earlier_year"));
        // A data field ('매출액 2024년') is not the round: the 2026 form publishes.
        let d = decide_for(
            &f("jeju_content_jobs_2026_application.hwp"),
            "hwp",
            None,
            "제주 콘텐츠 청년 일자리 빌드업 지원사업",
            true,
            2026,
        );
        assert_eq!((d.publish, d.reason), (true, "form"));
        // Its signature line prints 2020. . .: the 2020 round's form, still listed.
        let d = decide_for(
            &f("marriage_migrant_2020_application.hwp"),
            "hwp",
            None,
            "여성결혼이민자 운전면허 및 자격취득비 지원",
            true,
            2026,
        );
        assert_eq!((d.publish, d.reason), (false, "earlier_year"));
        let d = decide_for(
            &f("youth_ordinance.hwp"),
            "hwp",
            None,
            "구미시 청년 정책",
            true,
            2026,
        );
        assert_eq!((d.publish, d.reason), (false, "not_a_form"));
        let d = decide_for(
            &f("share_house_notice_with_form.hwp"),
            "hwp",
            None,
            "청년쉐어하우스 조성 및 운영",
            false,
            2026,
        );
        assert_eq!(
            (d.publish, d.reason, d.title.as_str()),
            (true, "packet", "청년쉐어하우스입주신청서")
        );
        // A year later the same packet is last round's: the notice's opening says
        // 2026년 although its form's heading carries no year.
        assert_eq!(
            decide_for(
                &f("share_house_notice_with_form.hwp"),
                "hwp",
                None,
                "청년쉐어하우스 조성 및 운영",
                false,
                2027,
            )
            .reason,
            "earlier_year"
        );
        // The same notice on another programme's page is not that programme's form.
        assert!(
            !decide_for(
                &f("share_house_notice_with_form.hwp"),
                "hwp",
                None,
                "청년 면접 정장 지원",
                false,
                2026
            )
            .publish
        );
    }

    /// A PDF's first page through `pdftotext`, as the worker reads it: the
    /// 연제구 blank application form opens with its title; the 기장군 youth
    /// ordinance (the only file 복지로 lists for 기장 청년 면접수당) does not;
    /// bytes that are not a readable PDF have no text.
    #[tokio::test]
    async fn pdfs_are_decided_by_their_first_page_text() {
        let f = |name: &str| {
            std::fs::read(format!(
                "{}/tests/fixtures/download_forms/{name}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap()
        };
        let form = f("yeonje_youth_allowance_application.pdf");
        let text = pdf_first_page_text(&form)
            .await
            .expect("pdftotext reads the form's text layer");
        let d = decide_for(
            &form,
            "pdf",
            Some(&text),
            "자립준비 청년수당 지원",
            true,
            2026,
        );
        assert_eq!(
            (d.publish, d.reason, d.title.as_str()),
            (
                true,
                "form",
                "연제구자립준비[]청년수당·[]청년자금지원신청서"
            )
        );
        // Not listed by the programme's API, it must still name the programme.
        assert!(
            !decide_for(
                &form,
                "pdf",
                Some(&text),
                "청년 면접 정장 지원",
                false,
                2026
            )
            .publish
        );
        let ordinance = f("gijang_youth_ordinance.pdf");
        let text = pdf_first_page_text(&ordinance).await.unwrap();
        let d = decide_for(
            &ordinance,
            "pdf",
            Some(&text),
            "기장군 청년 면접수당 지원사업",
            true,
            2026,
        );
        assert_eq!((d.publish, d.reason), (false, "not_a_form"));
        assert_eq!(pdf_first_page_text(b"%PDF-1.7 truncated").await, None);
        assert_eq!(pdf_first_page_text(b"").await, None);
    }

    /// Eye-check harness, not a regression check: decides every file of a
    /// manifest (`FORM_CHECK_MANIFEST`: JSON rows with `slug`, `path`,
    /// `format`, `program`, `listed`) with the worker's own code and writes
    /// the decisions to `FORM_CHECK_OUT`. Run it with `--ignored`.
    #[tokio::test]
    #[ignore]
    async fn decide_a_manifest() {
        let rows: Vec<serde_json::Value> = serde_json::from_slice(
            &std::fs::read(std::env::var("FORM_CHECK_MANIFEST").unwrap()).unwrap(),
        )
        .unwrap();
        // FORM_CHECK_CATALOGUE: JSON lines {title, provider, regions} of the
        // current programmes (the worker reads the same from current_programs).
        fn strings(v: &serde_json::Value, key: &str) -> Vec<String> {
            v[key]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|r| r.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        }
        let catalogue: Vec<(String, String, Vec<String>, Vec<String>)> =
            std::fs::read_to_string(std::env::var("FORM_CHECK_CATALOGUE").unwrap())
                .unwrap()
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .map(|v| {
                    (
                        v["title"].as_str().unwrap_or_default().to_string(),
                        v["provider"].as_str().unwrap_or_default().to_string(),
                        strings(&v, "regions"),
                        strings(&v, "districts"),
                    )
                })
                .collect();
        let vocabulary =
            Vocabulary::from_catalogue(catalogue.iter().map(|(t, p, r, d)| Programme {
                title: t,
                provider: p,
                regions: r,
                districts: d,
            }));
        let mut out = serde_json::Map::new();
        for row in rows {
            let field = |k: &str| row[k].as_str().unwrap_or_default().to_string();
            let bytes = std::fs::read(field("path")).unwrap();
            let format = field("format");
            let text = if format == "pdf" {
                pdf_first_page_text(&bytes).await
            } else {
                None
            };
            let (regions, districts) = (strings(&row, "regions"), strings(&row, "districts"));
            let (title, provider) = (field("program"), field("provider"));
            let programme = Programme {
                title: &title,
                provider: &provider,
                regions: &regions,
                districts: &districts,
            };
            let d = decide(
                &bytes,
                &format,
                text.as_deref(),
                &programme,
                row["listed"].as_bool().unwrap_or(false),
                2026,
                &vocabulary,
            );
            out.insert(
                field("slug"),
                serde_json::json!({"publish": d.publish, "reason": d.reason, "title": d.title}),
            );
        }
        std::fs::write(
            std::env::var("FORM_CHECK_OUT").unwrap(),
            serde_json::to_vec_pretty(&out).unwrap(),
        )
        .unwrap();
    }

    /// Held-out harness, not a regression check: the rules' verdict for each
    /// programme/file pair of `FORM_MATCH_PAIRS` (JSON lines with `id`,
    /// `form_title`, `excerpt`, `program`, `provider`, `regions`,
    /// `districts`), over the catalogue `FORM_CHECK_CATALOGUE`; written to
    /// `FORM_MATCH_OUT`. Run it with `--ignored`.
    #[test]
    #[ignore]
    fn match_pairs() {
        fn strings(v: &serde_json::Value, key: &str) -> Vec<String> {
            v[key]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|r| r.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        }
        let read = |var: &str| -> Vec<serde_json::Value> {
            std::fs::read_to_string(std::env::var(var).unwrap())
                .unwrap()
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        };
        let catalogue: Vec<(String, String, Vec<String>, Vec<String>)> =
            read("FORM_CHECK_CATALOGUE")
                .iter()
                .map(|v| {
                    (
                        v["title"].as_str().unwrap_or_default().to_string(),
                        v["provider"].as_str().unwrap_or_default().to_string(),
                        strings(v, "regions"),
                        strings(v, "districts"),
                    )
                })
                .collect();
        let vocabulary =
            Vocabulary::from_catalogue(catalogue.iter().map(|(t, p, r, d)| Programme {
                title: t,
                provider: p,
                regions: r,
                districts: d,
            }));
        let mut out = serde_json::Map::new();
        for pair in read("FORM_MATCH_PAIRS") {
            let field = |k: &str| pair[k].as_str().unwrap_or_default().to_string();
            let (regions, districts) = (strings(&pair, "regions"), strings(&pair, "districts"));
            let (program, provider) = (field("program"), field("provider"));
            let programme = Programme {
                title: &program,
                provider: &provider,
                regions: &regions,
                districts: &districts,
            };
            let verdict = match programme_verdict(
                &compact(&field("form_title")),
                &field("excerpt"),
                &programme,
                &vocabulary,
            ) {
                Verdict::OtherPlace => "reject",
                Verdict::Words(ProgrammeMatch::Accept) => "accept",
                Verdict::Words(ProgrammeMatch::Reject) => "reject",
                Verdict::Words(ProgrammeMatch::Ambiguous) => "ambiguous",
            };
            out.insert(field("id"), serde_json::json!(verdict));
        }
        std::fs::write(
            std::env::var("FORM_MATCH_OUT").unwrap(),
            serde_json::to_vec_pretty(&out).unwrap(),
        )
        .unwrap();
    }

    /// A small catalogue where 저소득, 장애인, 한부모(가족) occur in many
    /// titles and never end one, while the benefits do.
    fn catalogue() -> Vocabulary {
        let heads = [
            "주거", "양육", "교육", "의료", "급식", "교통", "문화", "돌봄", "상담", "통신",
        ];
        let mut titles = vec![];
        for group in ["저소득", "장애인", "한부모", "한부모가족", "신혼부부"] {
            for head in heads {
                titles.push((format!("{group} {head} 지원"), String::new(), vec![]));
            }
        }
        titles.push((
            "서울특별시 청년 월세 지원".into(),
            "서울특별시 주택정책과".into(),
            vec!["seoul".to_string()],
        ));
        titles.push((
            "서울특별시 청년 수당".into(),
            "서울특별시 청년정책과".into(),
            vec!["seoul".to_string()],
        ));
        titles.push((
            "전라남도 청년 통장".into(),
            "전라남도 인구청년과".into(),
            vec!["jeonnam".to_string()],
        ));
        titles.push((
            "전남 청년 수당".into(),
            "전라남도 청년과".into(),
            vec!["jeonnam".to_string()],
        ));
        titles.push((
            "통합 청년 통장".into(),
            "전남광주통합특별시 인구청년과".into(),
            vec!["jeonnam".to_string()],
        ));
        titles.push((
            "통합 청년 수당".into(),
            "전남광주통합특별시 청년과".into(),
            vec!["jeonnam".to_string()],
        ));
        for city in ["보령시", "광양시"] {
            for n in 0..2 {
                titles.push((
                    format!("{city} 출산 지원 {n}"),
                    format!("충청남도 {city} 복지과"),
                    vec!["chungnam".to_string()],
                ));
            }
        }
        titles.push((
            "경기도 학자금 대출이자".into(),
            "경기도 청년정책과".into(),
            vec!["gyeonggi".to_string()],
        ));
        titles.push((
            "경기도 청년 면접".into(),
            "경기도 일자리과".into(),
            vec!["gyeonggi".to_string()],
        ));
        Vocabulary::from_catalogue(titles.iter().map(|(t, p, r)| Programme {
            title: t,
            provider: p,
            regions: r,
            districts: &[],
        }))
    }

    /// The review's three pairs: only a target group is shared, so the
    /// title names none of the programme's own part and is refused; a title
    /// that names the benefit is accepted; half named is ambiguous.
    #[test]
    fn a_shared_target_group_alone_is_not_the_programme() {
        let v = catalogue();
        assert!(v.qualifier("저소득") && v.qualifier("장애인") && v.qualifier("한부모"));
        assert!(!v.qualifier("주거"), "a benefit ends its titles");
        for (title, programme) in [
            ("저소득층자녀학원비지원신청서", "저소득 장례용품 지급"),
            ("장애인보장구수리비지원신청서", "장애인 연금"),
            ("한부모가족자녀양육비지원신청서", "한부모 가족 주거 지원"),
        ] {
            assert_eq!(
                programme_match(title, programme, &v),
                ProgrammeMatch::Reject,
                "{title} / {programme}"
            );
        }
        assert_eq!(
            programme_match("장례용품지원신청서", "저소득 장례용품 지급", &v),
            ProgrammeMatch::Accept
        );
        assert_eq!(
            programme_match("한부모가족주거지원신청서", "한부모 가족 주거 지원", &v),
            ProgrammeMatch::Accept
        );
        assert_eq!(
            programme_match("면접정장대여신청서", "청년 면접 교통비 지원", &v),
            ProgrammeMatch::Ambiguous
        );
        // The old rule accepted all three (a shared word of 3+ syllables).
        assert!(strict_match(
            "저소득층자녀학원비지원신청서",
            "저소득 장례용품 지급"
        ));
    }

    #[test]
    fn programme_words_keep_benefits_and_drop_places() {
        assert_eq!(programme_words("장애인 연금"), vec!["장애인", "연금"]);
        assert_eq!(
            programme_words("취업 대비 자격증 등 시험응시료 지원"),
            vec!["취업", "대비", "자격증", "시험응시"]
        );
        assert_eq!(
            programme_words("원주시 청년 우울증 치료비"),
            vec!["우울증", "치료"]
        );
    }

    /// Places: another province (by region code, so a merged province's new
    /// name still matches its old one) or another 시/군/구 is certain; a
    /// municipal form for a province-wide programme is not.
    #[test]
    fn a_form_that_names_another_place_is_not_this_programmes() {
        let v = catalogue();
        let seoul = ["seoul".to_string()];
        let jeonnam = ["jeonnam".to_string()];
        let gyeonggi = ["gyeonggi".to_string()];
        let chungnam = ["chungnam".to_string()];
        let p = |title, provider, regions| Programme {
            title,
            provider,
            regions,
            districts: &[],
        };
        assert_eq!(
            v.other_place(
                "청년월세신청서",
                "서울특별시 청년 월세 신청서",
                &p("청년 월세 지원", "전라남도 청년과", &jeonnam)
            ),
            Some(true)
        );
        assert_eq!(
            v.other_place(
                "청년통장신청서",
                "전라남도 청년 통장 참여 신청서",
                &p("청년 통장", "전남광주통합특별시 인구청년과", &jeonnam)
            ),
            None,
            "the old province name, the same region code"
        );
        assert_eq!(
            v.other_place(
                "보령시출산지원신청서",
                "",
                &p("출산 지원", "충청남도 광양시 복지과", &chungnam)
            ),
            Some(true)
        );
        assert_eq!(
            v.other_place(
                "보령시출산지원신청서",
                "",
                &p("출산 지원", "충청남도 보령시 복지과", &chungnam)
            ),
            None
        );
        assert_eq!(
            v.other_place(
                "보령시학자금대출이자지원신청서",
                "",
                &p("학자금 대출이자", "경기도 청년정책과", &gyeonggi)
            ),
            Some(false)
        );
        assert_eq!(
            v.other_place(
                "청년월세신청서",
                "신청인 성명",
                &p("청년 월세", "서울특별시", &seoul)
            ),
            None
        );
    }

    /// A renamed district keeps its code: a form that names 제물포구 is the
    /// programme of 인천 동구 (both 28125); 영종구 (28155) is another district.
    #[test]
    fn a_renamed_district_is_compared_by_its_code() {
        let incheon = ["incheon".to_string()];
        let (east, yeongjong) = (["28125".to_string()], ["28155".to_string()]);
        let rows = [
            ("동구 청년 수당", "인천광역시 동구 일자리과", &east),
            ("동구 청년 월세", "인천광역시 동구 주택과", &east),
            ("제물포구 청년 수당", "인천광역시 제물포구", &east),
            ("제물포구 청년 통장", "인천광역시 제물포구 복지과", &east),
            ("영종구 청년 수당", "인천광역시 영종구", &yeongjong),
            ("영종구 청년 통장", "인천광역시 영종구 복지과", &yeongjong),
        ];
        let v = Vocabulary::from_catalogue(rows.iter().map(|(t, p, d)| Programme {
            title: t,
            provider: p,
            regions: &incheon,
            districts: d.as_slice(),
        }));
        let programme = Programme {
            title: "(동구) 청년 웰컴페이 지원",
            provider: "인천광역시 동구 경제환경국",
            regions: &incheon,
            districts: &east,
        };
        assert_eq!(
            v.other_place(
                "행정정보공동이용사전동의서",
                "이용기관 인천광역시 제물포구청 제물포구 청년",
                &programme
            ),
            None
        );
        assert_eq!(
            v.other_place("영종구웰컴페이신청서", "", &programme),
            Some(true)
        );
    }

    /// A box counts only as a tick box. The review's plans (□ headings
    /// followed by '○' points, prose, '1.' or '가.') are refused; the real
    /// 연제구 form's boxes (보호구분: □ 아동복지시설 / ( / □ 가정위탁 / □ 기타)
    /// and a row of choices on one line count.
    #[tokio::test]
    async fn a_box_counts_only_as_a_tick_box() {
        let head = "2026년 부산 청년정책 추진 사업계획서";
        for under in [
            "○ 청년 인구 유출",
            "청년 인구가 감소하고 있음",
            "1. 청년 인구 유출",
            "가. 청년 인구 유출",
        ] {
            let plan = format!("{head}\n□ 추진 배경\n{under}\n□ 추진 방향\n{under}");
            assert!(!tick_boxes(&plan), "{under}");
            let d = decide_for(b"%PDF-1.7", "pdf", Some(&plan), "부산 청년정책", true, 2026);
            assert_eq!(
                (d.publish, d.reason),
                (false, "no_applicant_marks"),
                "{under}"
            );
        }
        let form = std::fs::read(format!(
            "{}/tests/fixtures/download_forms/yeonje_youth_allowance_application.pdf",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let text = pdf_first_page_text(&form).await.unwrap();
        assert!(tick_boxes(&text), "a real form's choice boxes");
        assert!(tick_boxes("미취업 여부 □ 예 □ 아니오"));
        assert!(!tick_boxes(
            "□ 추진 배경 및 필요성에 관한 상세한 설명 □ 추진 방향 및 세부 계획"
        ));
    }

    /// A plan opens with 계획서 too; on a PDF only an applicant's marks make it a form.
    #[test]
    fn a_pdf_plan_without_applicant_marks_is_refused() {
        let plan = "2026년 부산 청년정책 추진 사업계획서\n추진 배경\n1. 목적";
        assert!(opening_title(plan).is_some());
        let d = decide_for(b"%PDF-1.7", "pdf", Some(plan), "부산 청년정책", true, 2026);
        assert_eq!(d.reason, "no_applicant_marks");
        let form = "청년 면접수당 지원 신청서\n신청인 성명\n□ 미취업";
        let d = decide_for(
            b"%PDF-1.7",
            "pdf",
            Some(form),
            "청년 면접수당 지원",
            true,
            2026,
        );
        assert_eq!((d.publish, d.reason), (true, "form"));
    }

    #[test]
    fn para_text_skips_controls() {
        let mut units: Vec<u16> = "신청".encode_utf16().collect();
        units.extend([11u16, 0, 0, 0, 0, 0, 0, 11]); // an extended control: eight units
        units.extend("서".encode_utf16());
        let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(para_text(&bytes), "신청서");
    }
}
