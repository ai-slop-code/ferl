use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
    time::Duration,
};

use axum::body::Body;
use http::{HeaderMap, header};
use http_body_util::BodyExt;
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::Value;
use tokio::time::timeout;

pub const MAX_JSON_BODY_BYTES: usize = 65_536;
const DEFAULT_BODY_DEADLINE: Duration = Duration::from_secs(30);

pub enum BodyDecodeError {
    TooLarge,
    UnsupportedMediaType,
    InvalidRequest,
}

/// A duplicate-aware object representation. It deliberately does not implement
/// `Debug` or `Deserialize` publicly; raw input is validated before DTO creation.
pub struct RawObject(BTreeMap<String, Value>);

impl RawObject {
    pub fn contains(&self, field: &str) -> bool {
        self.0.contains_key(field)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn into_fields(self) -> BTreeMap<String, Value> {
        self.0
    }
}

pub async fn read_json_object(
    headers: &HeaderMap,
    body: Body,
) -> Result<RawObject, BodyDecodeError> {
    read_json_object_with_deadline(headers, body, DEFAULT_BODY_DEADLINE).await
}

pub async fn read_json_object_with_deadline(
    headers: &HeaderMap,
    body: Body,
    deadline: Duration,
) -> Result<RawObject, BodyDecodeError> {
    require_json_content_type(headers)?;
    let bytes = read_bounded(body, MAX_JSON_BODY_BYTES, deadline).await?;
    serde_json::from_slice::<UniqueObject>(&bytes)
        .map(|object| RawObject(object.0))
        .map_err(|_| BodyDecodeError::InvalidRequest)
}

/// Checks actual streamed bytes rather than a Content-Length or size hint. The
/// reader exits immediately on the first nonempty frame, including when a client
/// omitted Content-Length.
pub async fn require_bodyless(body: Body) -> Result<(), BodyDecodeError> {
    require_bodyless_with_deadline(body, DEFAULT_BODY_DEADLINE).await
}

pub async fn require_bodyless_with_deadline(
    mut body: Body,
    deadline: Duration,
) -> Result<(), BodyDecodeError> {
    let expires = tokio::time::Instant::now() + deadline;
    loop {
        let remaining = expires.saturating_duration_since(tokio::time::Instant::now());
        let next = timeout(remaining, body.frame())
            .await
            .map_err(|_| BodyDecodeError::InvalidRequest)?;
        let Some(frame) = next else {
            return Ok(());
        };
        let frame = frame.map_err(|_| BodyDecodeError::InvalidRequest)?;
        if frame.is_data() {
            if frame.into_data().is_ok_and(|data| !data.is_empty()) {
                return Err(BodyDecodeError::InvalidRequest);
            }
        } else {
            // Strictly bodyless methods do not accept trailer frames either.
            return Err(BodyDecodeError::InvalidRequest);
        }
    }
}

async fn read_bounded(
    mut body: Body,
    limit: usize,
    deadline: Duration,
) -> Result<Vec<u8>, BodyDecodeError> {
    let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
    let expires = tokio::time::Instant::now() + deadline;
    loop {
        let remaining = expires.saturating_duration_since(tokio::time::Instant::now());
        let next = timeout(remaining, body.frame())
            .await
            .map_err(|_| BodyDecodeError::InvalidRequest)?;
        let Some(frame) = next else {
            return Ok(bytes);
        };
        let frame = frame.map_err(|_| BodyDecodeError::InvalidRequest)?;
        if let Ok(data) = frame.into_data() {
            if bytes.len().saturating_add(data.len()) > limit {
                return Err(BodyDecodeError::TooLarge);
            }
            bytes.extend_from_slice(&data);
        }
        // Trailers do not count as request-body bytes. HTTP framing errors are
        // represented by the body error above and never expose parser details.
    }
}

fn require_json_content_type(headers: &HeaderMap) -> Result<(), BodyDecodeError> {
    let mut values = headers.get_all(header::CONTENT_TYPE).iter();
    let Some(value) = values.next() else {
        return Err(BodyDecodeError::UnsupportedMediaType);
    };
    if values.next().is_some() {
        return Err(BodyDecodeError::UnsupportedMediaType);
    }
    let value = value
        .to_str()
        .map_err(|_| BodyDecodeError::UnsupportedMediaType)?;
    let mime = mime::Mime::from_str(value).map_err(|_| BodyDecodeError::UnsupportedMediaType)?;
    let subtype = mime.subtype().as_str();
    let json_suffix = mime
        .suffix()
        .is_some_and(|suffix| suffix.as_str().eq_ignore_ascii_case("json"));
    if mime.type_() != mime::APPLICATION || !(subtype.eq_ignore_ascii_case("json") || json_suffix) {
        return Err(BodyDecodeError::UnsupportedMediaType);
    }
    let mut saw_charset = false;
    for (name, value) in mime.params() {
        if name != mime::CHARSET || saw_charset || !value.as_str().eq_ignore_ascii_case("utf-8") {
            return Err(BodyDecodeError::UnsupportedMediaType);
        }
        saw_charset = true;
    }
    Ok(())
}

struct UniqueObject(BTreeMap<String, Value>);

impl<'de> Deserialize<'de> for UniqueObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = UniqueObject;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object with unique member names")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut fields = BTreeMap::new();
                let mut names = BTreeSet::new();
                while let Some((name, value)) = map.next_entry::<String, Value>()? {
                    if !names.insert(name.clone()) {
                        return Err(serde::de::Error::custom("duplicate member"));
                    }
                    fields.insert(name, value);
                }
                Ok(UniqueObject(fields))
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

impl fmt::Debug for BodyDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "BodyDecodeError::TooLarge",
            Self::UnsupportedMediaType => "BodyDecodeError::UnsupportedMediaType",
            Self::InvalidRequest => "BodyDecodeError::InvalidRequest",
        })
    }
}

