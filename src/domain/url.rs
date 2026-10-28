use std::fmt;

use http::HeaderValue;
use url::Url;

use super::{ValidationError, invalid, short_code::ShortCode};

pub const MAX_URL_BYTES: usize = 8_192;

pub struct DestinationUrl(String);
pub struct StoredUrl(String);
#[derive(Clone)]
pub struct PublicOrigin(String);

impl DestinationUrl {
    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        validate_raw_url(raw)?;
        let parsed = Url::parse(raw).map_err(|_| invalid("destination URL is invalid"))?;
        validate_http_url(&parsed)?;
        let serialized = parsed.to_string();
        if serialized.len() > MAX_URL_BYTES {
            return Err(invalid("serialized destination URL is too long"));
        }
        HeaderValue::from_str(&serialized)
            .map_err(|_| invalid("destination URL cannot be used as a Location header"))?;
        Ok(Self(serialized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn location_header(&self) -> HeaderValue {
        // Construction already checked the exact serialized value.
        HeaderValue::from_str(&self.0).expect("validated URL is a valid header value")
    }
}

impl StoredUrl {
    /// Checks a database URL without applying request-input normalization or
    /// replacing an immutable historical snapshot with a newly serialized URL.
    pub fn from_database(raw: String) -> Result<Self, ValidationError> {
        validate_raw_url(&raw)?;
        let parsed = Url::parse(&raw).map_err(|_| invalid("stored URL is invalid"))?;
        validate_http_url(&parsed)?;
        if parsed.as_str() != raw {
            return Err(invalid("stored URL is not in canonical serialized form"));
        }
        HeaderValue::from_str(&raw)
            .map_err(|_| invalid("stored URL cannot be used as a Location header"))?;
        Ok(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn location_header(&self) -> HeaderValue {
        HeaderValue::from_str(&self.0).expect("validated stored URL is a valid header value")
    }
}

impl PublicOrigin {
    pub fn parse(raw: &str) -> Result<Self, ValidationError> {
        if raw
            .chars()
            .any(|c| is_whitespace(c) || is_control(c) || c == '\\')
        {
            return Err(invalid("public origin is invalid"));
        }
        let (scheme, rest) = raw
            .split_once("://")
            .ok_or(invalid("public origin is invalid"))?;
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return Err(invalid("public origin is invalid"));
        }
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..end];
        let suffix = &rest[end..];
        if authority.is_empty()
            || authority.contains('@')
            || authority.contains('\\')
            || !matches!(suffix, "" | "/")
            || validate_authority_port(authority).is_err()
        {
            return Err(invalid("public origin is invalid"));
        }
        let parsed = Url::parse(raw).map_err(|_| invalid("public origin is invalid"))?;
        validate_http_url(&parsed)?;
        if !matches!(parsed.path(), "" | "/")
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.username() != ""
            || parsed.password().is_some()
        {
            return Err(invalid("public origin is invalid"));
        }
        Ok(Self(parsed.origin().ascii_serialization()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn short_url(&self, code: &ShortCode) -> String {
        format!("{}/{}", self.0, code.as_str())
    }
}

impl fmt::Debug for DestinationUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DestinationUrl([REDACTED])")
    }
}

impl fmt::Debug for StoredUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoredUrl([REDACTED])")
    }
}

impl fmt::Debug for PublicOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PublicOrigin([REDACTED])")
    }
}

fn validate_raw_url(raw: &str) -> Result<(), ValidationError> {
    if raw.is_empty() || raw.len() > MAX_URL_BYTES {
        return Err(invalid("URL length is invalid"));
    }
    if raw.chars().next().is_some_and(is_whitespace)
        || raw.chars().next_back().is_some_and(is_whitespace)
    {
        return Err(invalid("URL has surrounding whitespace"));
    }
    if raw
        .bytes()
        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(invalid(
            "URL contains ASCII whitespace or control characters",
        ));
    }
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or(invalid("URL must have explicit HTTP(S) authority syntax"))?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(invalid("URL scheme is unsupported"));
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty()
        || authority.contains('@')
        || authority.contains('\\')
        || validate_authority_port(authority).is_err()
    {
        return Err(invalid("URL authority is invalid"));
    }
    Ok(())
}

