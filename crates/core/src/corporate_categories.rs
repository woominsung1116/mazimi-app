//! Browsing categories of the 기업혜택 tab: the chips that replace the
//! per-company chips.
//!
//! The stored `benefit_category` is too coarse to browse by (`membership`
//! holds coffee stamps, cinema points and convenience-store points alike), so
//! each programme on the tab is placed in exactly one [`CATEGORIES`] entry:
//!
//! 1. the first [`RULES`] entry that matches wins. A rule matches when one of
//!    its `brands` (company names) is in the title, provider name or company
//!    name, or one of its `words` (what the offer is) is in the title;
//! 2. otherwise the stored `benefit_category` decides ([`FALLBACK`]);
//! 3. otherwise `other`.
//!
//! On a campus row (`program_type = campus_benefit`) the provider is a school,
//! so only brands named in the title count and `words` are skipped: a school
//! name (한국항공대, 한국교통대) never decides a category.
//!
//! Matching is on ASCII-lowercased text. A keyword that starts or ends with an
//! ASCII letter or digit must sit on a word boundary there, where `&` counts
//! as part of the word (`canva` does not match `canvas`, `kt` matches neither
//! `ktx` nor `kt&g`); Korean keywords match anywhere (`커피` in `메가MGC커피`).
//!
//! The rules live only here and run only in Rust. Their answer is stored in
//! `programs.browse_category` (migration 20261026000301) by [`refresh`]: the
//! write paths call it for the rows they write and the worker sweeps every
//! five minutes for rows whose inputs or rules changed. The list filter, the
//! chip counts and the admin metric read the column.

/// Chip order and labels. `other` is always last.
pub const CATEGORIES: [(&str, &str); 9] = [
    ("dev", "개발·소프트웨어"),
    ("design", "디자인·창작"),
    ("culture", "문화·콘텐츠"),
    ("food", "카페·외식"),
    ("shopping", "쇼핑·포인트"),
    ("study", "학습·자격증"),
    ("telecom", "통신"),
    ("travel", "교통·여행"),
    ("other", "기타"),
];

/// One category's keywords. Lowercase; see the module docs for where each
/// list is matched.
pub struct Rule {
    pub code: &'static str,
    /// Company and service names.
    pub brands: &'static [&'static str],
    /// What the offer is, matched in the title only.
    pub words: &'static [&'static str],
}

/// Tried in order. Order matters where names overlap: 통신 before 교통
/// (`kt` vs `ktx` is also kept apart by the boundary), 문화 before 쇼핑
/// (`apple music` vs `apple`, `롯데시네마 L.POINT`), 학습 before 쇼핑
/// (`삼성 드림클래스 장학금` by 삼성전자), 문화 before 학습 (`교보문고 … 교재`).
pub const RULES: &[Rule] = &[
    Rule {
        code: "telecom",
        brands: &["sk텔레콤", "skt", "lg유플러스", "lg u+", "유플러스", "kt"],
        words: &["알뜰폰", "y덤", "데이터 쿠폰"],
    },
    Rule {
        code: "travel",
        brands: &[
            "코레일",
            "한국철도공사",
            "srt",
            "야놀자",
            "놀유니버스",
            "nol",
            "대한항공",
            "아시아나",
            "제주항공",
            "진에어",
            "티웨이",
            "티머니",
        ],
        words: &[
            "ktx",
            "철도",
            "기차",
            "항공",
            "고속버스",
            "시외버스",
            "시내버스",
            "여행",
            "숙소",
            "렌터카",
        ],
    },
    Rule {
        code: "culture",
        brands: &[
            "cgv",
            "롯데시네마",
            "메가박스",
            "알라딘",
            "예스24",
            "yes24",
            "교보문고",
            "포토이즘",
            "아쿠아플라넷",
            "티빙",
            "spotify",
            "apple music",
            "youtube",
            "유튜브",
            "netflix",
            "넷플릭스",
            "멜론",
            "왓챠",
            "웨이브",
            "지니뮤직",
        ],
        words: &["영화", "공연", "전시", "도서", "음악"],
    },
    Rule {
        code: "food",
        brands: &[
            "스타벅스",
            "투썸",
            "이디야",
            "할리스",
            "폴 바셋",
            "파스쿠찌",
            "엔제리너스",
            "커피빈",
            "공차",
            "던킨",
            "배스킨라빈스",
            "쉐이크쉑",
            "크리스피크림",
            "파리바게뜨",
            "뚜레쥬르",
            "롯데리아",
            "lotte eatz",
            "해피포인트",
        ],
        words: &["커피", "카페", "치킨", "버거", "피자", "베이커리"],
    },
    Rule {
        code: "study",
        brands: &[],
        words: &[
            "토익",
            "토플",
            "오픽",
            "자격증",
            "어학",
            "인강",
            "강의",
            "교재",
            "수강",
            "응시",
            "장학",
        ],
    },
    Rule {
        code: "design",
        brands: &[
            "adobe", "autodesk", "axure", "canva", "figma", "framer", "sketch", "miro",
        ],
        words: &["디자인", "드로잉", "일러스트"],
    },
    Rule {
        code: "shopping",
        brands: &[
            "무신사",
            "올리브영",
            "다이소",
            "ikea",
            "이케아",
            "신세계",
            "이마트",
            "gs25",
            "cu",
            "h.point",
            "l.point",
            "ok캐쉬백",
            "나이키",
            "쿠팡",
            "삼성전자",
            "lg전자",
            "apple",
        ],
        words: &["갤럭시", "쇼핑", "편의점"],
    },
    Rule {
        code: "dev",
        brands: &["github", "jetbrains", "microsoft", "aws", "azure"],
        words: &["코딩"],
    },
];

