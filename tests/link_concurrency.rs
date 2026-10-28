#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;

use account_support::{
    call, json, migrated_database, production_router, production_router_with, request, seed_account,
};
use axum::{body::Body, extract::ConnectInfo, http::Method, http::StatusCode};
use ferl_server::db::coordination::{
    ACCOUNT_AUTHORITY_LOCK_KEY, acquire_account_authority_exclusive, begin_read_committed,
};
use std::{net::SocketAddr, time::Duration};
use tokio::{sync::Barrier, time::timeout};

fn anonymous_create(body: &str) -> axum::http::Request<Body> {
    let mut request = request(Method::POST, "/api/v1/links", None, body);
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 88], 4567))));
    request
}

async fn wait_for_authority_waiters(pool: &sqlx::PgPool, expected: i64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND NOT granted \
             AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) \
             AND classid::bigint = $1 AND objid::bigint = $2 AND objsubid = 2",
        )
        .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.0))
        .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.1))
        .fetch_one(pool)
        .await
        .expect("test observes PostgreSQL authority-gate waiters");
        if waiting == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {expected} account-gate waiters, observed {waiting}"
        );
        tokio::task::yield_now().await;
    }
}

async fn wait_for_activity_query(pool: &sqlx::PgPool, needle: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
             WHERE datname = current_database() AND wait_event_type = 'Lock' AND query LIKE $1)",
        )
        .bind(needle)
        .fetch_one(pool)
        .await
        .expect("test observes the intended active PostgreSQL statement");
        if waiting {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the intended query did not enter a PostgreSQL lock wait: {needle}"
        );
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn concurrent_custom_code_requests_have_one_winner_and_one_durable_reservation() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, key) =
                    seed_account(database.pool(), "code_race_creator", "user", true).await;
                let application = production_router(database).await;
                let barrier = std::sync::Arc::new(Barrier::new(3));
                let mut tasks = tokio::task::JoinSet::new();
                for destination in ["https://left.example/", "https://right.example/"] {
                    let app = application.clone();
                    let key = key.clone();
                    let barrier = barrier.clone();
                    tasks.spawn(async move {
                        barrier.wait().await;
                        call(
                            app,
                            request(
                                Method::POST,
                                "/api/v1/links",
                                Some(&key),
                                &format!(
                                    r#"{{"destination_url":"{destination}","short_code":"same-code","tags":["atomic"]}}"#
                                ),
                            ),
                        )
                        .await
                    });
                }
                timeout(Duration::from_secs(5), barrier.wait())
                    .await
                    .expect("both independent requests reach the start barrier");
                let mut created = 0;
                let mut conflicted = 0;
                for _ in 0..2 {
                    let response = timeout(Duration::from_secs(8), tasks.join_next())
                        .await
                        .expect("code race finishes before the deadline")
                        .unwrap()
                        .unwrap();
                    match response.status() {
                        StatusCode::CREATED => {
                            created += 1;
                            assert_eq!(json(response).await["link"]["short_code"], "same-code");
                        }
                        StatusCode::CONFLICT => {
                            conflicted += 1;
                            assert_eq!(json(response).await["error"]["code"], "short_code_unavailable");
                        }
                        status => panic!("unexpected code-race result: {status}"),
                    }
                }
                assert_eq!((created, conflicted), (1, 1));
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links WHERE short_code = 'same-code'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations WHERE short_code = 'same-code'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM link_tags")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1,
                    "the conflict leaves no orphan tag association"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    2,
                    "both eligible attempts charge independently"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn guarded_account_mutations_revalidate_after_wait_and_hold_shared_gate_to_commit() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (admin_id, admin_key) =
                    seed_account(database.pool(), "race_gate_admin", "admin", true).await;
                let (_, second_admin_key) =
                    seed_account(database.pool(), "race_gate_admin_two", "admin", true).await;
                let (owner_id, owner_key) =
                    seed_account(database.pool(), "race_gate_owner", "user", true).await;
                let application = production_router(database).await;
                let created = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&owner_key),
                        r#"{"destination_url":"https://initial-race.example/","short_code":"authority-race"}"#,
                    ),
                )
                .await;
                assert_eq!(created.status(), StatusCode::CREATED);
                let _ = json(created).await;

                // Invalidation wins the exclusive gate before the actual route
                // enters its shared guard. The waiter is observed in pg_locks.
                let mut exclusive = begin_read_committed(database.pool()).await.unwrap();
                acquire_account_authority_exclusive(&mut exclusive)
                    .await
                    .unwrap();
                let waiting_app = application.clone();
                let stale_key = owner_key.clone();
                let mut stale_tasks = tokio::task::JoinSet::new();
                stale_tasks.spawn(async move {
                    call(
                        waiting_app,
                        request(
                            Method::PATCH,
                            "/api/v1/links/authority-race",
                            Some(&stale_key),
                            r#"{"destination_url":"https://must-not-commit.example/"}"#,
                        ),
                    )
                    .await
                });
                wait_for_authority_waiters(database.pool(), 1).await;
                sqlx::query("UPDATE accounts SET enabled = false WHERE id = $1")
                    .bind(owner_id.as_uuid())
                    .execute(&mut *exclusive)
                    .await
                    .unwrap();
                exclusive.commit().await.unwrap();
                let stale = timeout(Duration::from_secs(8), stale_tasks.join_next())
                    .await
                    .expect("stale route resumes after account invalidation")
                    .unwrap();
                let stale = stale.unwrap();
                assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(json(stale).await["error"]["code"], "invalid_credentials");
                let destination: String = sqlx::query_scalar(
                    "SELECT destination_url FROM links WHERE short_code = 'authority-race'",
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(destination, "https://initial-race.example/");
                let reenabled = call(
                    application.clone(),
                    request(
                        Method::PATCH,
                        &format!("/api/v1/admin/accounts/{}", owner_id.as_uuid()),
                        Some(&admin_key),
                        r#"{"enabled":true}"#,
                    ),
                )
                .await;
                assert_eq!(reenabled.status(), StatusCode::OK);
                let _ = json(reenabled).await;

                // Key replacement commits while a real owner mutation waits on
                // the shared authority gate; post-wait digest revalidation wins.
                let replacement =
                    ferl_server::domain::secret::AccountApiKey::generate().unwrap();
                let mut key_rotation = begin_read_committed(database.pool()).await.unwrap();
                acquire_account_authority_exclusive(&mut key_rotation)
                    .await
                    .unwrap();
                let waiting_app = application.clone();
                let stale_owner_key = owner_key.clone();
                let mut rotation_waiter = tokio::task::JoinSet::new();
                rotation_waiter.spawn(async move {
                    call(
                        waiting_app,
                        request(
                            Method::PATCH,
                            "/api/v1/links/authority-race",
                            Some(&stale_owner_key),
                            r#"{"destination_url":"https://stale-after-key-rotation.example/"}"#,
                        ),
                    )
                    .await
                });
                wait_for_authority_waiters(database.pool(), 1).await;
                sqlx::query("UPDATE accounts SET api_key_digest = $2 WHERE id = $1")
                    .bind(owner_id.as_uuid())
                    .bind(replacement.digest().as_bytes().as_slice())
                    .execute(&mut *key_rotation)
                    .await
                    .unwrap();
                key_rotation.commit().await.unwrap();
                let rejected = timeout(Duration::from_secs(8), rotation_waiter.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(json(rejected).await["error"]["code"], "invalid_credentials");
                let unchanged: String = sqlx::query_scalar(
                    "SELECT destination_url FROM links WHERE short_code = 'authority-race'",
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(unchanged, "https://initial-race.example/");

                // When a protected route has revalidated first, an exclusive
                // invalidation waits through its actual link transaction commit.
                sqlx::query(
                    "CREATE FUNCTION link_test_hold_update() RETURNS trigger LANGUAGE plpgsql AS $$ \
                     BEGIN IF NEW.destination_url <> OLD.destination_url THEN PERFORM pg_advisory_xact_lock(715303); END IF; RETURN NEW; END $$",
                )
                .execute(database.pool())
                .await
                .unwrap();
                sqlx::query(
                    "CREATE TRIGGER link_test_hold_update BEFORE UPDATE ON links FOR EACH ROW EXECUTE FUNCTION link_test_hold_update()",
                )
                .execute(database.pool())
                .await
                .unwrap();
                let mut blocker = begin_read_committed(database.pool()).await.unwrap();
                sqlx::query("SELECT pg_advisory_xact_lock(715303)")
                    .execute(&mut *blocker)
                    .await
                    .unwrap();
                let waiting_app = application.clone();
                let fresh_key = admin_key.clone();
                let mut guarded_tasks = tokio::task::JoinSet::new();
                guarded_tasks.spawn(async move {
                    (
                        "patch",
                        call(
                            waiting_app,
                            request(
                                Method::PATCH,
                                "/api/v1/links/authority-race",
                                Some(&fresh_key),
                                r#"{"destination_url":"https://commits-before-suspension.example/"}"#,
                            ),
                        )
                        .await,
                    )
                });
                wait_for_activity_query(database.pool(), "UPDATE links SET destination_url%")
                    .await;

                let invalidator_app = application.clone();
                let admin = admin_key.clone();
                guarded_tasks.spawn(async move {
                    (
                        "invalidation",
                        call(
                            invalidator_app,
                            request(
                                Method::PATCH,
                                &format!("/api/v1/admin/accounts/{}", owner_id.as_uuid()),
                                Some(&admin),
                                r#"{"enabled":false}"#,
                            ),
                        )
                        .await,
                    )
                });
                wait_for_authority_waiters(database.pool(), 1).await;
                blocker.commit().await.unwrap();
                let mut patch = None;
                let mut invalidation = None;
                for _ in 0..2 {
                    let (operation, response) = timeout(
                        Duration::from_secs(8),
                        guarded_tasks.join_next(),
                    )
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                    if operation == "patch" {
                        patch = Some(response);
                    } else {
                        invalidation = Some(response);
                    }
                }
                let patch = patch.expect("protected PATCH completed");
                assert_eq!(patch.status(), StatusCode::OK);
                assert_eq!(json(patch).await["destination_url"], "https://commits-before-suspension.example/");
                let invalidation = invalidation.expect("account invalidation completed");
                assert_eq!(invalidation.status(), StatusCode::OK);
                let _ = json(invalidation).await;
                let final_destination: String = sqlx::query_scalar(
                    "SELECT destination_url FROM links WHERE short_code = 'authority-race'",
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(final_destination, "https://commits-before-suspension.example/");
                sqlx::query("DROP TRIGGER link_test_hold_update ON links")
                    .execute(database.pool())
                    .await
                    .unwrap();
                sqlx::query("DROP FUNCTION link_test_hold_update()")
                    .execute(database.pool())
                    .await
                    .unwrap();

                let mut demotion = begin_read_committed(database.pool()).await.unwrap();
                acquire_account_authority_exclusive(&mut demotion)
                    .await
                    .unwrap();
                let waiting_app = application.clone();
                let stale_admin = admin_key.clone();
                let mut demoted_tasks = tokio::task::JoinSet::new();
                demoted_tasks.spawn(async move {
                    call(
                        waiting_app,
                        request(
                            Method::PATCH,
                            "/api/v1/links/authority-race",
                            Some(&stale_admin),
                            r#"{"destination_url":"https://demoted-admin.example/"}"#,
                        ),
                    )
                    .await
                });
                wait_for_authority_waiters(database.pool(), 1).await;
                sqlx::query("UPDATE accounts SET role = 'user' WHERE id = $1")
                    .bind(admin_id.as_uuid())
                    .execute(&mut *demotion)
                    .await
                    .unwrap();
                demotion.commit().await.unwrap();
                let waiting_admin = timeout(Duration::from_secs(8), demoted_tasks.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert_eq!(waiting_admin.status(), StatusCode::NOT_FOUND);
                assert_eq!(json(waiting_admin).await["error"]["code"], "not_found");
                let still_committed: String = sqlx::query_scalar(
                    "SELECT destination_url FROM links WHERE short_code = 'authority-race'",
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(still_committed, "https://commits-before-suspension.example/");
                assert_eq!(
                    sqlx::query_scalar::<_, String>("SELECT role FROM accounts WHERE id = $1")
                        .bind(admin_id.as_uuid())
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    "user"
                );
                let remains_admin: String = sqlx::query_scalar(
                    "SELECT role FROM accounts WHERE api_key_digest = $1",
                )
                .bind(
                    ferl_server::domain::secret::SecretDigest::from_presented_secret(
                        &second_admin_key,
                    )
                    .as_bytes()
                    .as_slice(),
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(remains_admin, "admin");

                let (deleted_owner_id, deleted_owner_key) =
                    seed_account(database.pool(), "race_deleted_owner", "user", true).await;
                let link = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&deleted_owner_key),
                        r#"{"destination_url":"https://deleted-authority.example/","short_code":"deleted-authority"}"#,
                    ),
                )
                .await;
                assert_eq!(link.status(), StatusCode::CREATED);
                let _ = json(link).await;
                let mut deletion = begin_read_committed(database.pool()).await.unwrap();
                acquire_account_authority_exclusive(&mut deletion)
                    .await
                    .unwrap();
                let waiting_app = application.clone();
                let stale_owner_key = deleted_owner_key.clone();
                let mut delete_waiter = tokio::task::JoinSet::new();
                delete_waiter.spawn(async move {
                    call(
                        waiting_app,
                        request(
                            Method::PATCH,
                            "/api/v1/links/deleted-authority",
                            Some(&stale_owner_key),
                            r#"{"destination_url":"https://must-not-survive-owner-deletion.example/"}"#,
                        ),
                    )
                    .await
                });
                wait_for_authority_waiters(database.pool(), 1).await;
                sqlx::query("DELETE FROM accounts WHERE id = $1")
                    .bind(deleted_owner_id.as_uuid())
                    .execute(&mut *deletion)
                    .await
                    .unwrap();
                deletion.commit().await.unwrap();
                let rejected = timeout(Duration::from_secs(8), delete_waiter.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(json(rejected).await["error"]["code"], "invalid_credentials");
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM links WHERE short_code = 'deleted-authority'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    0,
                    "account deletion cascades the target link rather than preserving a stale write"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM code_reservations WHERE short_code = 'deleted-authority'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    1
                );
            })
        })
        .await;
}

