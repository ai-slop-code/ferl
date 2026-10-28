#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/rate_limits.rs"]
#[allow(dead_code)]
mod rate_support;

use account_support::{migrated_database, seed_account, seed_anonymous_link};
use axum::body::Body;
use ferl_server::{
    auth::{self, GuardFailure},
    rate_limit::{CreationFailure, RateSubject},
};
use rate_support::{close, count, parts, prepare, state};

#[tokio::test]
async fn reduced_startup_limits_and_independent_statement_bounds_are_composed() {
    let db = migrated_database().await;
    db.verify(|db| Box::pin(async move {
        sqlx::query("CREATE TABLE rate_setting_evidence (statement_timeout text, lock_timeout text)").execute(db.pool()).await.unwrap();
        sqlx::query("CREATE FUNCTION rate_settings() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO rate_setting_evidence VALUES (current_setting('statement_timeout'), current_setting('lock_timeout')); RETURN NEW; END $$").execute(db.pool()).await.unwrap();
        sqlx::query("CREATE TRIGGER rate_settings BEFORE INSERT ON rate_charges FOR EACH ROW EXECUTE FUNCTION rate_settings()").execute(db.pool()).await.unwrap();
        let (_, key) = seed_account(db.pool(), "reduced_limit", "user", true).await;
        let original = state(db.schema_database_url(), &[("FERL_ACCOUNT_CREATE_LIMIT", "7"), ("FERL_DB_LOCK_TIMEOUT_SECS", "1"), ("FERL_DB_STATEMENT_TIMEOUT_SECS", "3")]);
        for _ in 0..7 { prepare(&original, &parts("192.0.2.30", Some(&key))).await.unwrap(); }
        let settings: Vec<(String, String)> = sqlx::query_as("SELECT statement_timeout, lock_timeout FROM rate_setting_evidence").fetch_all(db.pool()).await.unwrap();
        assert_eq!(settings, vec![("3s".into(), "1s".into()); 7], "the shared authority helper's 1s setting was restored to the independent 3s SQL bound");
        let before: Vec<(uuid::Uuid, String)> = sqlx::query_as("SELECT id, charged_at::text FROM rate_charges ORDER BY id").fetch_all(db.pool()).await.unwrap();
        close(&original.pool).await;
        let reduced = state(db.schema_database_url(), &[("FERL_ACCOUNT_CREATE_LIMIT", "3")]);
        assert!(matches!(prepare(&reduced, &parts("192.0.2.30", Some(&key))).await, Err(CreationFailure::RateLimited(_))));
        let after: Vec<(uuid::Uuid, String)> = sqlx::query_as("SELECT id, charged_at::text FROM rate_charges ORDER BY id").fetch_all(db.pool()).await.unwrap();
        assert_eq!(before, after);
        close(&reduced.pool).await;
    })).await;
}

#[tokio::test]
async fn defaults_independent_subjects_and_single_connection_composition() {
    let db = migrated_database().await;
    db.verify(|db| Box::pin(async move {
    let state = state(db.schema_database_url(), &[("FERL_DB_POOL_MAX", "1")]);
    state.verify_schema().await.unwrap();
    let (user, key) = seed_account(db.pool(), "rate_user", "user", true).await;
    let (admin, admin_key) = seed_account(db.pool(), "rate_admin", "admin", true).await;
    for (ip, key, limit, expected) in [
        ("192.0.2.9", None, 5, None),
        ("192.0.2.10", Some(key.as_str()), 60, Some(user)),
        ("192.0.2.10", Some(admin_key.as_str()), 60, Some(admin)),
    ] {
        for _ in 0..limit {
            let charge = prepare(&state, &parts(ip, key)).await.unwrap();
            if let Some(id) = expected {
                assert_eq!(charge.creator().subject(), RateSubject::Account(id));
            }
        }
        let error = prepare(&state, &parts(ip, key)).await.unwrap_err();
        assert!(
            matches!(error, CreationFailure::RateLimited(retry) if (1..=60).contains(&retry.seconds()))
        );
    }
    assert!(matches!(
        prepare(&state, &parts("::ffff:192.0.2.9", None)).await,
        Err(CreationFailure::RateLimited(_))
    ));
    prepare(&state, &parts("192.0.2.10", None)).await.unwrap();
    assert_eq!(count(db.pool()).await, 126);
    let buckets: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_buckets")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(buckets, 4);
    close(&state.pool).await;
    })).await;
}