/// `benefit_category` → chip, when no rule matched.
pub const FALLBACK: &[(&str, &str)] = &[
    ("software", "dev"),
    ("subscription", "culture"),
    ("culture", "culture"),
    ("education", "study"),
    ("scholarship", "study"),
    ("telecom", "telecom"),
    ("transport", "travel"),
    ("membership", "shopping"),
    ("discount", "shopping"),
    ("student_discount", "shopping"),
];

/// What placed a programme in its chip, for the self-metric of how many rows
/// no keyword recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecidedBy {
    Keyword,
    Fallback,
    Nothing,
}

impl DecidedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            DecidedBy::Keyword => "keyword",
            DecidedBy::Fallback => "fallback",
            DecidedBy::Nothing => "none",
        }
    }
}

/// The columns the rules read.
#[derive(Debug, Clone, Copy, Default)]
pub struct Row<'a> {
    pub program_type: &'a str,
    pub title: &'a str,
    pub provider_name: Option<&'a str>,
    pub company_name: Option<&'a str>,
    pub benefit_category: Option<&'a str>,
}

/// A filter value: a code or its Korean label, trimmed. `None` = unknown.
pub fn parse(value: &str) -> Option<&'static str> {
    let value = value.trim();
    CATEGORIES
        .iter()
        .find(|(code, label)| code.eq_ignore_ascii_case(value) || *label == value)
        .map(|(code, _)| *code)
}

pub fn label(code: &str) -> Option<&'static str> {
    CATEGORIES.iter().find(|(c, _)| *c == code).map(|(_, l)| *l)
}

/// Position of a code in [`CATEGORIES`] (chip order); unknown codes last.
pub fn order(code: &str) -> usize {
    CATEGORIES
        .iter()
        .position(|(c, _)| *c == code)
        .unwrap_or(CATEGORIES.len())
}

/// Characters that continue a word for the boundary test: ASCII letters and
/// digits, and `&`, which joins brand tokens (`kt&g` is one name, not `kt`
/// followed by a separator).
fn word_char(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '&')
}

/// Whether the keyword needs a boundary before / after it.
fn edges(keyword: &str) -> (bool, bool) {
    (
        word_char(keyword.chars().next()),
        word_char(keyword.chars().next_back()),
    )
}

/// `keyword` in `text`, on a word boundary where the keyword has an ASCII
/// word edge.
fn contains(text: &str, keyword: &str) -> bool {
    let (left, right) = edges(keyword);
    text.match_indices(keyword).any(|(at, _)| {
        (!left || !word_char(text[..at].chars().next_back()))
            && (!right || !word_char(text[at + keyword.len()..].chars().next()))
    })
}

