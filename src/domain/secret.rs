use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http::HeaderMap;
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};

const SECRET_BYTES: usize = 32;
const ACCOUNT_KEY_PREFIX: &str = "ferl_ak_v1_";
const MANAGEMENT_TOKEN_PREFIX: &str = "ferl_mt_v1_";
const ENCODED_SECRET_BYTES: usize = 43;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CredentialClass {
    AccountKey,
    ManagementToken,
}

#[derive(Clone, PartialEq, Eq)]
pub struct SecretDigest([u8; SECRET_BYTES]);
pub struct ParsedCredential {
    class: CredentialClass,
    digest: SecretDigest,
}

/// A deliberate, serializable one-time output. Ordinary DTOs contain neither
/// this type nor its digest; only explicit creation/replacement wrappers do.
pub struct OneTimeSecret(String);
pub struct AccountApiKey(OneTimeSecret);
pub struct ManagementTokenSecret(OneTimeSecret);

pub enum CredentialHeader {
    Missing,
    SuppliedInvalid,
    Parsed(ParsedCredential),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SecretGenerationError;

pub trait SecretRandomSource {
    fn fill(&mut self, output: &mut [u8]) -> Result<(), SecretGenerationError>;
}

impl CredentialClass {
    const fn prefix(self) -> &'static str {
        match self {
            Self::AccountKey => ACCOUNT_KEY_PREFIX,
            Self::ManagementToken => MANAGEMENT_TOKEN_PREFIX,
        }
    }
}

impl SecretDigest {
    pub fn from_presented_secret(complete_secret: &str) -> Self {
        let digest = Sha256::digest(complete_secret.as_bytes());
        let mut bytes = [0; SECRET_BYTES];
        bytes.copy_from_slice(&digest);
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; SECRET_BYTES] {
        &self.0
    }

    /// Validate a digest loaded from PostgreSQL without ever treating malformed
    /// stored bytes as a caller credential failure.
    pub fn from_database_bytes(bytes: &[u8]) -> Option<Self> {
        let value: [u8; SECRET_BYTES] = bytes.try_into().ok()?;
        Some(Self(value))
    }
}

impl ParsedCredential {
    pub const fn class(&self) -> CredentialClass {
        self.class
    }

    pub const fn digest(&self) -> &SecretDigest {
        &self.digest
    }
}

impl OneTimeSecret {
    pub fn generate(class: CredentialClass) -> Result<Self, SecretGenerationError> {
        struct OsRandom;
        impl SecretRandomSource for OsRandom {
            fn fill(&mut self, output: &mut [u8]) -> Result<(), SecretGenerationError> {
                getrandom::fill(output).map_err(|_| SecretGenerationError)
            }
        }
        Self::generate_with(class, &mut OsRandom)
    }

