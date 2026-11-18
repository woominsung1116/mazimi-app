//! Evidence is tied to a program revision, not merely a reachable URL.
use crate::{models::Program, source_policy::canonicalize_public_https_url};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use uuid::Uuid;

pub const MAX_RESOURCES: i64 = 40;

/// The request-free half of the publication host rule (security M1 / code
/// M4): a published host must belong to the programme's ingested official
/// site or to an approved source domain. An IDN (punycode) host never
/// vouches for itself; it and a shared-hosting host need an approved entry
/// for that exact host. `official` still requires the caller to check that
/// the official URL was not written through the admin API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationHost {
    pub host: String,
    pub idn: bool,
    /// Only an entry for this very host approves it.
    pub exact: bool,
    pub official: bool,
}

/// `None` when `url` has no public https host.
pub fn publication_host(url: &str, official_url: Option<&str>) -> Option<PublicationHost> {
    use crate::source_policy::{has_punycode_label, is_shared_hosting, official_site_covers};
    let host = canonicalize_public_https_url(url).ok()?.domain;
    let idn = has_punycode_label(&host);
    let official = !idn
        && official_url
            .and_then(|value| canonicalize_public_https_url(value).ok())
            .is_some_and(|official| official_site_covers(&official.domain, &host));
    Some(PublicationHost {
        exact: idn || is_shared_hosting(&host),
        idn,
        official,
        host,
    })
}

/// `reviewed_by` of an application link the worker verified by itself
/// (`apply_entry` in the worker). No user or admin has this id; a human
/// review replaces it with the reviewer's own.
pub const AUTOMATED_REVIEWER: Uuid = Uuid::from_u128(0x6d617a69_6d69_4170_8000_6170706c7931);

/// Government domains: an application step on any of them is the state's.
const GOVERNMENT_SUFFIXES: &[&str] = &[".go.kr", ".mil.kr"];
/// Public institutions outside `.go.kr` that notices send applicants to:
/// the link resolver's original allowlist, unchanged, now its one home
/// (the resolver and the apply-link reader both read it). A host here is
/// matched with its subdomains. New hosts are not added here: an operator
/// approves them as source domains (`benefit_source_domains`), which the
/// worker and the API both honour.
pub const PUBLIC_INSTITUTION_HOSTS: &[&str] = &[
    "jobaba.net",
    "khug.or.kr",
    "lh.or.kr",
    "sh.co.kr",
    "kepco.co.kr",
];

/// A government domain (`.go.kr`, `.mil.kr`); never an IDN host (homographs).
pub fn government_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    !crate::source_policy::has_punycode_label(&host)
        && GOVERNMENT_SUFFIXES.iter().any(|s| host.ends_with(s))
}

/// The host is a government domain or a listed public institution.
/// IDN hosts never qualify by suffix (homographs).
pub fn public_institution_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if crate::source_policy::has_punycode_label(&host) {
        return false;
    }
    GOVERNMENT_SUFFIXES.iter().any(|s| host.ends_with(s))
        || PUBLIC_INSTITUTION_HOSTS
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// May an automatically derived application link on `url` be published for
/// a programme whose official page is `official_url`? Its host must be the
/// official page's own institution (same registrable domain, shared
/// hosting excluded), a government domain, or a listed public institution. The worker checks this before it verifies a link and the API
/// checks it again on every read (an approved source domain also passes
/// there, as for reviewed links).
pub fn automatic_apply_host(official_url: Option<&str>, url: &str) -> bool {
    let Some(rule) = publication_host(url, official_url) else {
        return false;
    };
    !crate::source_policy::is_shared_hosting(&rule.host)
        && (rule.official || public_institution_host(&rule.host))
}