fn validate_authority_port(authority: &str) -> Result<(), ValidationError> {
    let port = if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or(invalid("URL authority is invalid"))?;
        let suffix = &authority[close + 1..];
        if suffix.is_empty() {
            return Ok(());
        }
        Some(
            suffix
                .strip_prefix(':')
                .ok_or(invalid("URL authority is invalid"))?,
        )
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => {
                if host.contains(':') {
                    return Err(invalid("IPv6 URL hosts must use brackets"));
                }
                Some(port)
            }
            None => None,
        }
    };
    if let Some(port) = port
        && (port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || port.parse::<u16>().is_err())
    {
        return Err(invalid("URL port is invalid"));
    }
    Ok(())
}

fn validate_http_url(parsed: &Url) -> Result<(), ValidationError> {
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none_or(str::is_empty)
        || parsed.username() != ""
        || parsed.password().is_some()
    {
        return Err(invalid(
            "URL must be HTTP(S), have a host, and contain no user info",
        ));
    }
    Ok(())
}

fn is_whitespace(c: char) -> bool {
    icu_properties::CodePointSetData::new::<icu_properties::props::WhiteSpace>().contains(c)
}

fn is_control(c: char) -> bool {
    icu_properties::CodePointMapData::<icu_properties::props::GeneralCategory>::new().get(c)
        == icu_properties::props::GeneralCategory::Control
}

#[cfg(test)]
mod tests {
    use super::{DestinationUrl, MAX_URL_BYTES, PublicOrigin, StoredUrl};
    use url::Url;

    #[test]
    fn validates_authority_before_url_parser_repair() {
        let repaired = Url::parse("https://internal:")
            .expect("the URL parser repairs an empty port in the selected url release");
        assert_eq!(repaired.host_str(), Some("internal"));
        assert!(repaired.port().is_none());
        for value in [
            "http:///missing.example",
            "http://@host.example/",
            "http://user@host.example/",
            "http://\\host.example/",
            "http:\\host.example",
            "http://host.example:bad/",
            "http://host.example:99999/",
            " http://host.example/",
            "http://host.example/\u{a0}",
            "ftp://host.example/",
        ] {
            assert!(DestinationUrl::parse(value).is_err(), "rejected: {value:?}");
        }
        for value in [
            "http://localhost/",
            "https://127.0.0.1:8080/a",
            "http://[::1]/",
            "https://intranet/",
            "https://bücher.example/a",
        ] {
            assert!(DestinationUrl::parse(value).is_ok(), "accepted: {value:?}");
        }
    }

    #[test]
    fn serializes_once_preserving_query_and_fragment_and_checks_location() {
        let url = DestinationUrl::parse("HTTPS://bücher.example/café?q=2&q=1#frag")
            .expect("valid URL is normalized");
        assert_eq!(
            url.as_str(),
            "https://xn--bcher-kva.example/caf%C3%A9?q=2&q=1#frag"
        );
        assert_eq!(url.location_header().to_str().unwrap(), url.as_str());
        assert_eq!(
            StoredUrl::from_database(url.as_str().to_owned())
                .expect("serialized URL round trips")
                .as_str(),
            url.as_str()
        );
    }

    #[test]
    fn raw_and_serialized_url_bounds_are_independent() {
        let too_long = format!("http://a/{}", "x".repeat(MAX_URL_BYTES));
        assert!(DestinationUrl::parse(&too_long).is_err());
        let expansion = format!("http://a/{}", "é".repeat((MAX_URL_BYTES - 9) / 2));
        assert!(expansion.len() <= MAX_URL_BYTES);
        assert!(DestinationUrl::parse(&expansion).is_err());
    }

    #[test]
    fn origin_reuses_strict_pre_normalization_rules() {
        assert_eq!(
            PublicOrigin::parse("HTTPS://links.example/")
                .unwrap()
                .as_str(),
            "https://links.example"
        );
        for value in [
            "https://@links.example",
            "https://links.example/path",
            "https://links.example/.",
            "https://links.example?x=1",
            "https://links.example#frag",
            "https://links.example\\path",
        ] {
            assert!(PublicOrigin::parse(value).is_err(), "rejected: {value}");
        }
    }
}