    pub fn generate_with(
        class: CredentialClass,
        random: &mut impl SecretRandomSource,
    ) -> Result<Self, SecretGenerationError> {
        let mut bytes = [0_u8; SECRET_BYTES];
        random.fill(&mut bytes)?;
        Ok(Self(format!(
            "{}{}",
            class.prefix(),
            URL_SAFE_NO_PAD.encode(bytes)
        )))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn digest(&self) -> SecretDigest {
        SecretDigest::from_presented_secret(&self.0)
    }
}

impl AccountApiKey {
    pub fn generate() -> Result<Self, SecretGenerationError> {
        OneTimeSecret::generate(CredentialClass::AccountKey).map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn digest(&self) -> SecretDigest {
        self.0.digest()
    }
}

impl ManagementTokenSecret {
    pub fn generate() -> Result<Self, SecretGenerationError> {
        OneTimeSecret::generate(CredentialClass::ManagementToken).map(Self)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn digest(&self) -> SecretDigest {
        self.0.digest()
    }
}

impl CredentialHeader {
    /// Parses exactly one HTTP Authorization field. Missing and supplied-invalid
    /// remain distinct so callers cannot silently fall back to anonymous access.
    pub fn parse(headers: &HeaderMap) -> Self {
        let mut values = headers.get_all(http::header::AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return Self::Missing;
        };
        if values.next().is_some() {
            return Self::SuppliedInvalid;
        }
        let Ok(value) = value.to_str() else {
            return Self::SuppliedInvalid;
        };
        let Some((scheme, credentials)) = value.split_once(' ') else {
            return Self::SuppliedInvalid;
        };
        if !scheme.eq_ignore_ascii_case("bearer")
            || scheme.is_empty()
            || !scheme.bytes().all(is_http_token_byte)
        {
            return Self::SuppliedInvalid;
        }
        let token = credentials.trim_start_matches(' ');
        if token.is_empty()
            || token.bytes().any(|byte| !is_token68_byte(byte))
            || token.contains(',')
        {
            return Self::SuppliedInvalid;
        }
        match parse_secret(token) {
            Some(parsed) => Self::Parsed(parsed),
            None => Self::SuppliedInvalid,
        }
    }
}

fn parse_secret(secret: &str) -> Option<ParsedCredential> {
    let (class, payload) = if let Some(payload) = secret.strip_prefix(ACCOUNT_KEY_PREFIX) {
        (CredentialClass::AccountKey, payload)
    } else {
        (
            CredentialClass::ManagementToken,
            secret.strip_prefix(MANAGEMENT_TOKEN_PREFIX)?,
        )
    };
    if payload.len() != ENCODED_SECRET_BYTES {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    if decoded.len() != SECRET_BYTES || URL_SAFE_NO_PAD.encode(&decoded) != payload {
        return None;
    }
    Some(ParsedCredential {
        class,
        digest: SecretDigest::from_presented_secret(secret),
    })
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn is_token68_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte)
}

impl Serialize for OneTimeSecret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl Serialize for AccountApiKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl Serialize for ManagementTokenSecret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl fmt::Debug for SecretDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretDigest([REDACTED])")
    }
}

impl fmt::Debug for ParsedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParsedCredential")
            .field("class", &self.class)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for OneTimeSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OneTimeSecret([REDACTED])")
    }
}

impl fmt::Debug for AccountApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccountApiKey([REDACTED])")
    }
}

impl fmt::Debug for ManagementTokenSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ManagementTokenSecret([REDACTED])")
    }
}

impl fmt::Debug for CredentialHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("CredentialHeader::Missing"),
            Self::SuppliedInvalid => f.write_str("CredentialHeader::SuppliedInvalid"),
            Self::Parsed(credential) => f
                .debug_tuple("CredentialHeader::Parsed")
                .field(credential)
                .finish(),
        }
    }
}

impl fmt::Debug for CredentialClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AccountKey => "AccountKey",
            Self::ManagementToken => "ManagementToken",
        })
    }
}

impl fmt::Debug for SecretGenerationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretGenerationError")
    }
}

impl fmt::Display for SecretGenerationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cryptographic randomness is unavailable")
    }
}