/// The chip of one programme and what decided it.
pub fn decide(row: Row<'_>) -> (&'static str, DecidedBy) {
    let campus = row.program_type == "campus_benefit";
    let title = row.title.to_ascii_lowercase();
    let named = if campus {
        title.clone()
    } else {
        [Some(row.title), row.provider_name, row.company_name]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
    };
    let hit = RULES.iter().find(|rule| {
        rule.brands.iter().any(|k| contains(&named, k))
            || (!campus && rule.words.iter().any(|k| contains(&title, k)))
    });
    if let Some(rule) = hit {
        return (rule.code, DecidedBy::Keyword);
    }
    FALLBACK
        .iter()
        .find(|(stored, _)| Some(*stored) == row.benefit_category)
        .map_or(("other", DecidedBy::Nothing), |(_, code)| {
            (*code, DecidedBy::Fallback)
        })
}

pub fn classify(row: Row<'_>) -> &'static str {
    decide(row).0
}

// ── Stored column ───────────────────────────────────────────────────────────

/// A fingerprint of [`CATEGORIES`], [`RULES`] and [`FALLBACK`] (FNV-1a, 64
/// bit, hex). It is part of every stored key, so editing a rule makes every
/// row pending and [`refresh`] recomputes them; nobody bumps a version.
pub fn rules_fingerprint() -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |s: &str| {
        for b in s.bytes().chain([0xff]) {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for (code, label) in CATEGORIES {
        feed(code);
        feed(label);
    }
    for rule in RULES {
        feed(rule.code);
        rule.brands.iter().for_each(|k| feed(k));
        feed("|");
        rule.words.iter().for_each(|k| feed(k));
    }
    for (stored, code) in FALLBACK {
        feed(stored);
        feed(code);
    }
    format!("{hash:016x}")
}

/// The key of the inputs of `programs` row `alias` (empty = unqualified) as
/// they are now: the rule fingerprint and an md5 of the columns
/// [`decide`] reads. NULL and '' differ in a row's text form.
fn key_sql(alias: &str) -> String {
    assert!(
        alias
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "alias must be an identifier"
    );
    let a = if alias.is_empty() {
        String::new()
    } else {
        format!("{alias}.")
    };
    format!(
        "('{}:' || md5(ROW({a}program_type, {a}title, {a}provider_name, {a}company_name, {a}benefit_category)::text))",
        rules_fingerprint()
    )
}

/// Rows per statement of [`refresh`].
pub const BATCH: i64 = 500;

/// What one [`refresh`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refresh {
    pub updated: u64,
    /// Rows that left the 기업혜택 tab and lost their chip.
    pub cleared: u64,
    /// Rows still pending after the run.
    pub remaining: i64,
}

#[derive(sqlx::FromRow)]
struct Pending {
    id: uuid::Uuid,
    program_type: String,
    title: String,
    provider_name: Option<String>,
    company_name: Option<String>,
    benefit_category: Option<String>,
    key: String,
}

/// Store the chip of every 기업혜택 row whose inputs or rules changed since
/// it was computed (`browse_category_key` no longer matches), and clear it
/// on rows that left the tab. `ids` limits the run to those programmes (a
/// write path calls it for the rows it just wrote, inside its own
/// transaction); `None` is the worker's sweep. At most `max_batches` batches
/// of [`BATCH`] rows.
///
/// Every batch locks its rows in id order with `FOR UPDATE SKIP LOCKED`, in
/// its own transaction (a savepoint inside the caller's), then writes them.
/// A row another transaction holds (feed ingestion, a write path) is skipped,
/// never waited on, and stays pending for the next run: the sweep cannot
/// block ingestion or deadlock with it. The write path's own rows are its
/// own locks, so they are never skipped.
pub async fn refresh(
    conn: &mut sqlx::PgConnection,
    ids: Option<&[uuid::Uuid]>,
    max_batches: usize,
) -> sqlx::Result<Refresh> {
    use sqlx::Connection;
    let tab = crate::categories::corporate_tab_sql("programs");
    let key = key_sql("programs");
    let only = "AND ($1::uuid[] IS NULL OR programs.id = ANY($1))";
    let ids: Option<Vec<uuid::Uuid>> = ids.map(<[_]>::to_vec);
    let cleared = sqlx::query(&format!(
        "UPDATE programs SET browse_category = NULL, browse_category_by = NULL,
                browse_category_key = NULL, browse_category_at = now()
         WHERE id IN (
             SELECT id FROM programs
             WHERE browse_category_key IS NOT NULL AND NOT COALESCE({tab}, false) {only}
             ORDER BY id FOR UPDATE SKIP LOCKED)"
    ))
    .bind(&ids)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    let mut out = Refresh {
        cleared,
        ..Refresh::default()
    };
    let pending =
        format!("COALESCE({tab}, false) AND browse_category_key IS DISTINCT FROM {key} {only}");
    for _ in 0..max_batches {
        let mut tx = conn.begin().await?;
        let rows = sqlx::query_as::<_, Pending>(&format!(
            "SELECT id, program_type, title, provider_name, company_name, benefit_category, {key} AS key
             FROM programs WHERE {pending} ORDER BY id LIMIT $2 FOR UPDATE SKIP LOCKED"
        ))
        .bind(&ids)
        .bind(BATCH)
        .fetch_all(&mut *tx)
        .await?;
        if rows.is_empty() {
            tx.commit().await?;
            break;
        }
        let mut columns: (Vec<uuid::Uuid>, Vec<&str>, Vec<&str>, Vec<&str>) = Default::default();
        for row in &rows {
            let (code, by) = decide(Row {
                program_type: &row.program_type,
                title: &row.title,
                provider_name: row.provider_name.as_deref(),
                company_name: row.company_name.as_deref(),
                benefit_category: row.benefit_category.as_deref(),
            });
            columns.0.push(row.id);
            columns.1.push(code);
            columns.2.push(by.as_str());
            columns.3.push(&row.key);
        }
        // The rows are locked: their inputs, hence their key, cannot change.
        let updated = sqlx::query(
            "UPDATE programs SET browse_category = u.code, browse_category_by = u.by,
                    browse_category_key = u.key, browse_category_at = now()
             FROM unnest($1::uuid[], $2::text[], $3::text[], $4::text[]) AS u(id, code, by, key)
             WHERE programs.id = u.id",
        )
        .bind(&columns.0)
        .bind(&columns.1)
        .bind(&columns.2)
        .bind(&columns.3)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        out.updated += updated;
        if (rows.len() as i64) < BATCH {
            break;
        }
    }
    out.remaining = sqlx::query_scalar(&format!("SELECT count(*) FROM programs WHERE {pending}"))
        .bind(&ids)
        .fetch_one(&mut *conn)
        .await?;
    Ok(out)
}

/// Whether any 기업혜택 row has no stored chip yet (after the migration,
/// or written by a path that does not store it). Cheap: it stops at the
/// first such row.
pub async fn any_unfilled(conn: &mut sqlx::PgConnection) -> sqlx::Result<bool> {
    let tab = crate::categories::corporate_tab_sql("programs");
    sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM programs WHERE COALESCE({tab}, false) AND browse_category_key IS NULL)"
    ))
    .fetch_one(&mut *conn)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (program_type, title, provider_name, company_name, benefit_category,
    /// expected) of every row of a fixture.
    fn rows(tsv: &'static str) -> Vec<[&'static str; 6]> {
        tsv.lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                assert_eq!(f.len(), 6, "{l}");
                [f[0], f[1], f[2], f[3], f[4], f[5]]
            })
            .collect()
    }

    fn production() -> Vec<[&'static str; 6]> {
        rows(include_str!("../tests/fixtures/corporate_categories.tsv"))
    }

    fn row<'a>(r: &'a [&'a str; 6]) -> Row<'a> {
        let opt = |v: &'a str| (!v.is_empty()).then_some(v);
        Row {
            program_type: r[0],
            title: r[1],
            provider_name: opt(r[2]),
            company_name: opt(r[3]),
            benefit_category: opt(r[4]),
        }
    }

    fn t(title: &str) -> Row<'_> {
        Row {
            program_type: "corporate_benefit",
            title,
            ..Row::default()
        }
    }

    #[test]
    fn every_production_title_gets_its_reviewed_category() {
        let rows = production();
        assert_eq!(rows.len(), 88, "the whole 기업혜택 tab of 2026-10-26");
        let wrong: Vec<String> = rows
            .iter()
            .filter_map(|r| {
                let got = classify(row(r));
                (got != r[5]).then(|| format!("{}: {got} (want {})", r[1], r[5]))
            })
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
        // The table the report prints: category → titles, and what decided each.
        for (code, label) in CATEGORIES {
            let titles: Vec<String> = rows
                .iter()
                .filter(|r| r[5] == code)
                .map(|r| match decide(row(r)).1 {
                    DecidedBy::Keyword => r[1].to_string(),
                    by => format!("{} [{}]", r[1], by.as_str()),
                })
                .collect();
            println!("{label} ({code}, {}): {}", titles.len(), titles.join(" / "));
        }
        let unmatched = rows
            .iter()
            .filter(|r| decide(row(r)).1 != DecidedBy::Keyword)
            .count();
        println!("decided by no keyword: {unmatched} of {}", rows.len());
    }

    /// Held-out titles labelled before the rules were run on them. Reports
    /// accuracy; asserts only a floor so a regression is caught.
    #[test]
    fn held_out_accuracy() {
        let rows = rows(include_str!(
            "../tests/fixtures/corporate_categories_holdout.tsv"
        ));
        assert!(rows.len() >= 25);
        let misses: Vec<String> = rows
            .iter()
            .filter_map(|r| {
                let (got, by) = decide(row(r));
                (got != r[5]).then(|| format!("{}: {got} [{}] (want {})", r[1], by.as_str(), r[5]))
            })
            .collect();
        println!(
            "held-out: {} of {} correct; misses: {misses:#?}",
            rows.len() - misses.len(),
            rows.len()
        );
        assert!(misses.len() * 5 <= rows.len(), "below 80%: {misses:#?}");
    }

    #[test]
    fn no_single_category_swallows_the_tab() {
        // `membership` held 36 of 77 rows; no chip may hold a third of the tab.
        let rows = production();
        for (code, _) in CATEGORIES {
            let n = rows.iter().filter(|r| r[5] == code).count();
            assert!(n * 3 < rows.len(), "{code} holds {n} of {}", rows.len());
        }
    }

    #[test]
    fn rules_are_well_formed() {
        let codes: Vec<&str> = CATEGORIES.iter().map(|(c, _)| *c).collect();
        assert_eq!(codes.last(), Some(&"other"));
        for rule in RULES {
            assert!(codes.contains(&rule.code), "{}", rule.code);
            for k in rule.brands.iter().chain(rule.words) {
                assert_eq!(*k, k.to_ascii_lowercase(), "keywords are lowercase");
                assert!(!k.trim().is_empty() && k.trim() == *k, "{k:?}");
                assert!(!k.contains('\'') && !k.contains('\\'), "{k}");
            }
        }
        for (_, code) in FALLBACK {
            assert!(codes.contains(code), "{code}");
        }
    }

    #[test]
    fn a_school_name_never_decides_a_category() {
        let campus = |title, provider| Row {
            program_type: "campus_benefit",
            title,
            provider_name: Some(provider),
            benefit_category: Some("software"),
            ..Row::default()
        };
        assert_eq!(
            classify(campus("한국항공대 재학생 Microsoft 365", "한국항공대학교")),
            "dev"
        );
        assert_eq!(
            classify(campus("한국교통대 Office 365", "한국교통대학교")),
            "dev"
        );
        assert_eq!(
            classify(campus("한국교통대 Adobe CC 무료", "한국교통대학교")),
            "design"
        );
        // A corporate provider's name does count.
        assert_eq!(
            classify(Row {
                provider_name: Some("대한항공"),
                ..t("청소년 할인")
            }),
            "travel"
        );
        // A generic word in a provider name does not.
        assert_eq!(
            classify(Row {
                provider_name: Some("여행자보험(주)"),
                benefit_category: Some("discount"),
                ..t("가입 할인")
            }),
            "shopping"
        );
    }

    #[test]
    fn ascii_keywords_sit_on_word_boundaries() {
        assert_eq!(
            classify(Row {
                benefit_category: Some("software"),
                ..t("Canvas LMS 학생 계정")
            }),
            "dev"
        );
        assert_eq!(classify(t("Canva Pro 학생 무료")), "design");
        assert_eq!(classify(t("코레일 KTX 할인")), "travel");
        assert_eq!(classify(t("KT 청년 요금")), "telecom");
        assert_eq!(classify(t("NOL·CJ ONE 적립")), "travel");
        assert_eq!(classify(t("Technology 학생 할인")), "other");
        assert!(contains("x cu·cj", "cu") && !contains("cucumber", "cu"));
        assert!(contains("메가mgc커피", "커피"));
    }

    #[test]
    fn unknown_rows_fall_back_then_land_in_other() {
        assert_eq!(
            decide(Row {
                benefit_category: Some("membership"),
                ..t("새 혜택")
            }),
            ("shopping", DecidedBy::Fallback)
        );
        assert_eq!(
            decide(Row {
                benefit_category: Some("scholarship"),
                ..t("새 혜택")
            }),
            ("study", DecidedBy::Fallback)
        );
        assert_eq!(
            decide(Row {
                benefit_category: Some("finance"),
                ..t("새 혜택")
            }),
            ("other", DecidedBy::Nothing)
        );
        assert_eq!(decide(t("ADOBE CC")), ("design", DecidedBy::Keyword));
        assert_eq!(
            classify(Row {
                provider_name: Some("Spotify"),
                benefit_category: Some("student_discount"),
                ..t("x")
            }),
            "culture"
        );
        assert_eq!(classify(t("롯데시네마 L.POINT 적립")), "culture");
        assert_eq!(classify(t("Apple Music 학생")), "culture");
        assert_eq!(classify(t("Apple 교육 할인")), "shopping");
    }

    #[test]
    fn filters_take_codes_or_labels() {
        assert_eq!(parse("design"), Some("design"));
        assert_eq!(parse(" 카페·외식 "), Some("food"));
        assert_eq!(parse("DEV"), Some("dev"));
        assert_eq!(parse("학습·자격증"), Some("study"));
        assert_eq!(parse("membership"), None);
        assert_eq!(parse(""), None);
        assert_eq!(label("travel"), Some("교통·여행"));
        assert_eq!(order("dev"), 0);
        assert_eq!(order("nope"), CATEGORIES.len());
    }

    #[test]
    fn an_ampersand_joins_a_brand_token() {
        // KT&G is not KT: its offers are placed by what they are.
        let row = |title, category| Row {
            provider_name: Some("KT&G"),
            benefit_category: category,
            ..t(title)
        };
        assert_eq!(
            classify(row("KT&G 상상장학생 모집", Some("scholarship"))),
            "study"
        );
        assert_eq!(
            classify(row("KT&G 상상마당 공연 할인", Some("discount"))),
            "culture"
        );
        assert_eq!(
            decide(row("KT&G 상상유니브 클래스", None)),
            ("other", DecidedBy::Nothing)
        );
        assert!(!contains("kt&g 상상", "kt") && contains("kt y덤", "kt") && contains("(kt)", "kt"));
        assert!(!contains("at&t", "t"));
    }

    #[test]
    fn the_fingerprint_names_every_rule() {
        let fp = rules_fingerprint();
        assert_eq!(fp.len(), 16);
        assert!(fp.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(fp, rules_fingerprint(), "deterministic");
        let key = key_sql("p");
        assert!(key.starts_with(&format!("('{fp}:' || md5(ROW(p.program_type, p.title")));
        assert!(key_sql("")
            .contains("ROW(program_type, title, provider_name, company_name, benefit_category)"));
    }

    #[test]
    #[should_panic(expected = "alias must be an identifier")]
    fn alias_is_an_identifier() {
        key_sql("p; DROP TABLE programs; --");
    }
}
