use std::{fmt, net::IpAddr, str::FromStr};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload as CryptoPayload},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{
    ids::{AccountId, LinkId, VisitId},
    text::Tag,
    text::casefold_nfc,
    time::Timestamp,
};

use super::context::{CursorContext, CursorResource};
use crate::http::query::{GroupBy, LinkSort};

pub const MAX_CURSOR_GROUPING_VALUE_BYTES: usize = 8_192;
/// Maximum decoded `nonce || ciphertext || tag`, applied before AEAD work.
pub const MAX_CURSOR_ENVELOPE_BYTES: usize = 65_536;
/// `fc1x.` prefix plus unpadded base64url expansion of the decoded envelope.
pub const MAX_CURSOR_ENCODED_BYTES: usize = 87_387;

const NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
const MAX_PLAINTEXT_BYTES: usize = MAX_CURSOR_ENVELOPE_BYTES - NONCE_BYTES - TAG_BYTES;
const PREFIX_LENGTH: usize = 5;

#[derive(Clone)]
pub struct CursorCodec {
    key: [u8; 32],
}

pub enum CursorBoundary {
    AccountCreatedAt {
        at: Timestamp,
        id: AccountId,
    },
    LinkCreatedAt {
        at: Timestamp,
        id: LinkId,
    },
    LinkTitle {
        normalized_title: Option<String>,
        id: LinkId,
    },
    LinkVisitCount {
        count: i64,
        id: LinkId,
    },
    TagName {
        name: String,
    },
    VisitOccurredAt {
        at: Timestamp,
        id: VisitId,
    },
    BreakdownValue {
        value: Option<String>,
    },
}

pub enum CursorError {
    InvalidCursor,
    RandomnessUnavailable,
    CapacityExceeded,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    version: u8,
    resource: CursorResource,
    context_digest: [u8; 32],
    boundary: PayloadBoundary,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PayloadBoundary {
    AccountCreatedAt {
        created_at_micros: i64,
        id: Uuid,
    },
    LinkCreatedAt {
        created_at_micros: i64,
        id: Uuid,
    },
    LinkTitle {
        #[serde(deserialize_with = "required_nullable")]
        normalized_title: Option<String>,
        id: Uuid,
    },
    LinkVisitCount {
        count: i64,
        id: Uuid,
    },
    TagName {
        name: String,
    },
    VisitOccurredAt {
        occurred_at_micros: i64,
        id: Uuid,
    },
    BreakdownValue {
        #[serde(deserialize_with = "required_nullable")]
        value: Option<String>,
    },
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

impl CursorCodec {
    pub const fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub fn encode(
        &self,
        context: &CursorContext,
        boundary: &CursorBoundary,
    ) -> Result<String, CursorError> {
        let boundary = to_payload_boundary(boundary)?;
        if !compatible(context, &boundary) || validate_context_boundary(context, &boundary).is_err()
        {
            return Err(CursorError::InvalidCursor);
        }
        let payload = Payload {
            version: 1,
            resource: context.resource(),
            context_digest: *context.digest(),
            boundary,
        };
        let plaintext = serde_json::to_vec(&payload).map_err(|_| CursorError::InvalidCursor)?;
        if plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(CursorError::CapacityExceeded);
        }
        let mut nonce = [0_u8; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|_| CursorError::RandomnessUnavailable)?;
        let prefix = format!("fc1{}.", resource_code(context.resource()));
        let cipher =
            XChaCha20Poly1305::new_from_slice(&self.key).map_err(|_| CursorError::InvalidCursor)?;
        let nonce = XNonce::try_from(nonce.as_slice()).map_err(|_| CursorError::InvalidCursor)?;
        let ciphertext = cipher
            .encrypt(
                &nonce,
                CryptoPayload {
                    msg: &plaintext,
                    aad: prefix.as_bytes(),
                },
            )
            .map_err(|_| CursorError::InvalidCursor)?;
        let mut envelope = Vec::with_capacity(nonce.len() + ciphertext.len());
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&ciphertext);
        if envelope.len() > MAX_CURSOR_ENVELOPE_BYTES {
            return Err(CursorError::CapacityExceeded);
        }
        let token = format!("{prefix}{}", URL_SAFE_NO_PAD.encode(envelope));
        if token.len() > MAX_CURSOR_ENCODED_BYTES {
            return Err(CursorError::CapacityExceeded);
        }
        Ok(token)
    }