#[tokio::test]
async fn eligibility_and_body_order_preserve_independently_committed_charges() {
    let db = migrated_database().await;
    db.verify(|db| Box::pin(async move {
    let state = state(db.schema_database_url(), &[]);
    let (_id, key) = seed_account(db.pool(), "eligible_user", "user", true).await;
    let (_disabled, disabled) = seed_account(db.pool(), "disabled_user", "user", false).await;
    let (_, token) = seed_anonymous_link(db.pool(), "rate-token").await;
    let unmatched = ferl_server::domain::secret::AccountApiKey::generate().unwrap();
    let disabled_state = rate_support::state(
        db.schema_database_url(),
        &[("FERL_ANONYMOUS_CREATION", "false")],
    );
    assert_eq!(
        prepare(&disabled_state, &parts("192.0.2.9", None))
            .await
            .unwrap_err(),
        CreationFailure::AnonymousCreationDisabled
    );
    let app = rate_support::harness(state.clone());
    for (credential, expected) in [
        ("bad", 401),
        (unmatched.as_str(), 401),
        (disabled.as_str(), 401),
        (token.as_str(), 403),
    ] {
        let stream = futures_util::stream::poll_fn(
            |_| -> std::task::Poll<Option<Result<Vec<u8>, std::convert::Infallible>>> {
                panic!("ineligible request read body frames")
            },
        );
        let response = account_support::call(
            app.clone(),
            rate_support::request("192.0.2.9", Some(credential), Body::from_stream(stream)),
        )
        .await;
        assert_eq!(response.status().as_u16(), expected);
    }
    assert_eq!(count(db.pool()).await, 0);
    let response = account_support::call(
        app,
        rate_support::request("192.0.2.9", Some(&key), Body::from("{malformed")),
    )
    .await;
    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
    assert_eq!(count(db.pool()).await, 1);
    let charge = prepare(&state, &parts("192.0.2.9", Some(&key)))
        .await
        .unwrap();
    let creator = charge.into_creator();
    let mut guard = auth::begin_account_mutation(
        &state.pool,
        creator.principal().unwrap(),
        state.lock_timeout,
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO code_reservations (short_code) VALUES ('fixture-rollback')")
        .execute(guard.connection())
        .await
        .unwrap();
    sqlx::query("INSERT INTO links (id, short_code, owner_id, destination_url, destination_search) VALUES ($1, 'fixture-rollback', $2, 'https://example.test/', 'https://example.test/')")
        .bind(uuid::Uuid::new_v4()).bind(creator.principal().unwrap().account().unwrap().id().as_uuid()).execute(guard.connection()).await.unwrap();
    guard.rollback().await.unwrap();
    assert_eq!(count(db.pool()).await, 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM links WHERE short_code = 'fixture-rollback'"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        0
    );
    let replacement = ferl_server::accounts::service::rotate_current_key(
        &state.pool,
        creator.principal().unwrap(),
        state.lock_timeout,
    )
    .await
    .unwrap();
    assert!(matches!(
        auth::begin_account_mutation(
            &state.pool,
            creator.principal().unwrap(),
            state.lock_timeout
        )
        .await,
        Err(GuardFailure::InvalidCredentials)
    ));
    assert_eq!(
        prepare(&state, &parts("192.0.2.9", Some(&key)))
            .await
            .unwrap_err(),
        CreationFailure::InvalidCredentials
    );
    assert_eq!(count(db.pool()).await, 2);
    drop(replacement);
    close(&state.pool).await;
    close(&disabled_state.pool).await;
    })).await;
}

#[tokio::test]
async fn rotation_role_changes_and_username_reuse_keep_correct_bucket_identity() {
    let db = migrated_database().await;
    db.verify(|db| Box::pin(async move {
    let state = state(
        db.schema_database_url(),
        &[("FERL_ACCOUNT_CREATE_LIMIT", "2")],
    );
    let (id, key) = seed_account(db.pool(), "identity_user", "user", true).await;
    let charge = prepare(&state, &parts("192.0.2.11", Some(&key)))
        .await
        .unwrap();
    let replacement = ferl_server::accounts::service::rotate_current_key(
        &state.pool,
        charge.creator().principal().unwrap(),
        state.lock_timeout,
    )
    .await
    .unwrap();
    let key = serde_json::to_value(replacement).unwrap()["api_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut tx = ferl_server::db::coordination::begin_read_committed(db.pool())
        .await
        .unwrap();
    ferl_server::db::coordination::acquire_account_authority_exclusive(&mut tx)
        .await
        .unwrap();
    sqlx::query("UPDATE accounts SET role = 'admin' WHERE id = $1")
        .bind(id.as_uuid())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    prepare(&state, &parts("198.51.100.3", Some(&key)))
        .await
        .unwrap();
    assert!(matches!(
        prepare(&state, &parts("192.0.2.11", Some(&key))).await,
        Err(CreationFailure::RateLimited(_))
    ));
    let mut tx = ferl_server::db::coordination::begin_read_committed(db.pool())
        .await
        .unwrap();
    ferl_server::db::coordination::acquire_account_authority_exclusive(&mut tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(id.as_uuid())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let (new_id, new_key) = seed_account(db.pool(), "identity_user", "user", true).await;
    assert_ne!(id, new_id);
    prepare(&state, &parts("192.0.2.11", Some(&new_key)))
        .await
        .unwrap();
    assert_eq!(count(db.pool()).await, 3);
    let subjects: Vec<(uuid::Uuid, i64)> = sqlx::query_as("SELECT b.account_id, count(*) FROM rate_buckets b JOIN rate_charges c ON c.bucket_id = b.id GROUP BY b.account_id ORDER BY count(*)").fetch_all(db.pool()).await.unwrap();
    assert_eq!(subjects, vec![(new_id.as_uuid(), 1), (id.as_uuid(), 2)]);
    close(&state.pool).await;
    })).await;
}

#[tokio::test]
async fn health_account_and_unimplemented_routes_do_not_charge() {
    let db = migrated_database().await;
    db.verify(|db| {
        Box::pin(async move {
            let state = state(db.schema_database_url(), &[]);
            state.verify_schema().await.unwrap();
            let (_, key) = seed_account(db.pool(), "route_user", "user", true).await;
            let app = ferl_server::app::router(state.clone());
            for (method, path, status) in [
                (http::Method::GET, "/api/health/live", 200),
                (http::Method::GET, "/api/health/ready", 200),
                (http::Method::GET, "/api/v1/me", 200),
                (http::Method::POST, "/api/v1/me/api-key", 200),
                (http::Method::GET, "/api/v1/links", 405),
                (http::Method::POST, "/api/v1/links/unknown/visits", 404),
                (http::Method::GET, "/api/v1/unknown", 404),
                (http::Method::HEAD, "/api/v1/links", 405),
            ] {
                let response = account_support::call(
                    app.clone(),
                    account_support::request(method.clone(), path, Some(&key), ""),
                )
                .await;
                assert_eq!(response.status().as_u16(), status);
                if method == http::Method::HEAD {
                    assert!(account_support::bytes(response).await.is_empty());
                }
            }
            assert_eq!(count(db.pool()).await, 0);
            drop(app);
            close(&state.pool).await;
        })
    })
    .await;
}
