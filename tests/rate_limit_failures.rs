#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/commit_proxy.rs"]
mod commit_proxy;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/openapi.rs"]
#[allow(dead_code)]
mod openapi;
#[path = "support/rate_limits.rs"]
#[allow(dead_code)]
mod rate_support;

use account_support::{migrated_database, seed_account, seed_anonymous_link};
use axum::{body::Body, response::Response};
use ferl_server::{domain::secret::AccountApiKey, rate_limit::CreationFailure};
use http_body_util::BodyExt;
use rate_support::{close, count, parts, prepare, state, wait_for_query};
use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{task::JoinSet, time::timeout};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn check(response: Response, status: u16, code: &str, component: &str, head: bool) {
    assert_eq!(response.status().as_u16(), status);
    let headers = response.headers().clone();
    let id = headers["x-request-id"].to_str().unwrap();
    uuid::Uuid::parse_str(id).unwrap();
    assert_ne!(id, "untrusted-request-id");
    let bytes = timeout(Duration::from_secs(5), response.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    if head {
        assert!(bytes.is_empty());
        assert!(openapi::validates_response(component, &headers, None, true));
    } else {
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["request_id"], id);
        assert_eq!(body["error"]["code"], code);
        assert_eq!(headers["content-type"], "application/json");
        assert!(openapi::validates_response(
            component,
            &headers,
            Some(&body),
            false
        ));
        let mut malformed = body.clone();
        malformed["error"]["code"] = serde_json::json!("unexpected");
        assert!(!openapi::validates_response(
            component,
            &headers,
            Some(&malformed),
            false
        ));
        if status == 429 {
            let mut invalid = headers.clone();
            invalid.remove("retry-after");
            assert!(!openapi::validates_response(
                component,
                &invalid,
                Some(&body),
                false
            ));
            for value in ["0", "-1", "1.5", "tomorrow", "+1"] {
                invalid.insert("retry-after", value.parse().unwrap());
                assert!(!openapi::validates_response(
                    component,
                    &invalid,
                    Some(&body),
                    false
                ));
            }
        }
    }
}

