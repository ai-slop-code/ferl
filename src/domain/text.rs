use std::{collections::BTreeSet, fmt};

use icu_casemap::CaseMapper;
use icu_normalizer::ComposingNormalizerBorrowed;
use serde::Serialize;

use super::{ValidationError, invalid};

pub const MAX_TAG_INPUT_SCALARS: usize = 256;
pub const MAX_TAG_INPUTS: usize = 20;

#[derive(Serialize)]
#[serde(transparent)]
pub struct Username(String);

#[derive(Serialize)]
#[serde(transparent)]
pub struct Title(String);

#[derive(Serialize)]
#[serde(transparent)]
pub struct Tag(String);

#[derive(Serialize)]
#[serde(transparent)]
pub struct SearchQuery(String);

#[derive(Serialize)]
#[serde(transparent)]
pub struct ComparisonText(String);

#[derive(Serialize)]
#[serde(transparent)]
pub struct TagSet(Vec<Tag>);

impl Username {
    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        if (3..=64).contains(&raw.len())
            && raw
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
        {
            Ok(Self(raw.to_owned()))
        } else {
            Err(invalid("username syntax is invalid"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Title {
    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        let normalized = trim_unicode(raw);
        let count = normalized.chars().count();
        if !(1..=200).contains(&count) || normalized.chars().any(is_control) {
            return Err(invalid("title must contain 1–200 non-control characters"));
        }
        Ok(Self(normalized.to_owned()))
    }

    pub fn from_database(raw: String) -> Result<Self, ValidationError> {
        let count = raw.chars().count();
        if !(1..=200).contains(&count) || raw.chars().any(is_control) || trim_unicode(&raw) != raw {
            return Err(invalid("stored title violates its canonical invariant"));
        }
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Tag {
    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        if raw.chars().count() > MAX_TAG_INPUT_SCALARS {
            return Err(invalid("tag input is too long"));
        }
        Self::canonicalize(trim_unicode(raw))
    }

    pub fn from_database(raw: String) -> Result<Self, ValidationError> {
        let tag = Self::canonicalize(&raw)?;
        if tag.0 != raw {
            return Err(invalid("stored tag is not canonical"));
        }
        Ok(tag)
    }

    fn canonicalize(trimmed: &str) -> Result<Self, ValidationError> {
        let canonical = casefold_nfc(trimmed);
        let scalars = canonical.chars().count();
        if !(1..=64).contains(&scalars) || canonical.chars().any(is_control) {
            return Err(invalid(
                "canonical tag must contain 1–64 non-control characters",
            ));
        }
        Ok(Self(canonical))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TagSet {
    pub fn parse<I, S>(inputs: I) -> Result<Self, ValidationError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut canonical = BTreeSet::new();
        for (index, input) in inputs.into_iter().enumerate() {
            if index >= MAX_TAG_INPUTS {
                return Err(invalid("at most 20 tag values are allowed"));
            }
            canonical.insert(Tag::parse(input.as_ref())?.0);
        }
        Ok(Self(canonical.into_iter().map(Tag).collect()))
    }

    pub fn from_database<I, S>(inputs: I) -> Result<Self, ValidationError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut canonical = BTreeSet::new();
        for input in inputs {
            let value = input.into();
            canonical.insert(Tag::from_database(value)?.0);
        }
        if canonical.len() > MAX_TAG_INPUTS {
            return Err(invalid("stored tag set exceeds its limit"));
        }
        Ok(Self(canonical.into_iter().map(Tag).collect()))
    }

    pub fn as_slice(&self) -> &[Tag] {
        &self.0
    }
}

impl SearchQuery {
    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        let trimmed = trim_unicode(raw);
        let count = trimmed.chars().count();
        if !(1..=256).contains(&count) || trimmed.chars().any(is_control) {
            return Err(invalid("search must contain 1–256 non-control characters"));
        }
        Ok(Self(casefold_nfc(trimmed)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ComparisonText {
    /// Canonical comparison for persisted destination/title/tag data. This
    /// deliberately has no query trimming or query-size limit.
    pub fn from_persisted(raw: &str) -> Self {
        Self(casefold_nfc(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn trim_unicode(raw: &str) -> &str {
    raw.trim_matches(is_whitespace)
}

pub fn casefold_nfc(raw: &str) -> String {
    let folded = CaseMapper::new().fold_string(raw);
    ComposingNormalizerBorrowed::new_nfc()
        .normalize(folded.as_ref())
        .into_owned()
}

pub fn is_control(c: char) -> bool {
    icu_properties::CodePointMapData::<icu_properties::props::GeneralCategory>::new().get(c)
        == icu_properties::props::GeneralCategory::Control
}

pub fn is_whitespace(c: char) -> bool {
    icu_properties::CodePointSetData::new::<icu_properties::props::WhiteSpace>().contains(c)
}

macro_rules! redact_debug {
    ($($name:ty),+ $(,)?) => {$ (
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "([REDACTED])"))
            }
        }
    )+ };
}

redact_debug!(Username, Title, Tag, SearchQuery, ComparisonText, TagSet);

#[cfg(test)]
mod tests {
    use super::{
        ComparisonText, MAX_TAG_INPUT_SCALARS, MAX_TAG_INPUTS, SearchQuery, Tag, TagSet, Title,
        Username, casefold_nfc,
    };

    #[test]
    fn identifiers_are_exact_and_custom_codes_are_not_reserved_here() {
        assert!(Username::parse("abc-_09").is_ok());
        for invalid in ["ab", "ABC", "a b", "éabc", " abc", "abc\n"] {
            assert!(Username::parse(invalid).is_err());
        }
    }

    #[test]
    fn title_uses_unicode_scalar_limits_and_preserves_casing() {
        assert_eq!(
            Title::parse("\u{2003}Hello\u{a0}").unwrap().as_str(),
            "Hello"
        );
        assert_eq!(
            Title::parse(&"é".repeat(200))
                .unwrap()
                .as_str()
                .chars()
                .count(),
            200
        );
        assert!(Title::parse(&"é".repeat(201)).is_err());
        assert!(Title::parse("\u{a0}\u{2003}").is_err());
        assert!(Title::parse("a\u{85}b").is_err());
        assert!(Title::parse("a\u{9f}b").is_err());
    }

    #[test]
    fn full_fold_then_nfc_handles_expansion_sigma_and_composition() {
        assert_eq!(casefold_nfc("Straße"), "strasse");
        assert_eq!(casefold_nfc("STRASSE"), "strasse");
        assert_eq!(casefold_nfc("Σςσ"), "σσσ");
        assert_eq!(casefold_nfc("É"), "é");
        assert_eq!(casefold_nfc("E\u{301}"), "é");
        assert_eq!(casefold_nfc(&casefold_nfc("E\u{301}")), "é");
        assert_eq!(
            Tag::parse("\u{2003}Straße\u{a0}").unwrap().as_str(),
            "strasse"
        );
    }

    #[test]
    fn tags_apply_raw_limits_before_dedup_and_sort_by_utf8_bytes() {
        assert!(Tag::parse(&"a".repeat(MAX_TAG_INPUT_SCALARS)).is_err());
        assert!(Tag::parse(&"a".repeat(MAX_TAG_INPUT_SCALARS + 1)).is_err());
        let exactly_256 = format!("{}a{}", "\u{2003}".repeat(128), "\u{a0}".repeat(127));
        assert_eq!(Tag::parse(&exactly_256).unwrap().as_str(), "a");
        let two_fifty_seven = format!("{exactly_256}\u{2003}");
        assert!(Tag::parse(&two_fifty_seven).is_err());
        assert_eq!(
            Tag::parse(&"é".repeat(64))
                .unwrap()
                .as_str()
                .chars()
                .count(),
            64
        );
        assert!(Tag::parse(&"é".repeat(65)).is_err());
        let tags = TagSet::parse(["Work", "work", " work ", "rust", "É", "E\u{301}"]).unwrap();
        let actual: Vec<_> = tags.as_slice().iter().map(Tag::as_str).collect();
        assert_eq!(actual, ["rust", "work", "é"]);
        assert!(TagSet::parse(std::iter::repeat_n("x", MAX_TAG_INPUTS + 1)).is_err());
        assert!(TagSet::parse(["", "work"]).is_err());
        assert!(TagSet::parse(["a\u{1f}"]).is_err());
        assert!(TagSet::parse(["a\u{80}"]).is_err());
    }

    #[test]
    fn search_limit_is_on_trimmed_input_not_folded_expansion_or_bytes() {
        assert_eq!(SearchQuery::parse(" Straße ").unwrap().as_str(), "strasse");
        assert!(SearchQuery::parse(&"ß".repeat(256)).is_ok());
        assert!(SearchQuery::parse(&"é".repeat(256)).is_ok());
        assert!(SearchQuery::parse(&"a".repeat(257)).is_err());
        assert!(SearchQuery::parse("\u{2003}\u{a0}").is_err());
        assert!(SearchQuery::parse("a\u{9f}b").is_err());
        assert_eq!(
            ComparisonText::from_persisted(&format!("  {}  ", "ß".repeat(300))).as_str(),
            &format!("  {}  ", "ss".repeat(300))
        );
    }
}