/// The approved-source-domain half of the rule: an active, verified,
/// approved and unexpired entry covers `$1`, exactly or as a subdomain when
/// the entry allows subdomains and `$2` (exact) is false.
pub const APPROVED_DOMAIN_SQL: &str =
    "SELECT EXISTS(SELECT 1 FROM benefit_source_domains WHERE is_active AND verification_status='verified'
       AND compliance_status='approved' AND approval_expires_at>now()
       AND (domain=$1 OR (NOT $2 AND allow_subdomains AND right($1,length(domain)+1)='.'||domain)))";

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Identity and intake-round fields only. Review evidence answers "same
/// program, same round"; a normalizer version bump, whitespace change or
/// rewritten guidance text must not invalidate every approval at the next
/// sync. Content drift is observed separately through the worker hashes.
pub fn fingerprint(p: &Program) -> String {
    digest(
        serde_json::json!({
            "v":2,"source":p.source_type,"source_id":p.source_id,"provider":p.provider_name,
            "start":p.application_start_at,"end":p.application_end_at,
            "deadline":p.deadline_at,"benefit_valid_until":p.benefit_valid_until
        })
        .to_string()
        .as_bytes(),
    )
}

/// Navigation URLs retain all query keys and fragments; canonicalization is
/// validation only. Never fetch URL credentials or user-specific signed URLs.
pub fn public_url(value: &str) -> bool {
    if value.len() > 2048 || value.chars().any(char::is_control) {
        return false;
    }
    let Ok(parsed) = canonicalize_public_https_url(value) else {
        return false;
    };
    let host = parsed.domain.as_str();
    if [".local", ".internal", ".localhost"]
        .iter()
        .any(|s| host.ends_with(s))
    {
        return false;
    }
    !parsed.url.query_pairs().any(|(key, _)| {
        let k = key.to_ascii_lowercase();
        matches!(
            k.as_str(),
            "token" | "access_token" | "password" | "session" | "sessionid" | "authorization"
        ) || k.starts_with("x-amz-")
            || k.starts_with("x-goog-")
    })
}