#[tokio::test]
async fn link_row_lock_timeout_and_request_cancellation_leave_no_partial_mutation() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, owner_key) =
                    seed_account(database.pool(), "row_timeout_owner", "user", true).await;
                let application = production_router_with(
                    database,
                    &[
                        ("FERL_DB_LOCK_TIMEOUT_SECS", "1"),
                        ("FERL_LINK_OPERATION_TIMEOUT_SECS", "5"),
                    ],
                )
                .await;
                let created = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&owner_key),
                        r#"{"destination_url":"https://row-lock-before.example/","short_code":"row-lock-test"}"#,
                    ),
                )
                .await;
                assert_eq!(created.status(), StatusCode::CREATED);
                let _ = json(created).await;

                let mut holder = begin_read_committed(database.pool()).await.unwrap();
                sqlx::query("SELECT id FROM links WHERE short_code = 'row-lock-test' FOR UPDATE")
                    .execute(&mut *holder)
                    .await
                    .unwrap();
                let timeout_app = application.clone();
                let timeout_key = owner_key.clone();
                let mut timed = tokio::task::JoinSet::new();
                timed.spawn(async move {
                    call(
                        timeout_app,
                        request(
                            Method::PATCH,
                            "/api/v1/links/row-lock-test",
                            Some(&timeout_key),
                            r#"{"destination_url":"https://row-lock-timeout.example/"}"#,
                        ),
                    )
                    .await
                });
                wait_for_activity_query(database.pool(), "SELECT id, short_code%")
                    .await;
                let timed_out = timeout(Duration::from_secs(5), timed.join_next())
                    .await
                    .expect("bounded PostgreSQL lock wait returns a response")
                    .unwrap()
                    .unwrap();
                assert_eq!(timed_out.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(json(timed_out).await["error"]["code"], "storage_unavailable");
                holder.rollback().await.unwrap();

                let mut holder = begin_read_committed(database.pool()).await.unwrap();
                sqlx::query("SELECT id FROM links WHERE short_code = 'row-lock-test' FOR UPDATE")
                    .execute(&mut *holder)
                    .await
                    .unwrap();
                let cancellation_app = application.clone();
                let cancellation_key = owner_key.clone();
                let mut cancelled = tokio::task::JoinSet::new();
                cancelled.spawn(async move {
                    call(
                        cancellation_app,
                        request(
                            Method::PATCH,
                            "/api/v1/links/row-lock-test",
                            Some(&cancellation_key),
                            r#"{"destination_url":"https://cancelled-row-lock.example/"}"#,
                        ),
                    )
                    .await
                });
                wait_for_activity_query(database.pool(), "SELECT id, short_code%")
                    .await;
                cancelled.abort_all();
                assert!(timeout(Duration::from_secs(3), cancelled.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap_err()
                    .is_cancelled());
                let release_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                loop {
                    let waiting: bool = sqlx::query_scalar(
                        "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() \
                         AND wait_event_type = 'Lock' AND query LIKE 'SELECT id, short_code%')",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                    if !waiting {
                        break;
                    }
                    assert!(tokio::time::Instant::now() < release_deadline);
                    tokio::task::yield_now().await;
                }
                holder.rollback().await.unwrap();
                let after_cancellation = call(
                    application.clone(),
                    request(
                        Method::PATCH,
                        "/api/v1/links/row-lock-test",
                        Some(&owner_key),
                        r#"{"destination_url":"https://after-cancellation.example/"}"#,
                    ),
                )
                .await;
                assert_eq!(after_cancellation.status(), StatusCode::OK);
                assert_eq!(
                    json(after_cancellation).await["destination_url"],
                    "https://after-cancellation.example/"
                );
            })
        })
        .await;
}