    pub fn decode(
        &self,
        context: &CursorContext,
        token: &str,
    ) -> Result<CursorBoundary, CursorError> {
        if token.len() > MAX_CURSOR_ENCODED_BYTES || token.len() < PREFIX_LENGTH + 54 {
            return Err(CursorError::InvalidCursor);
        }
        let expected_prefix = format!("fc1{}.", resource_code(context.resource()));
        if token.get(..PREFIX_LENGTH) != Some(expected_prefix.as_str()) {
            return Err(CursorError::InvalidCursor);
        }
        let encoded = &token[PREFIX_LENGTH..];
        let envelope = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| CursorError::InvalidCursor)?;
        if envelope.len() > MAX_CURSOR_ENVELOPE_BYTES
            || envelope.len() < NONCE_BYTES + TAG_BYTES
            || URL_SAFE_NO_PAD.encode(&envelope) != encoded
        {
            return Err(CursorError::InvalidCursor);
        }
        let (nonce, ciphertext) = envelope.split_at(NONCE_BYTES);
        let cipher =
            XChaCha20Poly1305::new_from_slice(&self.key).map_err(|_| CursorError::InvalidCursor)?;
        let nonce = XNonce::try_from(nonce).map_err(|_| CursorError::InvalidCursor)?;
        let plaintext = cipher
            .decrypt(
                &nonce,
                CryptoPayload {
                    msg: ciphertext,
                    aad: expected_prefix.as_bytes(),
                },
            )
            .map_err(|_| CursorError::InvalidCursor)?;
        if plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(CursorError::InvalidCursor);
        }
        let payload: Payload =
            serde_json::from_slice(&plaintext).map_err(|_| CursorError::InvalidCursor)?;
        if payload.version != 1
            || payload.resource != context.resource()
            || payload.context_digest != *context.digest()
            || !compatible(context, &payload.boundary)
            || validate_context_boundary(context, &payload.boundary).is_err()
        {
            return Err(CursorError::InvalidCursor);
        }
        from_payload_boundary(payload.boundary)
    }
}

fn to_payload_boundary(boundary: &CursorBoundary) -> Result<PayloadBoundary, CursorError> {
    match boundary {
        CursorBoundary::AccountCreatedAt { at, id } => Ok(PayloadBoundary::AccountCreatedAt {
            created_at_micros: at.unix_micros(),
            id: id.as_uuid(),
        }),
        CursorBoundary::LinkCreatedAt { at, id } => Ok(PayloadBoundary::LinkCreatedAt {
            created_at_micros: at.unix_micros(),
            id: id.as_uuid(),
        }),
        CursorBoundary::LinkTitle {
            normalized_title,
            id,
        } => {
            validate_normalized_title(normalized_title.as_deref())?;
            Ok(PayloadBoundary::LinkTitle {
                normalized_title: normalized_title.clone(),
                id: id.as_uuid(),
            })
        }
        CursorBoundary::LinkVisitCount { count, id } => {
            if *count < 0 {
                return Err(CursorError::InvalidCursor);
            }
            Ok(PayloadBoundary::LinkVisitCount {
                count: *count,
                id: id.as_uuid(),
            })
        }
        CursorBoundary::TagName { name } => {
            validate_tag(name)?;
            Ok(PayloadBoundary::TagName { name: name.clone() })
        }
        CursorBoundary::VisitOccurredAt { at, id } => Ok(PayloadBoundary::VisitOccurredAt {
            occurred_at_micros: at.unix_micros(),
            id: id.as_uuid(),
        }),
        CursorBoundary::BreakdownValue { value } => {
            validate_grouping_value(value.as_deref())?;
            Ok(PayloadBoundary::BreakdownValue {
                value: value.clone(),
            })
        }
    }
}