#[tokio::test]
async fn real_failures_commit_uncertainty_http_contract_and_production_diagnostics_are_private() {
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = capture.clone();
    ferl_server::telemetry::logging::init_with_writer(move || writer.clone());
    let db = migrated_database().await;
    db.verify(|db| Box::pin(async move {
    let state = state(
        db.schema_database_url(),
        &[
            ("FERL_TRUSTED_PROXIES", "198.51.100.244"),
            ("FERL_ANONYMOUS_CREATE_LIMIT", "1"),
            ("FERL_DB_LOCK_TIMEOUT_SECS", "1"),
        ],
    );
    let (disabled_id, disabled) =
        seed_account(db.pool(), "privacy_disabled_canary", "user", false).await;
    let (revoked_id, revoked) =
        seed_account(db.pool(), "privacy_revoked_canary", "user", true).await;
    let replacement = AccountApiKey::generate().unwrap();
    sqlx::query("UPDATE accounts SET api_key_digest = $2 WHERE id = $1")
        .bind(revoked_id.as_uuid())
        .bind(replacement.digest().as_bytes().as_slice())
        .execute(db.pool())
        .await
        .unwrap();
    let (_, token) = seed_anonymous_link(db.pool(), "privacy-token-canary").await;
    let unmatched = AccountApiKey::generate().unwrap();
    let mut canaries = vec![
        db.schema_database_url().to_owned(),
        disabled.clone(),
        revoked.clone(),
        token.clone(),
        unmatched.as_str().to_owned(),
        replacement.as_str().to_owned(),
        "198.51.100.244".into(),
        "203.0.113.244".into(),
        "FORWARD_CANARY".into(),
        "BODY_CANARY".into(),
        "privacy_disabled_canary".into(),
        "SQL_DEPENDENCY_CANARY".into(),
        disabled_id.as_uuid().to_string(),
    ];
    for key in [&disabled, &revoked, &token] {
        let digest = ferl_server::domain::secret::SecretDigest::from_presented_secret(key);
        canaries.push(
            digest
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        );
    }
    tracing::info!(target: "sqlx::query", sql = "SELECT SQL_DEPENDENCY_CANARY", parameters = "BODY_CANARY", "dependency diagnostic must be suppressed by production composition");
    let app = rate_support::harness(state.clone());
    for head in [false, true] {
        for (key, status, code, component) in [
            (
                "malformed_SECRET_CANARY",
                401,
                "invalid_credentials",
                "Unauthorized",
            ),
            (
                unmatched.as_str(),
                401,
                "invalid_credentials",
                "Unauthorized",
            ),
            (
                disabled.as_str(),
                401,
                "invalid_credentials",
                "Unauthorized",
            ),
            (revoked.as_str(), 401, "invalid_credentials", "Unauthorized"),
            (token.as_str(), 403, "forbidden", "Forbidden"),
        ] {
            let mut request =
                rate_support::request("198.51.100.244", Some(key), Body::from("BODY_CANARY"));
            if head {
                *request.method_mut() = http::Method::HEAD;
            }
            check(
                account_support::call(app.clone(), request).await,
                status,
                code,
                component,
                head,
            )
            .await;
        }
    }
    canaries.push("malformed_SECRET_CANARY".into());
    assert_eq!(count(db.pool()).await, 0);
    let off = rate_support::state(
        db.schema_database_url(),
        &[("FERL_ANONYMOUS_CREATION", "false")],
    );
    let mut no_peer = parts("198.51.100.244", None);
    no_peer.extensions.clear();
    assert_eq!(
        prepare(&state, &no_peer).await.unwrap_err(),
        CreationFailure::Internal
    );
    for head in [false, true] {
        let mut request = rate_support::request("198.51.100.244", None, Body::from("BODY_CANARY"));
        if head {
            *request.method_mut() = http::Method::HEAD;
        }
        check(
            account_support::call(rate_support::harness(off.clone()), request).await,
            403,
            "anonymous_creation_disabled",
            "Forbidden",
            head,
        )
        .await;
    }
    close(&off.pool).await;
    // The entire malformed forwarded value traverses the actual resolver.
    let mut attempt = parts("198.51.100.244", None);
    attempt.headers.insert(
        "x-forwarded-for",
        "203.0.113.244, FORWARD_CANARY".parse().unwrap(),
    );
    prepare(&state, &attempt).await.unwrap();
    for head in [false, true] {
        let mut request = rate_support::request("198.51.100.244", None, Body::from("BODY_CANARY"));
        if head {
            *request.method_mut() = http::Method::HEAD;
        }
        check(
            account_support::call(app.clone(), request).await,
            429,
            "rate_limited",
            "RateLimited",
            head,
        )
        .await;
    }
    // Actual stored-value corruption remains 500, not fabricated bad credentials.
    sqlx::query("ALTER TABLE accounts DROP CONSTRAINT accounts_role_check")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE accounts SET enabled = true, role = 'ROLE_CANARY' WHERE id = $1")
        .bind(disabled_id.as_uuid())
        .execute(db.pool())
        .await
        .unwrap();
    canaries.push("ROLE_CANARY".into());
    for head in [false, true] {
        let mut request = rate_support::request("198.51.100.244", Some(&disabled), Body::empty());
        if head {
            *request.method_mut() = http::Method::HEAD;
        }
        check(
            account_support::call(app.clone(), request).await,
            500,
            "internal_error",
            "InternalError",
            head,
        )
        .await;
    }
    // Contention timeout and explicit cancellation use the production pool.
    sqlx::query("INSERT INTO rate_buckets (id, bucket_kind, client_ip) VALUES ($1, 'anonymous_ip', '192.0.2.244')").bind(uuid::Uuid::new_v4()).execute(db.pool()).await.unwrap();
    let mut holder = db.pool().begin().await.unwrap();
    sqlx::query("SELECT id FROM rate_buckets WHERE client_ip = '192.0.2.244' FOR UPDATE")
        .execute(&mut *holder)
        .await
        .unwrap();
    assert_eq!(
        prepare(&state, &parts("192.0.2.244", None))
            .await
            .unwrap_err(),
        CreationFailure::StorageUnavailable
    );
    let service = state.creation.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { service.prepare(&parts("192.0.2.244", None)).await });
    wait_for_query(db.pool(), "SELECT id FROM rate_buckets%FOR UPDATE", 1).await;
    tasks.abort_all();
    assert!(
        timeout(Duration::from_secs(3), tasks.join_next())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    holder.rollback().await.unwrap();
    canaries.push("192.0.2.244".into());
    // A failing bounded cleanup cannot silently grant admission.
    sqlx::query("INSERT INTO rate_charges (id, bucket_id, charged_at) SELECT $1, id, clock_timestamp() - interval '120 seconds' FROM rate_buckets WHERE client_ip = '192.0.2.244'").bind(uuid::Uuid::new_v4()).execute(db.pool()).await.unwrap();
    sqlx::query("CREATE FUNCTION rate_cleanup_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'CLEANUP_SQL_CANARY' USING ERRCODE = '57014'; END $$").execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TRIGGER rate_cleanup_fault BEFORE DELETE ON rate_charges FOR EACH ROW EXECUTE FUNCTION rate_cleanup_fault()").execute(db.pool()).await.unwrap();
    assert_eq!(
        prepare(&state, &parts("192.0.2.244", None))
            .await
            .unwrap_err(),
        CreationFailure::StorageUnavailable
    );
    assert_eq!(count(db.pool()).await, 2);
    sqlx::query("DROP TRIGGER rate_cleanup_fault ON rate_charges")
        .execute(db.pool())
        .await
        .unwrap();
    canaries.push("CLEANUP_SQL_CANARY".into());
    // Actual connection establishment failure through production configuration.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut unavailable_url = url::Url::parse(db.schema_database_url()).unwrap();
    unavailable_url.set_port(Some(port)).unwrap();
    let unavailable = rate_support::state(
        unavailable_url.as_str(),
        &[("FERL_DB_ACQUIRE_TIMEOUT_SECS", "1")],
    );
    canaries.push(unavailable_url.to_string());
    for head in [false, true] {
        let mut request = rate_support::request("198.51.100.244", None, Body::empty());
        if head {
            *request.method_mut() = http::Method::HEAD;
        }
        check(
            account_support::call(rate_support::harness(unavailable.clone()), request).await,
            503,
            "storage_unavailable",
            "Unavailable",
            head,
        )
        .await;
    }
    assert_eq!(
        prepare(
            &unavailable,
            &parts("198.51.100.244", Some(unmatched.as_str()))
        )
        .await
        .unwrap_err(),
        CreationFailure::StorageUnavailable
    );
    close(&unavailable.pool).await;
    aborted_commit(db).await;
    // True lost acknowledgement: PostgreSQL says COMMIT completed; independent
    // connection proves durability BEFORE the proxy drops the response.
    let before = count(db.pool()).await;
    let mut proxy = commit_proxy::CommitProxy::start(db.schema_database_url()).await;
    let proxied = rate_support::state(&proxy.url, &[("FERL_DB_POOL_MAX", "1")]);
    canaries.push(proxy.url.clone());
    let service = proxied.creation.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { service.prepare(&parts("192.0.2.245", None)).await });
    proxy.committed().await;
    assert_eq!(
        count(db.pool()).await,
        before + 1,
        "commit is durable before acknowledgement is withheld"
    );
    assert!(!tasks.is_empty());
    proxy.disconnect().await;
    assert_eq!(
        timeout(Duration::from_secs(5), tasks.join_next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap_err(),
        CreationFailure::CommitOutcomeUnknown
    );
    close(&proxied.pool).await;
    assert_eq!(
        count(db.pool()).await,
        before + 1,
        "no successful capability and no automatic second charge"
    );
    canaries.push("192.0.2.245".into());
    drop(app);
    close(&state.pool).await;
    let diagnostics = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let observed: Vec<_> = diagnostics
        .lines()
        .map(|line| {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            (
                event["fields"]["operation"].clone(),
                event["fields"]["outcome"].clone(),
            )
        })
        .collect();
    for expected in [
        "invalid_credentials",
        "anonymous_creation_disabled",
        "forbidden",
        "fallback",
        "charged",
        "rate_limited",
        "cancelled",
        "rate_storage_unavailable",
        "creation_internal_failure",
        "rate_cleanup_failed",
        "rate_commit_outcome_unknown",
        "request completed",
    ] {
        assert!(
            diagnostics.contains(expected),
            "intended diagnostic absent: {expected}; observed {observed:?}"
        );
    }
    for canary in canaries {
        assert!(
            !diagnostics.contains(&canary),
            "sensitive canary leaked in production diagnostics"
        );
    }
    for line in diagnostics.lines() {
        serde_json::from_str::<serde_json::Value>(line).expect("production logs are JSON");
    }
    })).await;
}

