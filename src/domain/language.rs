//! BCP 47 language-hint parsing and canonicalization.

use std::{collections::HashSet, fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_LANGUAGE_TAG_LENGTH: usize = 64;

/// Validated, casing-normalized BCP 47 language hint.
#[derive(Clone, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct LanguageHint(String);

impl LanguageHint {
    /// Returns the canonical language tag.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the lower-case primary language subtag.
    #[must_use]
    pub fn primary_language(&self) -> &str {
        self.0.split('-').next().unwrap_or_default()
    }
}

impl fmt::Debug for LanguageHint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("LanguageHint")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for LanguageHint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for LanguageHint {
    type Err = LanguageHintError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        canonicalize(value).map(Self)
    }
}

impl TryFrom<String> for LanguageHint {
    type Error = LanguageHintError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<LanguageHint> for String {
    fn from(value: LanguageHint) -> Self {
        value.0
    }
}

/// Language-hint validation failure.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum LanguageHintError {
    /// The tag was empty.
    #[error("language hint must not be empty")]
    Empty,
    /// The public contract bounds language hints to 64 characters.
    #[error("language hint exceeds 64 characters")]
    TooLong,
    /// BCP 47 transport syntax is ASCII.
    #[error("language hint must contain only ASCII characters")]
    NonAscii,
    /// The tag did not satisfy BCP 47 subtag structure.
    #[error("language hint is not a valid BCP 47 tag")]
    InvalidSyntax,
}

