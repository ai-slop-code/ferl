use std::{
    fmt::Write as _,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::Body,
    http::{HeaderValue, Method, Request, Response, StatusCode},
};
use ferl_server::{
    domain::{
        ids::AccountId,
        secret::{CredentialClass, CredentialHeader, OneTimeSecret},
        time::Timestamp,
    },
    http::test_support::decoder_router,
    pagination::{CursorBoundary, CursorCodec, CursorContext},
};
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::time::timeout;
use tower::ServiceExt;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context, prelude::*};
use uuid::Uuid;

const HTTP_DEADLINE: Duration = Duration::from_secs(3);

async fn call(app: axum::Router, request: Request<Body>) -> Response<Body> {
    timeout(HTTP_DEADLINE, app.oneshot(request))
        .await
        .expect("decoder route responds before its deadline")
        .expect("test router is infallible")
}

fn request(method: Method, uri: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(body)
        .expect("test request is structurally valid")
}

async fn body_json(response: Response<Body>) -> Value {
    let bytes = timeout(HTTP_DEADLINE, response.into_body().collect())
        .await
        .expect("response body completes before its deadline")
        .expect("response body is readable")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("error body is JSON")
}

fn expect_envelope(response: &Response<Body>, status: StatusCode) {
    assert_eq!(response.status(), status);
    assert_ne!(
        response.headers()["x-request-id"],
        "client-controlled-value"
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn body_limit_is_enforced_on_actual_streamed_bytes_at_both_boundaries() {
    let app = decoder_router();
    let stream_body = |bytes: Vec<u8>| {
        let chunks = bytes
            .chunks(32 * 1024)
            .map(|chunk| Ok::<_, std::io::Error>(chunk.to_vec()))
            .collect::<Vec<_>>();
        Body::from_stream(futures_util::stream::iter(chunks))
    };
    let prefix = r#"{"destination_url":"https://example.test/"}"#;
    let exact = format!("{prefix}{}", " ".repeat(65_536 - prefix.len()));
    assert_eq!(exact.len(), 65_536);
    let mut req = request(
        Method::POST,
        "/decode/create",
        stream_body(exact.into_bytes()),
    );
    req.headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    req.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_static("client-controlled-value"),
    );
    let response = call(app.clone(), req).await;
    expect_envelope(&response, StatusCode::NO_CONTENT);

    let over = format!("{prefix}{}", " ".repeat(65_537 - prefix.len()));
    assert_eq!(over.len(), 65_537);
    let mut req = request(
        Method::POST,
        "/decode/create",
        stream_body(over.into_bytes()),
    );
    req.headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    let response = call(app, req).await;
    expect_envelope(&response, StatusCode::PAYLOAD_TOO_LARGE);
    let body = body_json(response).await;
    assert_eq!(body["error"]["code"], "request_too_large");
}

#[tokio::test]
async fn content_type_json_errors_duplicates_and_unknown_fields_use_contract_statuses() {
    let app = decoder_router();
    for (content_type, expected_status, expected_code) in [
        (
            None,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
        ),
        (
            Some("text/application/json"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
        ),
        (
            Some("application/json; charset=utf-16"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
        ),
        (
            Some("application/problem+json; charset=UTF-8"),
            StatusCode::NO_CONTENT,
            "",
        ),
    ] {
        let mut req = request(
            Method::POST,
            "/decode/create",
            Body::from(r#"{"destination_url":"https://example.test/"}"#),
        );
        if let Some(content_type) = content_type {
            req.headers_mut()
                .insert("content-type", HeaderValue::from_str(content_type).unwrap());
        }
        let response = call(app.clone(), req).await;
        expect_envelope(&response, expected_status);
        if expected_code.is_empty() {
            assert!(
                timeout(HTTP_DEADLINE, response.into_body().collect())
                    .await
                    .unwrap()
                    .unwrap()
                    .to_bytes()
                    .is_empty()
            );
        } else {
            assert_eq!(body_json(response).await["error"]["code"], expected_code);
        }
    }

    for raw in [
        r#"{"destination_url":"https://example.test/","destination_url":"https://other.test/"}"#,
        r#"{"destination_url":"https://example.test/","owner_id":null}"#,
        r#"[]"#,
        r#"{"destination_url":"https://example.test/"} trailing"#,
    ] {
        let mut req = request(Method::POST, "/decode/create", Body::from(raw));
        req.headers_mut()
            .insert("content-type", HeaderValue::from_static("application/json"));
        let response = call(app.clone(), req).await;
        expect_envelope(&response, StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "invalid_request"
        );
    }

    let mut req = request(
        Method::POST,
        "/decode/patch",
        Body::from(r#"{"title":null,"tags":[]}"#),
    );
    req.headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    expect_envelope(&call(app, req).await, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn anonymous_feature_permission_uses_raw_presence_and_bodylessness_uses_received_bytes() {
    let app = decoder_router();
    for body in [
        r#"{"destination_url":"invalid","short_code":null}"#,
        r#"{"short_code":false,"owner_id":"bad"}"#,
    ] {
        let mut req = request(
            Method::POST,
            "/decode/create?creator=anonymous",
            Body::from(body),
        );
        req.headers_mut()
            .insert("content-type", HeaderValue::from_static("application/json"));
        let response = call(app.clone(), req).await;
        expect_envelope(&response, StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["error"]["code"], "forbidden");
    }

    let empty = call(
        app.clone(),
        request(Method::POST, "/decode/bodyless", Body::empty()),
    )
    .await;
    expect_envelope(&empty, StatusCode::NO_CONTENT);

    for body in ["{}", " \n\t"] {
        let mut req = request(Method::DELETE, "/decode/bodyless", Body::from(body));
        // A misleading zero length must not make actual body bytes disappear.
        req.headers_mut()
            .insert("content-length", HeaderValue::from_static("0"));
        let response = call(app.clone(), req).await;
        expect_envelope(&response, StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "invalid_request"
        );
    }

    let stream =
        futures_util::stream::iter([Ok::<_, std::io::Error>(b"{".to_vec()), Ok(b"}".to_vec())]);
    let response = call(
        app,
        request(Method::POST, "/decode/bodyless", Body::from_stream(stream)),
    )
    .await;
    expect_envelope(&response, StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await["error"]["code"],
        "invalid_request"
    );
}

#[tokio::test]
async fn query_decoders_are_operation_scoped_and_errors_remain_sanitized() {
    let app = decoder_router();
    for uri in [
        "/decode/links?tag=Work&tag=rust&tag_mode=all&limit=200",
        "/decode/accounts?limit=50&cursor=opaque",
        "/decode/tags?scope=own&q=Rust",
        "/decode/visits?from=2026-01-01T00%3A00%3A00Z&to=2026-01-02T00%3A00%3A00Z",
        "/decode/stats?from=2026-01-01T00%3A00%3A00Z",
        "/decode/breakdown?group_by=user_agent&limit=2",
    ] {
        let response = call(app.clone(), request(Method::GET, uri, Body::empty())).await;
        expect_envelope(&response, StatusCode::NO_CONTENT);
    }
    for uri in [
        "/decode/links?enabled=true&enabled=false",
        "/decode/links?unknown=1",
        "/decode/links?tag_mode=all",
        "/decode/accounts?limit=201",
        "/decode/stats?limit=1",
        "/decode/breakdown?group_by=day&group_by=os",
        "/decode/links?q=%FF",
    ] {
        let response = call(app.clone(), request(Method::GET, uri, Body::empty())).await;
        expect_envelope(&response, StatusCode::BAD_REQUEST);
        let error = body_json(response).await;
        assert_eq!(error["error"]["code"], "invalid_request");
        assert!(!error.to_string().contains("CANARY"));
    }
}

#[derive(Clone)]
struct LogCapture(Arc<Mutex<String>>);

struct EventFields(String);

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let _ = write!(&mut self.0, "{}={value:?};", field.name());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        let _ = write!(&mut self.0, "{}={value};", field.name());
    }
}

impl<S: Subscriber> Layer<S> for LogCapture {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut fields = EventFields(String::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push_str(&fields.0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn failing_real_parser_and_crypto_paths_do_not_log_canary_values() {
    let output = Arc::new(Mutex::new(String::new()));
    let subscriber = tracing_subscriber::registry().with(LogCapture(Arc::clone(&output)));
    tracing::subscriber::set_global_default(subscriber)
        .expect("the integration-test process has no prior global subscriber");

    let secret = OneTimeSecret::generate(CredentialClass::AccountKey).unwrap();
    let app = decoder_router();
    for (uri, body) in [
        (
            "/decode/create",
            r#"{"destination_url":"http://@CANARY_DESTINATION.invalid/?q=CANARY_QUERY","title":"CANARY_TITLE","tags":["CANARY_TAG"]}"#,
        ),
        (
            "/decode/create",
            r#"{"destination_url":"https://example.test/","title":"CANARY_TITLE","tags":["CANARY_TAG",""]}"#,
        ),
    ] {
        let mut req = request(Method::POST, uri, Body::from(body));
        req.headers_mut()
            .insert("content-type", HeaderValue::from_static("application/json"));
        req.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", secret.as_str())).unwrap(),
        );
        req.headers_mut().insert(
            "referer",
            HeaderValue::from_static("https://canary-referrer.invalid/path?secret=CANARY_HEADER"),
        );
        req.headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("198.51.100.77"));
        let response = call(app.clone(), req).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error = body_json(response).await;
        for canary in [
            "CANARY_DESTINATION",
            "CANARY_QUERY",
            "CANARY_TITLE",
            "CANARY_TAG",
        ] {
            assert!(!error.to_string().contains(canary));
        }
    }
    let failed_stream = futures_util::stream::iter([
        Ok::<_, std::io::Error>(b"{".to_vec()),
        Err(std::io::Error::other("CANARY_BODY_DEPENDENCY_FAILURE")),
    ]);
    let mut stream_request = request(
        Method::POST,
        "/decode/create",
        Body::from_stream(failed_stream),
    );
    stream_request
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    let stream_response = call(decoder_router(), stream_request).await;
    assert_eq!(stream_response.status(), StatusCode::BAD_REQUEST);
    assert!(
        !body_json(stream_response)
            .await
            .to_string()
            .contains("CANARY_BODY_DEPENDENCY_FAILURE")
    );
    let response = call(
        app,
        request(
            Method::GET,
            "/decode/links?q=CANARY_QUERY&enabled=TRUE",
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        !body_json(response)
            .await
            .to_string()
            .contains("CANARY_QUERY")
    );

    let mut credential_headers = http::HeaderMap::new();
    credential_headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", secret.as_str())).unwrap(),
    );
    let parsed = CredentialHeader::parse(&credential_headers);
    let ferl_server::domain::secret::CredentialHeader::Parsed(parsed) = parsed else {
        panic!("generated credential has supported syntactic format");
    };
    let digest_canary = parsed
        .digest()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    credential_headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer ferl_ak_v1_CANARY_CREDENTIAL"),
    );
    assert!(matches!(
        CredentialHeader::parse(&credential_headers),
        CredentialHeader::SuppliedInvalid
    ));

    let query = ferl_server::http::query::AccountListQuery::parse(None).unwrap();
    let context = CursorContext::accounts(AccountId::new(Uuid::from_u128(77)), &query);
    let codec = CursorCodec::new([0x33; 32]);
    assert!(codec.decode(&context, "CANARY_CURSOR_PAYLOAD").is_err());
    let timestamp = Timestamp::parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
    let valid = codec
        .encode(
            &context,
            &CursorBoundary::AccountCreatedAt {
                at: timestamp,
                id: AccountId::new(Uuid::from_u128(2)),
            },
        )
        .unwrap();
    assert!(!valid.is_empty());

    let logs = output.lock().unwrap().clone();
    for canary in [
        "CANARY_DESTINATION",
        "CANARY_QUERY",
        "CANARY_TITLE",
        "CANARY_TAG",
        "CANARY_CREDENTIAL",
        "CANARY_HEADER",
        "198.51.100.77",
        "CANARY_CURSOR_PAYLOAD",
        "CANARY_BODY_DEPENDENCY_FAILURE",
        &digest_canary,
        secret.as_str(),
    ] {
        assert!(
            !logs.contains(canary),
            "application logs must exclude {canary}"
        );
    }
    assert!(
        logs.contains("request completed"),
        "captured diagnostic output: {logs}"
    );
}
