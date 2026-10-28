use std::{collections::BTreeMap, net::SocketAddr, time::Duration};

use axum::{body::Body, extract::ConnectInfo, http::Request, response::Response};
use ferl_server::{app::AppState, config::Config};
use http::Method;
use sqlx::PgPool;
use tokio::time::{sleep, timeout};

pub fn state(database_url: &str, extra: &[(&str, &str)]) -> AppState {
    let mut values = BTreeMap::from([
        ("FERL_DATABASE_URL".to_owned(), database_url.to_owned()),
        (
            "FERL_PUBLIC_BASE_URL".to_owned(),
            "https://redirects.example.test".to_owned(),
        ),
        (
            "FERL_CURSOR_KEY".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ),
    ]);
    for (name, value) in extra {
        values.insert((*name).to_owned(), (*value).to_owned());
    }
    let config = Config::from_map(values).expect("redirect production config is valid");
    AppState::from_config(&config).expect("redirect production state composes")
}

pub fn request(method: Method, uri: &str, peer: SocketAddr) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "client-value")
        .body(Body::empty())
        .expect("redirect request is syntactically valid");
    request.extensions_mut().insert(ConnectInfo(peer));
    request
}

pub async fn call(app: axum::Router, request: Request<Body>) -> Response {
    timeout(
        Duration::from_secs(5),
        tower::ServiceExt::oneshot(app, request),
    )
    .await
    .expect("public router responds before its deadline")
    .expect("public router service is infallible")
}

pub async fn wait_for_visits(pool: &PgPool, code: &str, expected: i64) {
    timeout(Duration::from_secs(8), async {
        loop {
            let count = sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM visits v JOIN links l ON l.id = v.link_id \
                 WHERE l.short_code = $1",
            )
            .bind(code)
            .fetch_one(pool)
            .await
            .expect("fixture PostgreSQL remains queryable");
            if count == expected {
                return;
            }
            assert!(count < expected, "worker did not insert duplicate events");
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("analytics persistence becomes visible before its deadline");
}