impl std::error::Error for SecretGenerationError {}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fmt::Write};

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};

    use super::{
        CredentialClass, CredentialHeader, OneTimeSecret, SECRET_BYTES, SecretDigest,
        SecretGenerationError, SecretRandomSource,
    };

    struct FixedRandom {
        calls: Cell<usize>,
        fail: bool,
    }

    impl SecretRandomSource for FixedRandom {
        fn fill(&mut self, output: &mut [u8]) -> Result<(), SecretGenerationError> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                return Err(SecretGenerationError);
            }
            output.fill(0x5a);
            Ok(())
        }
    }

    fn credential(class: &str, data: &[u8; 32]) -> String {
        format!("ferl_{class}_v1_{}", URL_SAFE_NO_PAD.encode(data))
    }

    #[test]
    fn formats_are_versioned_distinct_and_contain_256_random_bits() {
        let mut rng = FixedRandom {
            calls: Cell::new(0),
            fail: false,
        };
        for (class, prefix) in [
            (CredentialClass::AccountKey, "ferl_ak_v1_"),
            (CredentialClass::ManagementToken, "ferl_mt_v1_"),
        ] {
            let secret = OneTimeSecret::generate_with(class, &mut rng).unwrap();
            assert!(secret.as_str().starts_with(prefix));
            let encoded = secret.as_str().strip_prefix(prefix).unwrap();
            let decoded = URL_SAFE_NO_PAD.decode(encoded).unwrap();
            assert_eq!(decoded.len(), SECRET_BYTES);
            assert_eq!(URL_SAFE_NO_PAD.encode(decoded), encoded);
            assert_eq!(
                serde_json::to_string(&secret).unwrap(),
                format!("\"{}\"", secret.as_str())
            );
            assert!(!format!("{secret:?}").contains(secret.as_str()));
        }
        assert_eq!(rng.calls.get(), 2);
    }

    #[test]
    fn random_failure_never_falls_back_to_a_secret() {
        let mut rng = FixedRandom {
            calls: Cell::new(0),
            fail: true,
        };
        assert!(OneTimeSecret::generate_with(CredentialClass::AccountKey, &mut rng).is_err());
        assert_eq!(rng.calls.get(), 1);
    }

    #[test]
    fn authorization_syntax_distinguishes_missing_invalid_and_parsed() {
        let mut headers = HeaderMap::new();
        assert!(matches!(
            CredentialHeader::parse(&headers),
            CredentialHeader::Missing
        ));
        for raw in [
            "Basic abc",
            "Bearer",
            "Bearer ",
            "Bearer\tferl_ak_v1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "Bearer ferl_ak_v1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa=",
            "Bearer ferl_ak_v1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa, other",
            "Bearer ferl_ak_v2_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "Bearer ferl_ak_v1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            headers.insert(AUTHORIZATION, HeaderValue::from_str(raw).unwrap());
            assert!(matches!(
                CredentialHeader::parse(&headers),
                CredentialHeader::SuppliedInvalid
            ));
        }

        let valid = credential("ak", &[7; 32]);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("bEaReR   {valid}")).unwrap(),
        );
        let CredentialHeader::Parsed(parsed) = CredentialHeader::parse(&headers) else {
            panic!("well-formed bearer secret is parsed syntactically");
        };
        assert_eq!(parsed.class(), CredentialClass::AccountKey);
        assert!(!format!("{parsed:?}").contains(&valid));

        headers.append(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer another-value"),
        );
        assert!(matches!(
            CredentialHeader::parse(&headers),
            CredentialHeader::SuppliedInvalid
        ));
    }

    #[test]
    fn digest_hashes_the_complete_presented_secret_known_answer() {
        let presented = "ferl_ak_v1_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        // Independently computed with Python hashlib.sha256 over the full UTF-8 string.
        let expected = "123cbc8df3e06545fc2ab16945365d1c836f5c7a636a593eb32b52bdce1eb750";
        let digest = SecretDigest::from_presented_secret(presented);
        let actual: String = digest
            .as_bytes()
            .iter()
            .fold(String::new(), |mut output, byte| {
                write!(&mut output, "{byte:02x}").unwrap();
                output
            });
        assert_eq!(actual, expected);
        assert!(!format!("{digest:?}").contains(&actual));
    }

    #[test]
    fn stored_digest_conversion_is_exactly_32_bytes() {
        assert!(SecretDigest::from_database_bytes(&[0; 31]).is_none());
        assert!(SecretDigest::from_database_bytes(&[0; 33]).is_none());
        assert_eq!(
            SecretDigest::from_database_bytes(&[7; 32])
                .expect("valid digest")
                .as_bytes(),
            &[7; 32]
        );
    }

    #[test]
    fn parsed_credential_hash_is_never_serialized() {
        let value = credential("mt", &[1; 32]);
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {value}")).unwrap(),
        );
        let parsed = CredentialHeader::parse(&headers);
        assert!(matches!(parsed, CredentialHeader::Parsed(_)));
        assert!(
            !serde_json::to_string(&format!("{parsed:?}"))
                .unwrap()
                .contains(&value)
        );
    }
}