// The ordered RFC 5646 grammar is clearer kept in one parser than split into
// stateful helpers whose contracts would duplicate the grammar.
#[allow(clippy::too_many_lines)]
fn canonicalize(value: &str) -> Result<String, LanguageHintError> {
    if value.is_empty() {
        return Err(LanguageHintError::Empty);
    }
    if value.len() > MAX_LANGUAGE_TAG_LENGTH {
        return Err(LanguageHintError::TooLong);
    }
    if !value.is_ascii() {
        return Err(LanguageHintError::NonAscii);
    }
    if value != value.trim() || value.contains('_') {
        return Err(LanguageHintError::InvalidSyntax);
    }

    if let Some(canonical) = canonical_grandfathered(value) {
        return Ok(canonical.to_owned());
    }

    let subtags = value.split('-').collect::<Vec<_>>();
    if subtags.iter().any(|subtag| {
        subtag.is_empty()
            || subtag.len() > 8
            || !subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }) {
        return Err(LanguageHintError::InvalidSyntax);
    }

    if subtags[0].eq_ignore_ascii_case("x") {
        if subtags.len() < 2 {
            return Err(LanguageHintError::InvalidSyntax);
        }
        return Ok(subtags
            .iter()
            .map(|subtag| subtag.to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join("-"));
    }

    let language = subtags[0];
    if !(2..=8).contains(&language.len()) || !is_alpha(language) {
        return Err(LanguageHintError::InvalidSyntax);
    }

    let mut canonical = vec![language.to_ascii_lowercase()];
    let mut index = 1;

    if language.len() <= 3 {
        let mut extlang_count = 0;
        while index < subtags.len()
            && extlang_count < 3
            && subtags[index].len() == 3
            && is_alpha(subtags[index])
        {
            canonical.push(subtags[index].to_ascii_lowercase());
            index += 1;
            extlang_count += 1;
        }
    }

    if index < subtags.len() && subtags[index].len() == 4 && is_alpha(subtags[index]) {
        let lower = subtags[index].to_ascii_lowercase();
        let mut characters = lower.chars();
        if let Some(first) = characters.next() {
            canonical.push(format!(
                "{}{}",
                first.to_ascii_uppercase(),
                characters.as_str()
            ));
        }
        index += 1;
    }

    if index < subtags.len() && is_region(subtags[index]) {
        canonical.push(subtags[index].to_ascii_uppercase());
        index += 1;
    }

    let mut variants = HashSet::new();
    while index < subtags.len() && is_variant(subtags[index]) {
        let variant = subtags[index].to_ascii_lowercase();
        if !variants.insert(variant.clone()) {
            return Err(LanguageHintError::InvalidSyntax);
        }
        canonical.push(variant);
        index += 1;
    }

    let mut extensions = HashSet::new();
    while index < subtags.len()
        && subtags[index].len() == 1
        && !subtags[index].eq_ignore_ascii_case("x")
    {
        let singleton = subtags[index].to_ascii_lowercase();
        if !extensions.insert(singleton.clone()) {
            return Err(LanguageHintError::InvalidSyntax);
        }
        canonical.push(singleton);
        index += 1;

        let start = index;
        while index < subtags.len() && (2..=8).contains(&subtags[index].len()) {
            canonical.push(subtags[index].to_ascii_lowercase());
            index += 1;
        }
        if index == start {
            return Err(LanguageHintError::InvalidSyntax);
        }
    }

    if index < subtags.len() && subtags[index].eq_ignore_ascii_case("x") {
        canonical.push("x".to_owned());
        index += 1;
        if index == subtags.len() {
            return Err(LanguageHintError::InvalidSyntax);
        }
        while index < subtags.len() {
            canonical.push(subtags[index].to_ascii_lowercase());
            index += 1;
        }
    }

    if index != subtags.len() {
        return Err(LanguageHintError::InvalidSyntax);
    }

    Ok(canonical.join("-"))
}

fn is_alpha(value: &str) -> bool {
    value.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn is_region(value: &str) -> bool {
    (value.len() == 2 && is_alpha(value))
        || (value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_variant(value: &str) -> bool {
    ((5..=8).contains(&value.len()))
        || (value.len() == 4 && value.as_bytes().first().is_some_and(u8::is_ascii_digit))
}

fn canonical_grandfathered(value: &str) -> Option<&'static str> {
    const GRANDFATHERED: &[(&str, &str)] = &[
        ("art-lojban", "jbo"),
        ("cel-gaulish", "cel-gaulish"),
        ("en-gb-oed", "en-GB-oxendict"),
        ("i-ami", "ami"),
        ("i-bnn", "bnn"),
        ("i-default", "i-default"),
        ("i-enochian", "i-enochian"),
        ("i-hak", "hak"),
        ("i-klingon", "tlh"),
        ("i-lux", "lb"),
        ("i-mingo", "i-mingo"),
        ("i-navajo", "nv"),
        ("i-pwn", "pwn"),
        ("i-tao", "tao"),
        ("i-tay", "tay"),
        ("i-tsu", "tsu"),
        ("no-bok", "nb"),
        ("no-nyn", "nn"),
        ("sgn-be-fr", "sfb"),
        ("sgn-be-nl", "vgt"),
        ("sgn-ch-de", "sgg"),
        ("zh-guoyu", "cmn"),
        ("zh-hakka", "hak"),
        ("zh-min", "zh-min"),
        ("zh-min-nan", "nan"),
        ("zh-xiang", "hsn"),
    ];

    GRANDFATHERED
        .iter()
        .find(|(candidate, _)| value.eq_ignore_ascii_case(candidate))
        .map(|(_, canonical)| *canonical)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::{LanguageHint, LanguageHintError};

    #[test]
    fn canonicalizes_language_script_region_and_extensions() {
        let language = LanguageHint::from_str("ZH-hant-tw-u-NU-hanidec-x-PRIVATE");

        assert_eq!(
            language.map(|value| value.to_string()),
            Ok("zh-Hant-TW-u-nu-hanidec-x-private".to_owned())
        );
    }

    #[test]
    fn canonicalizes_grandfathered_tags() {
        assert_eq!(
            LanguageHint::from_str("i-KLINGON").map(|value| value.to_string()),
            Ok("tlh".to_owned())
        );
        assert_eq!(
            LanguageHint::from_str("en-GB-oed").map(|value| value.to_string()),
            Ok("en-GB-oxendict".to_owned())
        );
    }

    #[test]
    fn accepts_private_use_tags() {
        assert_eq!(
            LanguageHint::from_str("x-TEAM-voice").map(|value| value.to_string()),
            Ok("x-team-voice".to_owned())
        );
    }

    #[test]
    fn rejects_malformed_and_duplicate_subtags() {
        for invalid in ["", "en_US", "e", "en--US", "en-u", "en-1901-1901", "x"] {
            assert!(
                LanguageHint::from_str(invalid).is_err(),
                "accepted {invalid}"
            );
        }
        assert_eq!(
            LanguageHint::from_str("français"),
            Err(LanguageHintError::NonAscii)
        );
    }

    #[test]
    fn exposes_the_primary_language() {
        let language = LanguageHint::from_str("pt-BR");
        assert_eq!(
            language.map(|value| value.primary_language().to_owned()),
            Ok("pt".to_owned())
        );
    }
}