fn from_payload_boundary(boundary: PayloadBoundary) -> Result<CursorBoundary, CursorError> {
    match boundary {
        PayloadBoundary::AccountCreatedAt {
            created_at_micros,
            id,
        } => Ok(CursorBoundary::AccountCreatedAt {
            at: timestamp(created_at_micros)?,
            id: AccountId::new(id),
        }),
        PayloadBoundary::LinkCreatedAt {
            created_at_micros,
            id,
        } => Ok(CursorBoundary::LinkCreatedAt {
            at: timestamp(created_at_micros)?,
            id: LinkId::new(id),
        }),
        PayloadBoundary::LinkTitle {
            normalized_title,
            id,
        } => {
            validate_normalized_title(normalized_title.as_deref())?;
            Ok(CursorBoundary::LinkTitle {
                normalized_title,
                id: LinkId::new(id),
            })
        }
        PayloadBoundary::LinkVisitCount { count, id } => {
            if count < 0 {
                return Err(CursorError::InvalidCursor);
            }
            Ok(CursorBoundary::LinkVisitCount {
                count,
                id: LinkId::new(id),
            })
        }
        PayloadBoundary::TagName { name } => {
            validate_tag(&name)?;
            Ok(CursorBoundary::TagName { name })
        }
        PayloadBoundary::VisitOccurredAt {
            occurred_at_micros,
            id,
        } => Ok(CursorBoundary::VisitOccurredAt {
            at: timestamp(occurred_at_micros)?,
            id: VisitId::new(id),
        }),
        PayloadBoundary::BreakdownValue { value } => {
            validate_grouping_value(value.as_deref())?;
            Ok(CursorBoundary::BreakdownValue { value })
        }
    }
}

fn timestamp(value: i64) -> Result<Timestamp, CursorError> {
    Timestamp::from_unix_micros(value).map_err(|_| CursorError::InvalidCursor)
}

fn validate_tag(value: &str) -> Result<(), CursorError> {
    if value.len() > 256 || Tag::from_database(value.to_owned()).is_err() {
        return Err(CursorError::InvalidCursor);
    }
    Ok(())
}

fn validate_normalized_title(value: Option<&str>) -> Result<(), CursorError> {
    if value.is_some_and(|value| {
        value.len() > 2_400
            || value.chars().count() > 600
            || value.chars().any(crate::domain::text::is_control)
            || casefold_nfc(value) != value
    }) {
        return Err(CursorError::InvalidCursor);
    }
    Ok(())
}

fn validate_grouping_value(value: Option<&str>) -> Result<(), CursorError> {
    if value.is_some_and(|value| value.len() > MAX_CURSOR_GROUPING_VALUE_BYTES) {
        return Err(CursorError::InvalidCursor);
    }
    Ok(())
}

fn compatible(context: &CursorContext, boundary: &PayloadBoundary) -> bool {
    matches!(
        (context.resource(), context.link_sort(), boundary),
        (
            CursorResource::Accounts,
            _,
            PayloadBoundary::AccountCreatedAt { .. }
        ) | (CursorResource::Tags, _, PayloadBoundary::TagName { .. })
            | (
                CursorResource::Visits,
                _,
                PayloadBoundary::VisitOccurredAt { .. }
            )
            | (
                CursorResource::Links,
                Some(LinkSort::CreatedAt),
                PayloadBoundary::LinkCreatedAt { .. }
            )
            | (
                CursorResource::Links,
                Some(LinkSort::Title),
                PayloadBoundary::LinkTitle { .. }
            )
            | (
                CursorResource::Links,
                Some(LinkSort::VisitCount),
                PayloadBoundary::LinkVisitCount { .. }
            )
            | (
                CursorResource::Breakdown,
                _,
                PayloadBoundary::BreakdownValue { .. }
            )
    )
}

