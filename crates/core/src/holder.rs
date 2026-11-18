//! Holder conditions: what a user must hold for an offer a telco, card
//! issuer or pay app pays for ("T 멤버십 · CGV 4,000원 할인" needs an SK텔레콤
//! line; "KB국민카드 · 쿠팡 즉시할인" needs a KB국민카드).
//!
//! The requirement is never read from page text. It is brand data: the
//! offer's `sponsor` brand (`program_brands.relationship_type = 'sponsor'`)
//! carries one holder key (`brands.holder_field`, `brands.holder_value`),
//! and every offer that brand sponsors inherits it: the one place it is
//! derived is the `current_programs.holder_requirement` column
//! (migration 20261111000102), read into
//! [`crate::models::Program::holder_requirement`]. The profile answers it through three optional
//! fields of [`crate::rule_engine::FIELDS`]; [`crate::audience::membership`]
//! turns requirement and answer into the one three-state check:
//! - holds → passes, as any answered condition;
//! - answered "does not hold" → the row is hidden;
//! - unanswered → 확인 필요, with that field as the missing check.
//!
//! The domains below are the only list of codes: the profile API validates
//! against them and a database test pins the SQL checks to them.

use crate::rule_engine::RuleVerdict;

/// `telecom_carrier`: the user's mobile carrier (알뜰폰 is `mvno`).
pub const TELECOM_CARRIERS: &[&str] = &["skt", "kt", "lgu", "mvno"];

/// `card_issuers`: card companies the user holds a card of.
pub const CARD_ISSUERS: &[&str] = &[
    "kb",
    "shinhan",
    "samsung",
    "hyundai",
    "lotte",
    "woori",
    "hana",
    "bc",
    "nh",
    "ibk",
    "citi",
    "kakaobank",
    "tossbank",
    "kbank",
];

/// `pay_apps`: easy-pay apps the user pays with.
pub const PAY_APPS: &[&str] = &[
    "naverpay",
    "kakaopay",
    "tosspay",
    "payco",
    "samsungpay",
    "applepay",
    "ssgpay",
    "lpay",
];

/// Which profile field answers a holder requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HolderField {
    TelecomCarrier,
    CardIssuers,
    PayApps,
}

impl HolderField {
    pub const ALL: [HolderField; 3] = [Self::TelecomCarrier, Self::CardIssuers, Self::PayApps];

    /// The profile field name, as in [`crate::rule_engine::FIELDS`].
    pub fn name(self) -> &'static str {
        match self {
            Self::TelecomCarrier => "telecom_carrier",
            Self::CardIssuers => "card_issuers",
            Self::PayApps => "pay_apps",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == name)
    }

    /// The codes the field accepts.
    pub fn domain(self) -> &'static [&'static str] {
        match self {
            Self::TelecomCarrier => TELECOM_CARRIERS,
            Self::CardIssuers => CARD_ISSUERS,
            Self::PayApps => PAY_APPS,
        }
    }

    /// One code (carrier) or a set of codes (cards, pay apps).
    pub fn is_set(self) -> bool {
        !matches!(self, Self::TelecomCarrier)
    }

    /// The displayed condition of an offer its holder grants, from the
    /// grantor's name: a template, never page text. Any restriction the
    /// page states is appended verbatim by the writer of the offer.
    pub fn condition_template(self, grantor: &str) -> String {
        match self {
            Self::TelecomCarrier => format!("{grantor} 휴대폰 이용 고객"),
            Self::CardIssuers => format!("{grantor} 보유 고객"),
            Self::PayApps => format!("{grantor} 결제 고객"),
        }
    }
}

/// What an offer requires: one code of one holder field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Holder {
    pub field: HolderField,
    pub value: &'static str,
}

impl Holder {
    /// `field:value` as `current_programs.holder_requirement` states it; `None` for anything
    /// else (an unknown field or code is no requirement this build models,
    /// so the caller treats the row as reserved for nobody it can check).
    /// True when a row states a holder requirement this build cannot read
    /// (code review M-11): such a row is an unanswerable check, never a row
    /// with no requirement.
    pub fn unreadable(raw: Option<&str>) -> bool {
        raw.is_some_and(|text| !text.trim().is_empty() && Self::parse(text).is_none())
    }

    pub fn parse(text: &str) -> Option<Self> {
        let (field, value) = text.split_once(':')?;
        let field = HolderField::parse(field.trim())?;
        let value = field.domain().iter().find(|code| **code == value.trim())?;
        Some(Self { field, value })
    }
}

/// The user's answers to the three holder fields. `None` = not answered;
/// an empty set is an answer ("none").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Holdings {
    carrier: Option<&'static str>,
    /// Bit `i` set = holds `CARD_ISSUERS[i]`.
    cards: Option<u64>,
    /// Bit `i` set = uses `PAY_APPS[i]`.
    pays: Option<u64>,
}

fn mask(domain: &[&str], codes: &[String]) -> u64 {
    codes
        .iter()
        .filter_map(|code| domain.iter().position(|d| d == code))
        .fold(0, |mask, i| mask | (1 << i))
}