/// Language/tracking queries do not make a homepage a program-specific page.
/// Real root-path applications (e.g. ?id=42) keep their navigation state.
pub fn is_generic_portal(value: &str) -> bool {
    let Ok(parsed) = url::Url::parse(value) else {
        return true;
    };
    let path = parsed.path().trim_end_matches('/').to_ascii_lowercase();
    let root = matches!(
        path.as_str(),
        "" | "/index.html" | "/index.htm" | "/index.do" | "/main" | "/main.do" | "/main/main.do"
    );
    root && parsed.query_pairs().all(|(key, _)| {
        let key = key.to_ascii_lowercase();
        matches!(
            key.as_str(),
            "lang" | "language" | "locale" | "hl" | "source" | "ref" | "campaign"
        ) || key.starts_with("utm_")
    }) && parsed.fragment().is_none()
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Resource {
    pub id: Uuid,
    pub program_id: Uuid,
    pub kind: String,
    pub url: String,
    pub source_url: String,
    pub label: String,
    pub status: String,
    pub access: String,
    pub program_fingerprint: String,
    pub discovery_depth: i32,
    pub content_hash: Option<String>,
    pub source_hash: Option<String>,
    pub file_format: Option<String>,
    pub byte_size: Option<i64>,
    pub checked_at: Option<DateTime<Utc>>,
    pub verified_at: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
    pub reviewed_by: Option<Uuid>,
    pub review_note: Option<String>,
    pub check_error: Option<String>,
    /// Worker observation, independent of the reviewer's `status`:
    /// ok | blocked | unreachable | changed | unsupported. None = never probed.
    pub check_status: Option<String>,
    pub consecutive_failures: i32,
    /// Consecutive HTTP 404/410 probes; the only signal that may demote.
    pub consecutive_gone: i32,
    pub last_failure_code: Option<String>,
    pub revision: i64,
}

/// `last_failure_code` of a verified row whose latest probe was redirected
/// off its reviewed site, or to no public https address (review M7). The
/// row is hidden, not demoted: the next clean probe or review clears it.
pub const REDIRECTED_OFF_SITE: &str = "redirect_off_site";

/// How far a `verified_at` stamp may lie in the future and still count as
/// verified. Stamps are written with the database's `now()`, and readers
/// compare them with the application host's clock: those are different
/// machines in production (managed database, separate app host) and in a
/// container VM locally, so a database clock a little ahead would hide a
/// freshly approved or renewed resource. Only the not-yet-valid direction is
/// tolerated, and boundedly (a forged far-future stamp still never counts);
/// expiry (`valid_until`) stays strict.
pub const CLOCK_SKEW_TOLERANCE: chrono::Duration = chrono::Duration::seconds(5);

impl Resource {
    pub fn usable(&self, program_fingerprint: &str, now: DateTime<Utc>) -> bool {
        self.published(program_fingerprint, now)
            && self.valid_until.is_some_and(|date| date > now)
            && self.last_failure_code.as_deref() != Some(REDIRECTED_OFF_SITE)
    }

    /// Reviewed for this round and never rejected, demoted or superseded,
    /// whether or not the verification is still fresh. This is the identity
    /// a user's checklist was built against; expiry only hides it.
    pub fn published(&self, program_fingerprint: &str, now: DateTime<Utc>) -> bool {
        // A changed file is different bytes from the reviewed version; a
        // changed HTML page still leads to the reviewed destination.
        let template_changed =
            self.kind == "template" && self.check_status.as_deref() == Some("changed");
        self.status == "verified"
            && !template_changed
            && self.program_fingerprint == program_fingerprint
            && self.reviewed_by.is_some()
            && self
                .verified_at
                .is_some_and(|date| date <= now + CLOCK_SKEW_TOLERANCE)
            && matches!(self.access.as_str(), "direct" | "login_required")
            && public_url(&self.url)
            && public_url(&self.source_url)
            && (self.kind != "application" || !is_generic_portal(&self.url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_institution_table_is_the_resolvers_original_five() {
        assert_eq!(
            PUBLIC_INSTITUTION_HOSTS,
            &[
                "jobaba.net",
                "khug.or.kr",
                "lh.or.kr",
                "sh.co.kr",
                "kepco.co.kr"
            ]
        );
    }

    #[test]
    fn automatic_apply_hosts_are_the_official_family_government_or_listed() {
        let gov24 = Some(
            "https://plus.gov.kr/portal/benefitV2/benefitTotalSrvcList/benefitSrvcDtl?svcId=1",
        );
        let city = Some("https://youth.seoul.go.kr/youthConts.do?key=1");
        let foundation = Some("https://www.gill.or.kr/gill/cntnts/i-291/web.do");
        for (official, url) in [
            // Same registrable domain as the official page.
            (
                gov24,
                "https://www.gov.kr/mw/AA020InfoCappView.do?CappBizCD=1",
            ),
            (foundation, "https://apply.gill.or.kr/form/1"),
            // Any government domain.
            (
                gov24,
                "https://www.bokjiro.go.kr/ssis-tbu/twoaa/aplySrvChc/selectCvlcptServChs.do",
            ),
            (city, "https://www.work24.go.kr/apply"),
            // Listed public institutions, with subdomains.
            (city, "https://apply.jobaba.net/x"),
        ] {
            assert!(automatic_apply_host(official, url), "{url}");
        }
        for (official, url) in [
            // An unrelated organisation domain is not vouched for.
            (city, "https://www.example.or.kr/apply"),
            (city, "https://shop.example.com/apply"),
            // Lookalikes and suffix tricks.
            (city, "https://www.work24.go.kr.evil.example/apply"),
            (city, "https://evilgov.kr/apply"),
            (city, "https://jobaba.net.evil.example/x"),
            // Not listed: banks and other institutions need an operator's
            // source-domain approval, not a code list.
            (city, "https://obank.kbstar.com/quics?page=C1"),
            (city, "https://ols.semas.or.kr/ols/man/SMAN010M/page.do"),
            // IDN hosts never pass by suffix.
            (city, "https://xn--3e0b707e.go.kr/apply"),
            // Shared hosting never, even when the official page is there.
            (
                Some("https://blog.naver.com/city/1"),
                "https://blog.naver.com/city/2",
            ),
            (city, "https://forms.gle/abc"),
            // Only public https URLs.
            (city, "http://www.work24.go.kr/apply"),
            (city, "https://127.0.0.1/apply"),
        ] {
            assert!(!automatic_apply_host(official, url), "{url}");
        }
        // Without an official URL only the government/listed half applies.
        assert!(automatic_apply_host(None, "https://www.kosaf.go.kr/apply"));
        assert!(!automatic_apply_host(
            None,
            "https://apply.gill.or.kr/form/1"
        ));
    }

    fn program() -> Program {
        Program {
            id: Uuid::new_v4(),
            program_type: "policy".into(),
            source_type: "youth_center".into(),
            source_id: Some("R2026-1".into()),
            title: "2026년 청년 지원".into(),
            summary: None,
            provider_name: Some("부산광역시".into()),
            official_url: Some("https://www.busan.go.kr/notice/1".into()),
            program_status: "active".into(),
            application_start_at: None,
            application_end_at: Some("2026-12-01T00:00:00Z".parse().unwrap()),
            benefit_amount_monthly: None,
            benefit_amount_semester: None,
            benefit_amount_once: None,
            region_scope: None,
            school_scope: None,
            tags: None,
            raw_payload: None,
            normalized_payload: None,
            min_age: None,
            max_age: None,
            regions: Some(vec!["busan".into()]),
            deadline_at: None,
            benefit_valid_until: None,
            is_active: true,
            last_synced_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            company_name: None,
            company_logo_url: None,
            benefit_category: None,
            provider_kind: None,
            application_steps: None,
            application_method: Some("온라인신청".into()),
            submission_documents: Some("신분증".into()),
            screening_method: None,
            region_districts: None,
            eligibility_condition: None,
            holder_requirement: None,
            age_basis: None,
        }
    }

    #[test]
    fn fingerprint_tracks_identity_and_round_but_not_normalized_text() {
        let base = program();
        let fp = fingerprint(&base);
        // Ingestion rewrites these on every sync; they must not invalidate reviews.
        let mut cosmetic = program();
        cosmetic.id = base.id;
        cosmetic.title = "2026년  청년 지원 (수정)".into();
        cosmetic.submission_documents = Some("신분증, 통장사본".into());
        cosmetic.application_method = Some("온라인 신청".into());
        cosmetic.regions = Some(vec!["busan".into(), "ulsan".into()]);
        cosmetic.official_url = Some("https://www.busan.go.kr/notice/1?utm_source=x".into());
        cosmetic.program_status = "open".into();
        assert_eq!(fingerprint(&cosmetic), fp);
        // A new intake round or a different source record is a new identity.
        let mut round = program();
        round.application_end_at = Some("2027-12-01T00:00:00Z".parse().unwrap());
        assert_ne!(fingerprint(&round), fp);
        let mut other = program();
        other.source_id = Some("R2027-1".into());
        assert_ne!(fingerprint(&other), fp);
        let mut provider = program();
        provider.provider_name = Some("울산광역시".into());
        assert_ne!(fingerprint(&provider), fp);
    }

    /// One host rule for the review, the public read (M6) and the worker's
    /// daily count (L-4).
    #[test]
    fn publication_host_binds_to_the_official_registrable_domain() {
        let official = Some("https://www.busan.go.kr/notice/1");
        let rule = |url: &str| publication_host(url, official).unwrap();
        assert!(rule("https://apply.busan.go.kr/form").official);
        assert!(!rule("https://busan-go-kr.example.com/form").official);
        let idn = rule("https://xn--80ak6aa92e.busan.go.kr/form");
        assert!(idn.idn && idn.exact && !idn.official);
        let shared = rule("https://form.naver.com/response/1");
        assert!(shared.exact && !shared.official);
        assert!(publication_host("http://apply.busan.go.kr/form", official).is_none());
        assert!(
            !publication_host("https://apply.busan.go.kr/form", None)
                .unwrap()
                .official
        );
    }

    #[test]
    fn changed_template_is_hidden_but_changed_page_stays_published() {
        let p = program();
        let now = Utc::now();
        let mut row = Resource {
            id: Uuid::new_v4(),
            program_id: p.id,
            kind: "application".into(),
            url: "https://www.busan.go.kr/apply/1".into(),
            source_url: "https://www.busan.go.kr/notice/1".into(),
            label: "신청하기".into(),
            status: "verified".into(),
            access: "direct".into(),
            program_fingerprint: fingerprint(&p),
            discovery_depth: 0,
            content_hash: None,
            source_hash: None,
            file_format: None,
            byte_size: None,
            checked_at: None,
            verified_at: Some(now - chrono::Duration::hours(1)),
            valid_until: Some(now + chrono::Duration::hours(1)),
            reviewed_by: Some(Uuid::new_v4()),
            review_note: None,
            check_error: None,
            check_status: Some("changed".into()),
            consecutive_failures: 0,
            consecutive_gone: 0,
            last_failure_code: None,
            revision: 1,
        };
        assert!(row.usable(&fingerprint(&p), now));
        row.kind = "template".into();
        assert!(!row.usable(&fingerprint(&p), now));
        row.check_status = Some("ok".into());
        assert!(row.usable(&fingerprint(&p), now));
        // Review M7: the latest probe was sent off the reviewed site. Hidden
        // (not demoted): still the reviewed identity, shown again after a
        // clean probe or a review clears the code.
        for kind in ["application", "notice", "template"] {
            row.kind = kind.into();
            row.check_status = Some("blocked".into());
            row.last_failure_code = Some(REDIRECTED_OFF_SITE.into());
            assert!(!row.usable(&fingerprint(&p), now), "{kind}");
            assert!(row.published(&fingerprint(&p), now), "{kind}");
            row.last_failure_code = Some("redirect_requires_review".into());
            assert!(row.usable(&fingerprint(&p), now), "{kind}");
        }
    }

    /// A database clock 1 second ahead of the app host stamps `verified_at`
    /// in the reader's future: the row is still verified (tolerance), a
    /// stamp beyond the tolerance is not, and expiry is never stretched.
    #[test]
    fn a_db_clock_slightly_ahead_does_not_hide_a_fresh_approval_but_expiry_stays_strict() {
        let p = program();
        let now = Utc::now();
        let row = |verified: chrono::Duration, valid: chrono::Duration| Resource {
            id: Uuid::new_v4(),
            program_id: p.id,
            kind: "template".into(),
            url: "https://www.busan.go.kr/form.hwp".into(),
            source_url: "https://www.busan.go.kr/notice/1".into(),
            label: "신청서".into(),
            status: "verified".into(),
            access: "direct".into(),
            program_fingerprint: fingerprint(&p),
            discovery_depth: 0,
            content_hash: Some("h".into()),
            source_hash: Some("s".into()),
            file_format: Some("hwp".into()),
            byte_size: Some(10),
            checked_at: Some(now),
            verified_at: Some(now + verified),
            valid_until: Some(now + valid),
            reviewed_by: Some(Uuid::new_v4()),
            review_note: None,
            check_error: None,
            check_status: Some("ok".into()),
            consecutive_failures: 0,
            consecutive_gone: 0,
            last_failure_code: None,
            revision: 1,
        };
        let fp = fingerprint(&p);
        let (second, day) = (chrono::Duration::seconds(1), chrono::Duration::hours(24));
        // DB clock 1s ahead: the approval just written is visible at once.
        assert!(row(second, day).published(&fp, now));
        assert!(row(second, day).usable(&fp, now));
        assert!(row(CLOCK_SKEW_TOLERANCE, day).usable(&fp, now));
        // Beyond the bound: a future stamp is not a verification.
        let beyond = CLOCK_SKEW_TOLERANCE + chrono::Duration::milliseconds(1);
        assert!(!row(beyond, day).published(&fp, now));
        assert!(!row(chrono::Duration::days(3), day).usable(&fp, now));
        // Expiry is strict: no tolerance in that direction.
        let hour = chrono::Duration::hours(1);
        assert!(!row(-hour, chrono::Duration::zero()).usable(&fp, now));
        assert!(!row(-hour, -chrono::Duration::milliseconds(1)).usable(&fp, now));
        assert!(row(-hour, chrono::Duration::milliseconds(1)).usable(&fp, now));
    }

    #[test]
    fn navigation_validation_preserves_scope_and_distinguishes_language_homepages() {
        assert!(is_generic_portal(
            "https://www.uctf.or.kr/?lang=ko&utm_source=x"
        ));
        assert!(!is_generic_portal("https://example.org/?id=42"));
        assert!(!is_generic_portal("https://example.org/#/apply/42"));
        assert!(public_url("https://example.org/apply?id=42#step"));
        for url in [
            "http://example.org/",
            "https://127.0.0.1/a",
            "https://u:p@example.org/a",
            "https://secret.internal/a",
            "https://example.org/?access_token=secret",
        ] {
            assert!(!public_url(url), "{url}");
        }
    }
}