fn validate_context_boundary(
    context: &CursorContext,
    boundary: &PayloadBoundary,
) -> Result<(), CursorError> {
    match boundary {
        PayloadBoundary::LinkTitle {
            normalized_title, ..
        } => validate_normalized_title(normalized_title.as_deref()),
        PayloadBoundary::BreakdownValue { value } => validate_grouping_for(
            context.group_by().ok_or(CursorError::InvalidCursor)?,
            value.as_deref(),
        ),
        _ => Ok(()),
    }
}

fn validate_grouping_for(group: GroupBy, value: Option<&str>) -> Result<(), CursorError> {
    validate_grouping_value(value)?;
    match (group, value) {
        (GroupBy::Referrer | GroupBy::UserAgent, None) => Ok(()),
        (_, None) => Err(CursorError::InvalidCursor),
        (GroupBy::Day, Some(value)) => {
            let parsed = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map_err(|_| CursorError::InvalidCursor)?;
            if parsed.format("%Y-%m-%d").to_string() == value {
                Ok(())
            } else {
                Err(CursorError::InvalidCursor)
            }
        }
        (GroupBy::Method, Some("GET" | "HEAD")) => Ok(()),
        (GroupBy::Method, Some(_)) => Err(CursorError::InvalidCursor),
        (GroupBy::Device, Some("desktop" | "mobile" | "tablet" | "other" | "unknown")) => Ok(()),
        (GroupBy::Device, Some(_)) => Err(CursorError::InvalidCursor),
        (GroupBy::ClientIp, Some(value)) => {
            let parsed = IpAddr::from_str(value).map_err(|_| CursorError::InvalidCursor)?;
            let canonical = match parsed {
                IpAddr::V6(address) => address.to_ipv4_mapped().map_or(parsed, IpAddr::V4),
                other => other,
            };
            if canonical.to_string() == value {
                Ok(())
            } else {
                Err(CursorError::InvalidCursor)
            }
        }
        (GroupBy::Browser | GroupBy::Os, Some("")) => Err(CursorError::InvalidCursor),
        (GroupBy::Browser | GroupBy::Os | GroupBy::Referrer | GroupBy::UserAgent, Some(_)) => {
            Ok(())
        }
    }
}

fn resource_code(resource: CursorResource) -> char {
    match resource {
        CursorResource::Accounts => 'a',
        CursorResource::Links => 'l',
        CursorResource::Tags => 't',
        CursorResource::Visits => 'v',
        CursorResource::Breakdown => 'b',
    }
}

impl fmt::Debug for CursorCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CursorCodec([REDACTED KEY])")
    }
}

impl fmt::Debug for CursorBoundary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CursorBoundary([REDACTED])")
    }
}

impl fmt::Debug for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidCursor => "CursorError::InvalidCursor",
            Self::RandomnessUnavailable => "CursorError::RandomnessUnavailable",
            Self::CapacityExceeded => "CursorError::CapacityExceeded",
        })
    }
}

impl fmt::Display for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidCursor => "cursor is invalid or incompatible",
            Self::RandomnessUnavailable => "cryptographic randomness is unavailable",
            Self::CapacityExceeded => "cursor exceeds the supported capacity",
        })
    }
}