impl Holdings {
    /// Nothing answered.
    pub const UNANSWERED: Holdings = Holdings {
        carrier: None,
        cards: None,
        pays: None,
    };

    /// From stored answers. Codes outside a domain are ignored (the API
    /// refuses them on save).
    pub fn from_answers(
        carrier: Option<&str>,
        cards: Option<&[String]>,
        pays: Option<&[String]>,
    ) -> Self {
        Self {
            carrier: carrier.and_then(|c| TELECOM_CARRIERS.iter().copied().find(|d| *d == c)),
            cards: cards.map(|codes| mask(CARD_ISSUERS, codes)),
            pays: pays.map(|codes| mask(PAY_APPS, codes)),
        }
    }

    /// Pass = the user holds it, Fail = answered and does not, Unknown =
    /// not answered.
    pub fn verdict(&self, holder: &Holder) -> RuleVerdict {
        let held = match holder.field {
            HolderField::TelecomCarrier => self.carrier.map(|c| c == holder.value),
            HolderField::CardIssuers => self.cards.map(|m| bit(m, CARD_ISSUERS, holder.value)),
            HolderField::PayApps => self.pays.map(|m| bit(m, PAY_APPS, holder.value)),
        };
        match held {
            Some(true) => RuleVerdict::Pass,
            Some(false) => RuleVerdict::Fail,
            None => RuleVerdict::Unknown,
        }
    }
}

fn bit(mask: u64, domain: &[&str], code: &str) -> bool {
    domain
        .iter()
        .position(|d| *d == code)
        .is_some_and(|i| mask & (1 << i) != 0)
}

/// Validates one answer for the profile API: `Ok(())` when every code is in
/// the field's domain and a set names no code twice, at most the domain's
/// size.
pub fn validate_answer(field: HolderField, codes: &[String]) -> Result<(), &'static str> {
    if !field.is_set() && codes.len() != 1 {
        return Err("holder_one_code");
    }
    if codes.len() > field.domain().len() {
        return Err("holder_too_many");
    }
    for (i, code) in codes.iter().enumerate() {
        if !field.domain().contains(&code.as_str()) {
            return Err("holder_unknown_code");
        }
        if codes[..i].contains(code) {
            return Err("holder_duplicate_code");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_requirement_parses_only_from_its_domains() {
        assert_eq!(
            Holder::parse("telecom_carrier:skt"),
            Some(Holder {
                field: HolderField::TelecomCarrier,
                value: "skt"
            })
        );
        assert_eq!(
            Holder::parse("card_issuers:kb").map(|h| h.field),
            Some(HolderField::CardIssuers)
        );
        for bad in [
            "telecom_carrier:sk",
            "cards:kb",
            "pay_apps",
            "",
            "pay_apps:PAYCO",
        ] {
            assert_eq!(Holder::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn holdings_are_three_state() {
        let skt = Holder::parse("telecom_carrier:skt").unwrap();
        let kb = Holder::parse("card_issuers:kb").unwrap();
        let payco = Holder::parse("pay_apps:payco").unwrap();
        let unanswered = Holdings::default();
        for h in [&skt, &kb, &payco] {
            assert_eq!(unanswered.verdict(h), RuleVerdict::Unknown);
        }
        let none = Holdings::from_answers(Some("kt"), Some(&[]), Some(&[]));
        for h in [&skt, &kb, &payco] {
            assert_eq!(none.verdict(h), RuleVerdict::Fail, "{h:?}");
        }
        let holds = Holdings::from_answers(
            Some("skt"),
            Some(&codes(&["shinhan", "kb"])),
            Some(&codes(&["payco"])),
        );
        for h in [&skt, &kb, &payco] {
            assert_eq!(holds.verdict(h), RuleVerdict::Pass, "{h:?}");
        }
    }

    #[test]
    fn answers_are_validated_against_the_domain() {
        assert!(validate_answer(HolderField::TelecomCarrier, &codes(&["skt"])).is_ok());
        assert!(validate_answer(HolderField::TelecomCarrier, &codes(&["skt", "kt"])).is_err());
        assert!(validate_answer(HolderField::CardIssuers, &codes(&[])).is_ok());
        assert!(validate_answer(HolderField::CardIssuers, &codes(&["kb", "kb"])).is_err());
        assert!(validate_answer(HolderField::PayApps, &codes(&["paypal"])).is_err());
    }

    #[test]
    fn the_condition_is_a_template_of_the_grantor() {
        assert_eq!(
            HolderField::CardIssuers.condition_template("KB국민카드"),
            "KB국민카드 보유 고객"
        );
        assert_eq!(
            HolderField::TelecomCarrier.condition_template("SK텔레콤"),
            "SK텔레콤 휴대폰 이용 고객"
        );
        assert_eq!(
            HolderField::PayApps.condition_template("PAYCO"),
            "PAYCO 결제 고객"
        );
    }

    #[test]
    fn domains_fit_a_mask_and_have_no_duplicates() {
        for field in HolderField::ALL {
            let domain = field.domain();
            assert!(domain.len() <= 64);
            for (i, code) in domain.iter().enumerate() {
                assert!(!domain[..i].contains(code), "{code}");
                assert!(code
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
            }
        }
    }
}