impl fmt::Display for BodyDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "request body exceeds 65536 bytes",
            Self::UnsupportedMediaType => "request body must use a supported JSON media type",
            Self::InvalidRequest => "request body is invalid",
        })
    }
}

impl std::error::Error for BodyDecodeError {}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, time::Duration};

    use axum::body::Body;
    use futures_util::stream;
    use http::{HeaderMap, HeaderValue, header};

    use super::{BodyDecodeError, MAX_JSON_BODY_BYTES, read_json_object_with_deadline};

    fn json_headers(content_type: &str) -> HeaderMap {
        HeaderMap::from_iter([(
            header::CONTENT_TYPE,
            HeaderValue::from_str(content_type).unwrap(),
        )])
    }

    #[tokio::test]
    async fn bounded_reader_accepts_exact_limit_and_rejects_one_more_byte() {
        // Use valid JSON whose encoded body is precisely at the hard limit.
        let exact = format!("{{\"x\":\"{}\"}}", "a".repeat(MAX_JSON_BODY_BYTES - 8));
        assert_eq!(exact.len(), MAX_JSON_BODY_BYTES);
        let result = read_json_object_with_deadline(
            &json_headers("application/json"),
            Body::from(exact),
            Duration::from_secs(1),
        )
        .await;
        assert!(result.is_ok());

        let over = format!("{{\"x\":\"{}\"}}", "a".repeat(MAX_JSON_BODY_BYTES - 7));
        assert_eq!(over.len(), MAX_JSON_BODY_BYTES + 1);
        let result = read_json_object_with_deadline(
            &json_headers("application/json"),
            Body::from(over),
            Duration::from_secs(1),
        )
        .await;
        assert!(matches!(result, Err(BodyDecodeError::TooLarge)));
    }

    #[tokio::test]
    async fn streamed_frames_and_stream_errors_are_bounded_and_sanitized() {
        let frames = stream::iter([
            Ok::<_, Infallible>("{\"x\":".as_bytes().to_vec()),
            Ok("1}".as_bytes().to_vec()),
        ]);
        let result = read_json_object_with_deadline(
            &json_headers("application/json"),
            Body::from_stream(frames),
            Duration::from_secs(1),
        )
        .await;
        assert!(result.is_ok());

        let failed = stream::iter([
            Ok::<_, std::io::Error>("{\"canary\":".as_bytes().to_vec()),
            Err(std::io::Error::other("CANARY BODY STREAM ERROR")),
        ]);
        let error = read_json_object_with_deadline(
            &json_headers("application/json"),
            Body::from_stream(failed),
            Duration::from_secs(1),
        )
        .await
        .err()
        .expect("stream errors become safe invalid-request errors");
        assert!(matches!(error, BodyDecodeError::InvalidRequest));
        assert!(!error.to_string().contains("CANARY"));
    }

    #[tokio::test]
    async fn media_type_uses_parsing_not_substring_matching() {
        for invalid in [
            "text/application/json",
            "application/jsonp",
            "application/json; charset=utf-16",
            "application/json; profile=other",
            "application/json; charset=utf-8; charset=utf-16",
            "application/json garbage",
        ] {
            let result = read_json_object_with_deadline(
                &json_headers(invalid),
                Body::from("{}"),
                Duration::from_secs(1),
            )
            .await;
            assert!(matches!(result, Err(BodyDecodeError::UnsupportedMediaType)));
        }
        assert!(
            read_json_object_with_deadline(
                &json_headers("application/problem+json; charset=UTF-8"),
                Body::from("{}"),
                Duration::from_secs(1),
            )
            .await
            .is_ok()
        );

        let mut duplicate = json_headers("application/json");
        duplicate.append(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        assert!(matches!(
            read_json_object_with_deadline(&duplicate, Body::from("{}"), Duration::from_secs(1),)
                .await,
            Err(BodyDecodeError::UnsupportedMediaType)
        ));

        let oversized_wrong_type = read_json_object_with_deadline(
            &json_headers("text/plain"),
            Body::from(vec![b'x'; MAX_JSON_BODY_BYTES + 1]),
            Duration::from_secs(1),
        )
        .await;
        assert!(matches!(
            oversized_wrong_type,
            Err(BodyDecodeError::UnsupportedMediaType)
        ));
    }

    #[tokio::test]
    async fn duplicate_fields_and_non_object_roots_are_invalid() {
        for body in ["{\"x\":1,\"x\":2}", "[]", "null", "{\"x\":1} {}"] {
            let result = read_json_object_with_deadline(
                &json_headers("application/json"),
                Body::from(body),
                Duration::from_secs(1),
            )
            .await;
            assert!(matches!(result, Err(BodyDecodeError::InvalidRequest)));
        }
    }

    #[tokio::test]
    async fn bodyless_reader_checks_streamed_bytes_and_uses_a_total_deadline() {
        let empty = stream::empty::<Result<Vec<u8>, std::io::Error>>();
        assert!(super::require_bodyless_with_deadline(
            Body::from_stream(empty),
            Duration::from_secs(1),
        )
        .await
        .is_ok());

        let actual_bytes = stream::iter([Ok::<_, std::io::Error>(Vec::new()), Ok(b"{}".to_vec())]);
        assert!(matches!(
            super::require_bodyless_with_deadline(
                Body::from_stream(actual_bytes),
                Duration::from_secs(1),
            )
            .await,
            Err(BodyDecodeError::InvalidRequest)
        ));

        let slow = futures_util::stream::pending::<Result<Vec<u8>, std::io::Error>>();
        assert!(matches!(
            super::require_bodyless_with_deadline(
                Body::from_stream(slow),
                Duration::from_millis(10),
            )
            .await,
            Err(BodyDecodeError::InvalidRequest)
        ));
    }
}