impl std::error::Error for CursorError {}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use chacha20poly1305::{
        XChaCha20Poly1305, XNonce,
        aead::{Aead, KeyInit, Payload as CryptoPayload},
    };
    use uuid::Uuid;

    use crate::{
        domain::{
            ids::{AccountId, LinkId, VisitId},
            time::Timestamp,
        },
        http::query::{BreakdownQuery, LinkListQuery, VisitListQuery},
    };

    use super::{
        CursorBoundary, CursorCodec, CursorError, MAX_CURSOR_ENCODED_BYTES,
        MAX_CURSOR_GROUPING_VALUE_BYTES,
    };
    use crate::pagination::{CursorActor, CursorContext, CursorResource};

    fn time() -> Timestamp {
        Timestamp::parse_rfc3339("2026-10-28T12:34:56.123456Z").unwrap()
    }

    fn account_context() -> CursorContext {
        let query = crate::http::query::AccountListQuery::parse(None).unwrap();
        CursorContext::accounts(AccountId::new(Uuid::from_u128(1)), &query)
    }

    fn authenticate_raw_payload(
        codec: &CursorCodec,
        resource: CursorResource,
        raw: &[u8],
    ) -> String {
        let prefix = format!("fc1{}.", super::resource_code(resource));
        let cipher = XChaCha20Poly1305::new_from_slice(&codec.key).unwrap();
        let mut nonce_bytes = [0; super::NONCE_BYTES];
        getrandom::fill(&mut nonce_bytes).unwrap();
        let nonce = XNonce::try_from(nonce_bytes.as_slice()).unwrap();
        let ciphertext = cipher
            .encrypt(
                &nonce,
                CryptoPayload {
                    msg: raw,
                    aad: prefix.as_bytes(),
                },
            )
            .unwrap();
        let mut envelope = nonce_bytes.to_vec();
        envelope.extend_from_slice(&ciphertext);
        format!("{prefix}{}", URL_SAFE_NO_PAD.encode(envelope))
    }

    #[test]
    fn authenticated_round_trip_is_interoperable_between_separate_codecs() {
        let key = [42; 32];
        let producer = CursorCodec::new(key);
        let consumer = CursorCodec::new(key);
        let context = account_context();
        let boundary = CursorBoundary::AccountCreatedAt {
            at: time(),
            id: AccountId::new(Uuid::from_u128(9)),
        };
        let encoded = producer.encode(&context, &boundary).unwrap();
        assert!(encoded.len() <= MAX_CURSOR_ENCODED_BYTES);
        let decoded = consumer.decode(&context, &encoded).unwrap();
        assert!(
            matches!(decoded, CursorBoundary::AccountCreatedAt { at, id } if at == time() && id.as_uuid() == Uuid::from_u128(9))
        );
        assert_ne!(
            producer.encode(&context, &boundary).unwrap(),
            encoded,
            "each encryption uses a fresh nonce"
        );
    }

    #[test]
    fn tampering_truncation_wrong_key_version_resource_and_context_fail_closed() {
        let key = [3; 32];
        let codec = CursorCodec::new(key);
        let context = account_context();
        let boundary = CursorBoundary::AccountCreatedAt {
            at: time(),
            id: AccountId::new(Uuid::nil()),
        };
        let token = codec.encode(&context, &boundary).unwrap();
        let mut parts = token.clone().into_bytes();
        let last = parts.len() - 1;
        parts[last] = if parts[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(parts).unwrap();
        assert!(codec.decode(&context, &tampered).is_err());
        assert!(codec.decode(&context, &token[..token.len() - 1]).is_err());
        assert!(CursorCodec::new([4; 32]).decode(&context, &token).is_err());
        assert!(codec.decode(&context, "fc2a.invalid").is_err());

        let links = LinkListQuery::parse(None).unwrap();
        assert!(
            codec
                .decode(
                    &CursorContext::links(AccountId::new(Uuid::nil()), &links),
                    &token,
                )
                .is_err()
        );
    }

    #[test]
    fn boundaries_are_resource_typed_and_validate_after_authentication() {
        let key = [5; 32];
        let codec = CursorCodec::new(key);
        let links = LinkListQuery::parse(Some("sort=title")).unwrap();
        let context = CursorContext::links(AccountId::new(Uuid::from_u128(2)), &links);
        assert!(
            codec
                .encode(
                    &context,
                    &CursorBoundary::TagName {
                        name: "work".into()
                    }
                )
                .is_err()
        );
        assert!(
            codec
                .encode(
                    &context,
                    &CursorBoundary::LinkVisitCount {
                        count: -1,
                        id: LinkId::new(Uuid::nil())
                    },
                )
                .is_err()
        );
        assert!(
            codec
                .encode(
                    &context,
                    &CursorBoundary::LinkTitle {
                        normalized_title: Some("x".repeat(2_401)),
                        id: LinkId::new(Uuid::nil()),
                    },
                )
                .is_err()
        );
        let created_order = LinkListQuery::parse(Some("sort=created_at")).unwrap();
        let created_context =
            CursorContext::links(AccountId::new(Uuid::from_u128(2)), &created_order);
        assert!(
            codec
                .encode(
                    &created_context,
                    &CursorBoundary::LinkCreatedAt {
                        at: time(),
                        id: LinkId::new(Uuid::nil())
                    },
                )
                .is_ok()
        );

        let visits = VisitListQuery::parse(None).unwrap();
        let link_id = LinkId::new(Uuid::from_u128(7));
        let anonymous_context =
            CursorContext::visits(CursorActor::AnonymousLink(link_id), link_id, &visits);
        assert!(
            codec
                .encode(
                    &anonymous_context,
                    &CursorBoundary::VisitOccurredAt {
                        at: time(),
                        id: VisitId::new(Uuid::nil())
                    },
                )
                .is_ok()
        );
    }

    #[test]
    fn null_breakdown_boundary_and_maximum_escaped_values_fit_capacity() {
        let codec = CursorCodec::new([7; 32]);
        let breakdown = BreakdownQuery::parse(Some("group_by=user_agent")).unwrap();
        let context = CursorContext::breakdown(
            CursorActor::Account(AccountId::new(Uuid::from_u128(1))),
            LinkId::new(Uuid::from_u128(2)),
            &breakdown,
        );
        let null = CursorBoundary::BreakdownValue { value: None };
        let token = codec.encode(&context, &null).unwrap();
        assert!(matches!(
            codec.decode(&context, &token).unwrap(),
            CursorBoundary::BreakdownValue { value: None }
        ));

        let maximum = "\0\"\\\u{0001}".repeat(1_170);
        let mut maximum = maximum;
        while maximum.len() < MAX_CURSOR_GROUPING_VALUE_BYTES {
            maximum.push('\u{0002}');
        }
        maximum.truncate(MAX_CURSOR_GROUPING_VALUE_BYTES);
        assert_eq!(maximum.len(), MAX_CURSOR_GROUPING_VALUE_BYTES);
        let boundary = CursorBoundary::BreakdownValue {
            value: Some(maximum.clone()),
        };
        let token = codec.encode(&context, &boundary).unwrap();
        assert!(token.len() <= MAX_CURSOR_ENCODED_BYTES);
        assert!(
            matches!(codec.decode(&context, &token).unwrap(), CursorBoundary::BreakdownValue { value: Some(value) } if value == maximum)
        );
        let worst_escaped = "\0".repeat(MAX_CURSOR_GROUPING_VALUE_BYTES);
        let worst_cursor = codec
            .encode(
                &context,
                &CursorBoundary::BreakdownValue {
                    value: Some(worst_escaped.clone()),
                },
            )
            .unwrap();
        assert!(worst_cursor.len() <= MAX_CURSOR_ENCODED_BYTES);
        assert!(matches!(
            codec.decode(&context, &worst_cursor).unwrap(),
            CursorBoundary::BreakdownValue { value: Some(value) } if value == worst_escaped
        ));
        assert!(
            codec
                .encode(
                    &context,
                    &CursorBoundary::BreakdownValue {
                        value: Some("x".repeat(MAX_CURSOR_GROUPING_VALUE_BYTES + 1))
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn maximum_encoded_and_malformed_inputs_are_rejected_before_decrypt() {
        let codec = CursorCodec::new([8; 32]);
        let context = account_context();
        assert!(matches!(
            codec.decode(&context, &"x".repeat(MAX_CURSOR_ENCODED_BYTES + 1)),
            Err(CursorError::InvalidCursor)
        ));
        for value in ["", "fc1a.", "fc1z.aaaaaaaa", "fc1a.====", "fc1a.!!!!"] {
            assert!(codec.decode(&context, value).is_err());
        }
    }

    #[test]
    fn authenticated_payloads_still_require_valid_version_resource_shape_and_values() {
        let codec = CursorCodec::new([0x91; 32]);
        let context = account_context();
        let digest = serde_json::to_value(context.digest()).unwrap();
        let valid_boundary = serde_json::json!({
            "kind": "account_created_at",
            "created_at_micros": 1_798_171_200_000_000_i64,
            "id": Uuid::from_u128(17),
        });
        let mut valid = serde_json::json!({
            "version": 1,
            "resource": "accounts",
            "context_digest": digest,
            "boundary": valid_boundary,
        });
        let valid_token = authenticate_raw_payload(
            &codec,
            CursorResource::Accounts,
            &serde_json::to_vec(&valid).unwrap(),
        );
        assert!(codec.decode(&context, &valid_token).is_ok());

        valid["version"] = serde_json::json!(2);
        let token = authenticate_raw_payload(
            &codec,
            CursorResource::Accounts,
            &serde_json::to_vec(&valid).unwrap(),
        );
        assert!(codec.decode(&context, &token).is_err());

        valid["version"] = serde_json::json!(1);
        valid["boundary"]["created_at_micros"] = serde_json::json!(i64::MAX);
        let token = authenticate_raw_payload(
            &codec,
            CursorResource::Accounts,
            &serde_json::to_vec(&valid).unwrap(),
        );
        assert!(codec.decode(&context, &token).is_err());

        valid["boundary"]["created_at_micros"] = serde_json::json!(1_798_171_200_000_000_i64);
        valid["boundary"]["id"] = serde_json::json!("not-a-uuid");
        let token = authenticate_raw_payload(
            &codec,
            CursorResource::Accounts,
            &serde_json::to_vec(&valid).unwrap(),
        );
        assert!(codec.decode(&context, &token).is_err());

        valid["boundary"] = serde_json::json!({"kind":"tag_name","name":"Work"});
        let token = authenticate_raw_payload(
            &codec,
            CursorResource::Accounts,
            &serde_json::to_vec(&valid).unwrap(),
        );
        assert!(codec.decode(&context, &token).is_err());

        valid["boundary"] = serde_json::json!({
            "kind":"account_created_at",
            "created_at_micros":1_798_171_200_000_000_i64,
            "id":Uuid::from_u128(17),
            "unexpected":"field"
        });
        let token = authenticate_raw_payload(
            &codec,
            CursorResource::Accounts,
            &serde_json::to_vec(&valid).unwrap(),
        );
        assert!(codec.decode(&context, &token).is_err());
    }

    #[test]
    fn authenticated_breakdown_payload_requires_explicit_nullable_boundary() {
        let codec = CursorCodec::new([0x92; 32]);
        let query = BreakdownQuery::parse(Some("group_by=referrer")).unwrap();
        let context = CursorContext::breakdown(
            CursorActor::Account(AccountId::new(Uuid::from_u128(1))),
            LinkId::new(Uuid::from_u128(2)),
            &query,
        );
        let digest = serde_json::to_value(context.digest()).unwrap();
        for boundary in [
            serde_json::json!({"kind":"breakdown_value"}),
            serde_json::json!({"kind":"breakdown_value","value":"x".repeat(MAX_CURSOR_GROUPING_VALUE_BYTES + 1)}),
        ] {
            let raw = serde_json::json!({
                "version":1,
                "resource":"breakdown",
                "context_digest":digest,
                "boundary":boundary,
            });
            let token = authenticate_raw_payload(
                &codec,
                CursorResource::Breakdown,
                &serde_json::to_vec(&raw).unwrap(),
            );
            assert!(codec.decode(&context, &token).is_err());
        }

        let raw = serde_json::json!({
            "version":1,
            "resource":"breakdown",
            "context_digest":digest,
            "boundary":{"kind":"breakdown_value","value":null},
        });
        let token = authenticate_raw_payload(
            &codec,
            CursorResource::Breakdown,
            &serde_json::to_vec(&raw).unwrap(),
        );
        assert!(matches!(
            codec.decode(&context, &token).unwrap(),
            CursorBoundary::BreakdownValue { value: None }
        ));
    }
}