async fn aborted_commit(db: &database_support::IsolatedTestDatabase) {
    let state = state(db.schema_database_url(), &[]);
    sqlx::query("CREATE FUNCTION rate_commit_hold() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(715059); RETURN NEW; END $$").execute(db.pool()).await.unwrap();
    sqlx::query("CREATE CONSTRAINT TRIGGER rate_commit_hold AFTER INSERT ON rate_charges DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION rate_commit_hold()").execute(db.pool()).await.unwrap();
    let mut holder = db.pool().begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(715059)")
        .execute(&mut *holder)
        .await
        .unwrap();
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    let before = count(db.pool()).await;
    let service = state.creation.clone();
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { service.prepare(&parts("192.0.2.246", None)).await });
    wait_for_query(db.pool(), "COMMIT%", 1).await;
    let pid: i32 = sqlx::query_scalar("SELECT pid FROM pg_stat_activity WHERE query LIKE 'COMMIT%' AND $1 = ANY(pg_blocking_pids(pid))").bind(holder_pid).fetch_one(db.pool()).await.unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT pg_terminate_backend($1)")
            .bind(pid)
            .fetch_one(db.pool())
            .await
            .unwrap()
    );
    assert_eq!(
        timeout(Duration::from_secs(5), tasks.join_next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap_err(),
        CreationFailure::CommitOutcomeUnknown
    );
    holder.rollback().await.unwrap();
    assert_eq!(
        count(db.pool()).await,
        before,
        "backend termination aborted this COMMIT; it is NOT proof of durable lost acknowledgement"
    );
    sqlx::query("DROP TRIGGER rate_commit_hold ON rate_charges")
        .execute(db.pool())
        .await
        .unwrap();
    close(&state.pool).await;
}
