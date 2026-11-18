//! The brand registry the doors bind names against: must-have brands and
//! issuers (`brand_discovery_targets.aliases`, `brands.holder_*`).
//!
//! Two readings, both from the same normalised names ([`normalize`]):
//! - [`Registry::exact`]: a hub row binds a brand only when the whole
//!   visible text of one element equals one of its names (review H2). A
//!   name inside a longer one ("이마트 에브리데이") never binds the shorter.
//! - [`Registry::issuers_in`]: the issuers a sentence names ("대상: T 멤버십
//!   할인형 고객" names SK텔레콤), for the sponsor of a first-party fact
//!   (review H6). A name binds only where it does not continue a longer
//!   word on its left ("SKT" never yields "KT"), and the longest name wins.

use mazimi_core::holder::Holder;
use sqlx::PgPool;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

/// The form names are compared in: NFKC, lowercase, every whitespace
/// removed ("T 멤버십" and "T멤버십" are one name).
pub fn normalize(name: &str) -> String {
    name.nfkc()
        .flat_map(char::to_lowercase)
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// One registry brand.
#[derive(Debug, Clone, PartialEq)]
pub struct Brand {
    pub brand_id: Uuid,
    pub name: String,
    /// Normalised names: the display name and every alias.
    pub names: Vec<String>,
    pub must_have: bool,
    /// Set for an issuer (a telco, card company or pay app).
    pub holder: Option<Holder>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Registry {
    brands: Vec<Brand>,
}

/// (brand id, name, every name, must-have, holder key) as loaded.
type RegistryRow = (Uuid, String, Vec<String>, bool, Option<String>);

/// Shortest and longest accepted name (normalised characters).
pub const NAME_CHARS: std::ops::RangeInclusive<usize> = 2..=40;

impl Registry {
    pub fn new(brands: Vec<Brand>) -> Self {
        Self { brands }
    }

    pub fn brands(&self) -> &[Brand] {
        &self.brands
    }

    pub fn get(&self, id: Uuid) -> Option<&Brand> {
        self.brands.iter().find(|b| b.brand_id == id)
    }

    /// The brand whose name is exactly `text`; `None` for no brand or for a
    /// name two brands share (the loader test refuses that).
    pub fn exact(&self, text: &str) -> Option<&Brand> {
        let wanted = normalize(text);
        if !NAME_CHARS.contains(&wanted.chars().count()) {
            return None;
        }
        let mut found = self.brands.iter().filter(|b| b.names.contains(&wanted));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// The issuers `sentence` names (code review C-2), each once, in the
    /// order named. A name binds only as a whole token: on its left no
    /// letter of the same script continues it ("SKT" never yields "KT"), on
    /// its right a space, punctuation, another script or a particle ends it
    /// ("KB국민카드로" binds; "토스트", "토스증권", "KT&G" do not). The
    /// longest name of any registry brand covering a span wins, and only an
    /// issuer's binds ("토스뱅크", "카카오 T 멤버십" bind no one).
    pub fn issuers_in(&self, sentence: &str) -> Vec<&Brand> {
        // Normalised characters, each with whether a space preceded it.
        let mut text: Vec<char> = Vec::new();
        let mut spaced: Vec<bool> = Vec::new();
        let mut space = true;
        for c in sentence.nfkc().flat_map(char::to_lowercase) {
            if c.is_whitespace() {
                space = true;
                continue;
            }
            text.push(c);
            spaced.push(space);
            space = false;
        }
        let continues = |a: char, b: char| {
            (a.is_alphanumeric() && b.is_alphanumeric() && is_same_script(a, b))
                || (a == '&' && b.is_alphanumeric())
                || (a.is_alphanumeric() && b == '&')
        };
        let ends_token = |end: usize| {
            if end >= text.len() || spaced[end] || !continues(text[end - 1], text[end]) {
                return true;
            }
            let rest: String = text[end..].iter().take(3).collect();
            PARTICLES.iter().any(|p| {
                rest.starts_with(p)
                    && (end + p.chars().count() >= text.len()
                        || spaced[end + p.chars().count()]
                        || !continues(
                            text[end + p.chars().count() - 1],
                            text[end + p.chars().count()],
                        ))
            })
        };
        // (start, end, brand) for every whole-token name occurrence.
        let mut hits: Vec<(usize, usize, &Brand)> = Vec::new();
        for brand in &self.brands {
            for name in &brand.names {
                let name: Vec<char> = name.chars().collect();
                if name.is_empty() || name.len() > text.len() {
                    continue;
                }
                for start in 0..=text.len() - name.len() {
                    let end = start + name.len();
                    if text[start..end] != name[..] {
                        continue;
                    }
                    let left_open =
                        start == 0 || spaced[start] || !continues(text[start - 1], text[start]);
                    if left_open && ends_token(end) {
                        hits.push((start, end, brand));
                    }
                }
            }
        }
        hits.sort_by_key(|(start, end, _)| (*start, std::cmp::Reverse(*end)));
        let mut out: Vec<&Brand> = Vec::new();
        let mut covered_to = 0;
        for (start, end, brand) in hits {
            if start < covered_to {
                continue;
            }
            covered_to = end;
            if brand.holder.is_some() && !out.iter().any(|b| b.brand_id == brand.brand_id) {
                out.push(brand);
            }
        }
        out
    }

    /// Loads must-have targets and issuers. Names outside [`NAME_CHARS`]
    /// are dropped.
    pub async fn load(pool: &PgPool) -> anyhow::Result<Self> {
        let rows: Vec<RegistryRow> = sqlx::query_as(
            "SELECT b.id, b.name,
                    COALESCE(array_agg(DISTINCT a) FILTER (WHERE a IS NOT NULL), '{}') ||
                      ARRAY[b.name] || COALESCE(array_agg(DISTINCT t.display_name), '{}'),
                    COALESCE(bool_or(t.must_have), false),
                    CASE WHEN b.holder_field IS NOT NULL THEN b.holder_field || ':' || b.holder_value END
             FROM brands b
             LEFT JOIN brand_discovery_targets t ON t.brand_id = b.id AND t.is_active
             LEFT JOIN LATERAL unnest(t.aliases) a ON true
             WHERE b.is_active AND (t.must_have OR b.holder_field IS NOT NULL)
               -- A duplicate row of a registry brand (same name, carrying
               -- the holder key too) is the same company, never a second
               -- registry brand: the target-linked row stands for it.
               AND (t.brand_id IS NOT NULL OR NOT EXISTS (
                   SELECT 1 FROM brand_discovery_targets t2 JOIN brands b2 ON b2.id = t2.brand_id
                   WHERE t2.is_active AND b2.id <> b.id AND b2.normalized_name = b.normalized_name))
             GROUP BY b.id, b.name, b.holder_field, b.holder_value
             ORDER BY b.id",
        )
        .fetch_all(pool)
        .await?;
        Ok(Self::new(
            rows.into_iter()
                .map(|(brand_id, name, names, must_have, holder)| {
                    let mut names: Vec<String> = names
                        .iter()
                        .map(|n| normalize(n))
                        .filter(|n| NAME_CHARS.contains(&n.chars().count()))
                        .collect();
                    names.sort();
                    names.dedup();
                    Brand {
                        brand_id,
                        name,
                        names,
                        must_have,
                        holder: holder.as_deref().and_then(Holder::parse),
                    }
                })
                .collect(),
        ))
    }

    /// Names two brands share (the loader test's check).
    pub fn shared_names(&self) -> Vec<(String, Uuid, Uuid)> {
        let mut out = Vec::new();
        for (i, a) in self.brands.iter().enumerate() {
            for b in &self.brands[i + 1..] {
                for name in a.names.iter().filter(|n| b.names.contains(n)) {
                    out.push((name.clone(), a.brand_id, b.brand_id));
                }
            }
        }
        out
    }
}

/// Particles that may follow a name and still end it ("KB국민카드로 결제").
const PARTICLES: &[&str] = &[
    "으로", "로", "와", "과", "의", "은", "는", "이", "가", "을", "를", "에서", "에", "만", "도",
];

/// Hangul next to Hangul, or Latin/digit next to Latin/digit, continues a
/// word; a Latin name right after Hangul ("의KT") does not.
fn is_same_script(a: char, b: char) -> bool {
    let hangul =
        |c: char| ('\u{AC00}'..='\u{D7A3}').contains(&c) || ('\u{3131}'..='\u{318E}').contains(&c);
    hangul(a) == hangul(b)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn brand(name: &str, aliases: &[&str], holder: Option<&str>) -> Brand {
        let mut names: Vec<String> = std::iter::once(name)
            .chain(aliases.iter().copied())
            .map(normalize)
            .collect();
        names.sort();
        names.dedup();
        Brand {
            brand_id: {
                use sha2::Digest;
                let digest = sha2::Sha256::digest(name.as_bytes());
                Uuid::from_slice(&digest[..16]).expect("16 bytes")
            },
            name: name.to_string(),
            names,
            must_have: true,
            holder: holder.and_then(Holder::parse),
        }
    }

    /// The registry the fixture tests read: the must-have rows of migration
    /// 20261111000103 that the saved hub pages name, plus the issuers.
    pub(crate) fn fixture_registry() -> Registry {
        let rows: &[(&str, &[&str], Option<&str>)] = &[
            (
                "SK텔레콤",
                &["SKT", "T멤버십", "T 멤버십"],
                Some("telecom_carrier:skt"),
            ),
            ("KT", &["KT멤버십"], Some("telecom_carrier:kt")),
            (
                "LG유플러스",
                &["LG U+", "U+멤버십"],
                Some("telecom_carrier:lgu"),
            ),
            ("KB국민카드", &["국민카드"], Some("card_issuers:kb")),
            ("신한카드", &[], Some("card_issuers:shinhan")),
            ("삼성카드", &[], Some("card_issuers:samsung")),
            ("PAYCO", &["페이코"], Some("pay_apps:payco")),
            ("토스", &["토스페이"], Some("pay_apps:tosspay")),
            ("배스킨라빈스", &[], None),
            ("파리바게뜨", &[], None),
            ("CU", &[], None),
            ("아웃백스테이크하우스", &["아웃백"], None),
            ("이마트", &[], None),
            ("뚜레쥬르", &[], None),
            ("공차", &[], None),
            ("도미노피자", &[], None),
            ("롯데리아", &[], None),
            ("세븐일레븐", &[], None),
            ("폴바셋", &[], None),
            ("CGV", &[], None),
            ("던킨", &["던킨도너츠"], None),
            ("메가MGC커피", &["메가커피"], None),
            ("VIPS", &["빕스"], None),
            ("롯데월드", &["롯데월드 어드벤처"], None),
            ("11번가", &[], None),
            ("피자헛", &[], None),
            ("파파존스", &["파파존스피자"], None),
            ("쿠팡", &[], None),
            ("G마켓", &[], None),
            ("롯데ON", &["롯데온"], None),
            ("SSG닷컴", &[], None),
            ("29CM", &[], None),
            ("컬리", &["마켓컬리"], None),
            ("진에어", &[], None),
            ("야놀자", &["NOL"], None),
            ("옥션", &[], None),
            ("CJ온스타일", &[], None),
            ("롯데마트", &[], None),
            ("홈플러스", &[], None),
            ("롯데백화점", &[], None),
            ("AK플라자", &["AK PLAZA"], None),
            ("네이버플러스스토어", &["네이버플러스 스토어"], None),
            ("삼성전자", &["SAMSUNG.COM"], None),
            ("LG전자", &["LGE.COM"], None),
            ("무신사", &[], None),
            ("ABC마트", &[], None),
            ("영풍문고", &[], None),
            ("모던하우스", &[], None),
            ("자주", &["JAJU"], None),
            ("토스뱅크", &[], None),
            ("카카오T", &[], None),
        ];
        Registry::new(
            rows.iter()
                .map(|(name, aliases, holder)| brand(name, aliases, *holder))
                .collect(),
        )
    }

    #[test]
    fn a_name_binds_only_as_a_whole_element() {
        let registry = fixture_registry();
        assert_eq!(registry.exact("이마트").unwrap().name, "이마트");
        assert_eq!(
            registry.exact(" 아웃백 스테이크하우스 ").unwrap().name,
            "아웃백스테이크하우스"
        );
        assert_eq!(registry.exact("ＣＧＶ").unwrap().name, "CGV", "NFKC");
        // Review H2: KB's sub-brand rows never bind the parent brand.
        for sub in [
            "이마트 에브리데이",
            "네이버 예약",
            "홈플러스 익스프레스",
            "롯데백화점 가전",
            "하이마트 온라인몰",
        ] {
            assert!(registry.exact(sub).is_none(), "{sub}");
        }
        assert!(registry.exact("C").is_none(), "too short");
    }

    #[test]
    fn issuers_are_named_as_whole_tokens_and_the_longest_name_wins() {
        let registry = fixture_registry();
        let names = |s: &str| -> Vec<String> {
            registry
                .issuers_in(s)
                .iter()
                .map(|b| b.name.clone())
                .collect()
        };
        assert_eq!(names("대상: T 멤버십 할인형 고객"), ["SK텔레콤"]);
        assert_eq!(
            names("SKT 멤버십 고객 10% 할인"),
            ["SK텔레콤"],
            "SKT never reads KT"
        );
        assert_eq!(names("KB국민카드로 결제 시 20% 할인"), ["KB국민카드"]);
        assert_eq!(
            names("토스페이, 삼성카드 결제 시 1,000원 할인"),
            ["토스", "삼성카드"]
        );
        assert!(names("전 고객 10% 할인").is_empty());
        assert!(names("배스킨라빈스 싱글레귤러").is_empty(), "not an issuer");
        // Code review C-2: both boundaries, and a longer brand name wins.
        for other in [
            "KTX 승차권 소지 고객 10% 할인",
            "토스트 세트 10% 할인",
            "토스뱅크 체크카드로 결제 시 5% 할인",
            "카카오 T 멤버십 회원 10% 할인",
            "KT&G 임직원 10% 할인",
            "토스증권 고객 10% 할인",
            "삼성카드몰 10% 할인",
        ] {
            assert!(names(other).is_empty(), "{other}: {:?}", names(other));
        }
        assert_eq!(names("KT 고객 10% 할인"), ["KT"]);
        assert_eq!(names("신한카드 고객 10% 할인"), ["신한카드"]);
        assert_eq!(names("(KT멤버십) 10% 할인"), ["KT"]);
    }
}