#[tokio::test]
async fn token_patch_serializes_with_rotation_and_deletion_on_the_live_link_row() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, admin_key) =
                    seed_account(database.pool(), "token_lock_admin", "admin", true).await;
                let application = production_router_with(
                    database,
                    &[("FERL_ANONYMOUS_CREATION", "true")],
                )
                .await;
                sqlx::query(
                    "CREATE FUNCTION link_test_hold_token_patch() RETURNS trigger LANGUAGE plpgsql AS $$ \
                     BEGIN IF NEW.destination_url <> OLD.destination_url THEN PERFORM pg_advisory_xact_lock(715304); END IF; RETURN NEW; END $$",
                )
                .execute(database.pool())
                .await
                .unwrap();
                sqlx::query(
                    "CREATE TRIGGER link_test_hold_token_patch BEFORE UPDATE ON links FOR EACH ROW EXECUTE FUNCTION link_test_hold_token_patch()",
                )
                .execute(database.pool())
                .await
                .unwrap();

                for final_operation in ["rotate", "delete"] {
                    let created = call(
                        application.clone(),
                        anonymous_create_for_token_race(final_operation),
                    )
                    .await;
                    assert_eq!(created.status(), StatusCode::CREATED);
                    let created = json(created).await;
                    let code = created["link"]["short_code"].as_str().unwrap().to_owned();
                    let uri = format!("/api/v1/links/{code}");
                    let token = created["management_token"]
                        .as_str()
                        .unwrap()
                        .to_owned();

                    let mut blocker = begin_read_committed(database.pool()).await.unwrap();
                    sqlx::query("SELECT pg_advisory_xact_lock(715304)")
                        .execute(&mut *blocker)
                        .await
                        .unwrap();
                    let patch_app = application.clone();
                    let patch_token = token.clone();
                    let patch_uri = uri.clone();
                    let mut operations = tokio::task::JoinSet::new();
                    operations.spawn(async move {
                        (
                            "patch",
                            call(
                                patch_app,
                                request(
                                    Method::PATCH,
                                    &patch_uri,
                                    Some(&patch_token),
                                    r#"{"destination_url":"https://token-patch-wins.example/"}"#,
                                ),
                            )
                            .await,
                        )
                    });
                    wait_for_activity_query(database.pool(), "UPDATE links SET destination_url%")
                        .await;

                    if final_operation == "rotate" {
                        let rotation_app = application.clone();
                        let rotation_uri = format!("{uri}/management-token");
                        let admin = admin_key.clone();
                        operations.spawn(async move {
                            (
                                "rotation",
                                call(
                                    rotation_app,
                                    request(
                                        Method::POST,
                                        &rotation_uri,
                                        Some(&admin),
                                        "",
                                    ),
                                )
                                .await,
                            )
                        });
                        wait_for_activity_query(database.pool(), "SELECT id, short_code%")
                            .await;
                        blocker.commit().await.unwrap();
                        let mut patch = None;
                        let mut rotation = None;
                        for _ in 0..2 {
                            let (operation, response) = timeout(
                                Duration::from_secs(8),
                                operations.join_next(),
                            )
                            .await
                            .unwrap()
                            .unwrap()
                            .unwrap();
                            if operation == "patch" {
                                patch = Some(response);
                            } else {
                                rotation = Some(response);
                            }
                        }
                        let patch = patch.expect("token-authorized PATCH completed");
                        assert_eq!(patch.status(), StatusCode::OK);
                        assert_eq!(
                            json(patch).await["destination_url"],
                            "https://token-patch-wins.example/"
                        );
                        let rotation = rotation.expect("token rotation completed");
                        assert_eq!(rotation.status(), StatusCode::OK);
                        let replacement = json(rotation).await["management_token"]
                            .as_str()
                            .unwrap()
                            .to_owned();
                        let old = call(
                            application.clone(),
                            request(Method::GET, &uri, Some(&token), ""),
                        )
                        .await;
                        assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
                        let fresh = call(
                            application.clone(),
                            request(Method::GET, &uri, Some(&replacement), ""),
                        )
                        .await;
                        assert_eq!(fresh.status(), StatusCode::OK);
                        let _ = json(fresh).await;
                    } else {
                        let delete_app = application.clone();
                        let delete_uri = uri.clone();
                        let admin = admin_key.clone();
                        operations.spawn(async move {
                            (
                                "deletion",
                                call(
                                    delete_app,
                                    request(Method::DELETE, &delete_uri, Some(&admin), ""),
                                )
                                .await,
                            )
                        });
                        wait_for_activity_query(database.pool(), "SELECT id, short_code%")
                            .await;
                        blocker.commit().await.unwrap();
                        let mut patch = None;
                        let mut deletion = None;
                        for _ in 0..2 {
                            let (operation, response) = timeout(
                                Duration::from_secs(8),
                                operations.join_next(),
                            )
                            .await
                            .unwrap()
                            .unwrap()
                            .unwrap();
                            if operation == "patch" {
                                patch = Some(response);
                            } else {
                                deletion = Some(response);
                            }
                        }
                        let patch = patch.expect("token-authorized PATCH completed");
                        assert_eq!(patch.status(), StatusCode::OK);
                        let _ = json(patch).await;
                        let deletion = deletion.expect("link deletion completed");
                        assert_eq!(deletion.status(), StatusCode::NO_CONTENT);
                        assert!(account_support::bytes(deletion).await.is_empty());
                        assert_eq!(
                            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links WHERE short_code = $1")
                                .bind(&code)
                                .fetch_one(database.pool())
                                .await
                                .unwrap(),
                            0
                        );
                        assert_eq!(
                            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations WHERE short_code = $1")
                                .bind(&code)
                                .fetch_one(database.pool())
                                .await
                                .unwrap(),
                            1
                        );
                    }
                }

                sqlx::query("DROP TRIGGER link_test_hold_token_patch ON links")
                    .execute(database.pool())
                    .await
                    .unwrap();
                sqlx::query("DROP FUNCTION link_test_hold_token_patch()")
                    .execute(database.pool())
                    .await
                    .unwrap();
            })
        })
        .await;
}

fn anonymous_create_for_token_race(name: &str) -> axum::http::Request<Body> {
    anonymous_create(&format!(
        r#"{{"destination_url":"https://{name}.example/"}}"#
    ))
}
