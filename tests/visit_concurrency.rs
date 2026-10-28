#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/analytics_commit_proxy.rs"]
mod analytics_commit_proxy;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/redirects.rs"]
#[allow(dead_code)]
mod redirect_support;

use std::{net::SocketAddr, time::Duration};

use account_support::{migrated_database, request as api_request, seed_anonymous_link};
use analytics_commit_proxy::AnalyticsCommitProxy;
use axum::http::{Method, StatusCode};
use ferl_server::{analytics::WorkerStopOutcome, app};
use redirect_support::{call, request, state, wait_for_visits};
use sqlx::{PgPool, Postgres, Transaction};
use tokio::{task::JoinSet, time::timeout};

const ADVISORY_KEY: i64 = 79_022_227_001;
const PEER: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 44000);

#[tokio::test]
async fn unknown_visit_commit_acknowledgement_retries_the_same_event_idempotently() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                seed_anonymous_link(database.pool(), "ambiguous-visit").await;
                let mut proxy = AnalyticsCommitProxy::start(database.schema_database_url()).await;
                let state = state(
                    &proxy.url,
                    &[
                        ("FERL_DB_POOL_MAX", "1"),
                        ("FERL_ANALYTICS_RETRY_BUDGET", "1"),
                    ],
                );
                state.verify_schema().await.unwrap();
                let app = app::router(state.clone());
                let workers = state.start_analytics_workers().unwrap();

                let response = call(
                    app.clone(),
                    request(Method::GET, "/ambiguous-visit", PEER),
                )
                .await;
                assert_eq!(response.status(), StatusCode::FOUND);
                assert!(response.headers().contains_key(http::header::LOCATION));
                proxy.committed().await;

                // The server committed the visit and counter before the proxy
                // withheld acknowledgement. Verify the durable outcome while
                // the worker still believes COMMIT is unresolved.
                let committed = sqlx::query_as::<_, (i64, i64)>(
                    "SELECT count(*), coalesce(max(l.visit_count), 0)::bigint FROM visits v \
                     JOIN links l ON l.id = v.link_id WHERE l.short_code = 'ambiguous-visit'",
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(committed, (1, 1));

                proxy.release_first_commit();
                proxy.retry_connected().await;
                assert_eq!(
                    workers.shutdown(Duration::from_secs(8)).await,
                    WorkerStopOutcome::Drained,
                    "worker finished its idempotent retry before shutdown"
                );
                let final_state = sqlx::query_as::<_, (i64, i64, i64)>(
                    "SELECT count(*), count(DISTINCT v.id), coalesce(max(l.visit_count), 0)::bigint FROM visits v \
                     JOIN links l ON l.id = v.link_id WHERE l.short_code = 'ambiguous-visit'",
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(final_state, (1, 1, 1));

                let pool = state.pool.clone();
                drop(app);
                drop(state);
                timeout(Duration::from_secs(5), pool.close())
                    .await
                    .expect("production pool closes before proxy shutdown");
                proxy.stop().await;
            })
        })
        .await;
}

#[tokio::test]
async fn concurrent_redirects_count_once_and_both_delete_lock_orders_are_safe() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_id, token) = seed_anonymous_link(database.pool(), "counter-race").await;
                let (_id, worker_first_token) =
                    seed_anonymous_link(database.pool(), "delete-worker-first").await;
                let (delete_first_id, _token) =
                    seed_anonymous_link(database.pool(), "delete-first").await;
                let state = state(
                    database.schema_database_url(),
                    &[
                        ("FERL_ANALYTICS_WORKERS", "4"),
                        ("FERL_ANALYTICS_QUEUE_CAPACITY", "64"),
                        ("FERL_DB_LOCK_TIMEOUT_SECS", "10"),
                    ],
                );
                state.verify_schema().await.unwrap();
                let app = app::router(state.clone());
                let workers = state.start_analytics_workers().unwrap();

                let mut requests = JoinSet::new();
                for index in 0..16_u8 {
                    let app = app.clone();
                    requests.spawn(async move {
                        let peer = SocketAddr::from(([198, 51, 100, index + 1], 45000 + u16::from(index)));
                        call(app, request(Method::GET, "/counter-race", peer)).await
                    });
                }
                for _ in 0..16 {
                    let response = timeout(Duration::from_secs(8), requests.join_next())
                        .await
                        .expect("concurrent redirect finishes before deadline")
                        .unwrap()
                        .unwrap();
                    assert_eq!(response.status(), StatusCode::FOUND);
                    assert!(response.headers().contains_key(http::header::LOCATION));
                }
                wait_for_visits(database.pool(), "counter-race", 16).await;
                let (visit_count, unique_ids): (i64, i64) = sqlx::query_as(
                    "SELECT count(*), count(DISTINCT v.id) FROM visits v \
                     JOIN links l ON l.id = v.link_id WHERE l.short_code = 'counter-race'",
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!((visit_count, unique_ids), (16, 16));
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT visit_count FROM links WHERE short_code = 'counter-race'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    16,
                    "concurrent counter increments are not lost"
                );

                // Worker wins the live link row first, then waits in a real
                // PostgreSQL trigger. The public DELETE is observed waiting on
                // that row; releasing the trigger lets the write commit before
                // DELETE cascades both event and counter with the live link.
                sqlx::query(
                    "CREATE FUNCTION wait_visit_advisory() RETURNS trigger LANGUAGE plpgsql AS $$ \
                     BEGIN PERFORM pg_advisory_xact_lock(79022227001); RETURN NEW; END; $$",
                )
                .execute(database.pool())
                .await
                .unwrap();
                sqlx::query(
                    "CREATE TRIGGER wait_visit_advisory BEFORE INSERT ON visits \
                     FOR EACH ROW EXECUTE FUNCTION wait_visit_advisory()",
                )
                .execute(database.pool())
                .await
                .unwrap();
                let advisory = advisory_transaction(database.pool()).await;
                let issued = call(
                    app.clone(),
                    request(Method::GET, "/delete-worker-first", PEER),
                )
                .await;
                assert_eq!(issued.status(), StatusCode::FOUND);
                wait_for_lock_waiters(database.pool(), 1).await;

                let mut delete_tasks = JoinSet::new();
                let delete_app = app.clone();
                delete_tasks.spawn(async move {
                    account_support::call(
                        delete_app,
                        api_request(
                            Method::DELETE,
                            "/api/v1/links/delete-worker-first",
                            Some(&worker_first_token),
                            "",
                        ),
                    )
                    .await
                });
                wait_for_lock_waiters(database.pool(), 2).await;
                advisory.commit().await.expect("test releases trigger lock");
                let deleted = timeout(Duration::from_secs(10), delete_tasks.join_next())
                    .await
                    .expect("delete unblocks after worker commit")
                    .unwrap()
                    .unwrap();
                assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
                wait_for_deleted_code(database.pool(), "delete-worker-first").await;
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM visits WHERE link_id NOT IN (SELECT id FROM links)",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    0,
                    "cascade leaves no orphan visits"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM code_reservations WHERE short_code = 'delete-worker-first'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    1,
                    "deletion retains its permanent code reservation"
                );
                sqlx::query("DROP TRIGGER wait_visit_advisory ON visits")
                    .execute(database.pool())
                    .await
                    .unwrap();
                sqlx::query("DROP FUNCTION wait_visit_advisory()")
                    .execute(database.pool())
                    .await
                    .unwrap();

                // DELETE owns the row first. A read-only resolver may still
                // issue the already-resolved redirect before the DELETE commit;
                // its queued worker then observes the committed absence and
                // discards the event rather than resurrecting the link.
                let mut deletion = database.pool().begin().await.unwrap();
                sqlx::query("DELETE FROM links WHERE id = $1")
                    .bind(delete_first_id.as_uuid())
                    .execute(&mut *deletion)
                    .await
                    .unwrap();
                let issued = call(app.clone(), request(Method::GET, "/delete-first", PEER)).await;
                assert_eq!(issued.status(), StatusCode::FOUND);
                wait_for_lock_waiters(database.pool(), 1).await;
                deletion.commit().await.unwrap();
                wait_for_visit_total(database.pool(), 16).await;
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM links WHERE short_code = 'delete-first'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM visits WHERE link_id = $1",
                    )
                    .bind(delete_first_id.as_uuid())
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    0,
                    "worker-after-delete does not insert or resurrect a visit"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM code_reservations WHERE short_code = 'delete-first'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    1
                );

                assert_eq!(
                    workers.shutdown(Duration::from_secs(8)).await,
                    WorkerStopOutcome::Drained
                );
                let pool = state.pool.clone();
                drop(app);
                drop(state);
                timeout(Duration::from_secs(5), pool.close())
                    .await
                    .expect("production pool closes before schema cleanup");

                let _ = token;
            })
        })
        .await;
}

async fn advisory_transaction(pool: &PgPool) -> Transaction<'static, Postgres> {
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ADVISORY_KEY)
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction
}

async fn wait_for_lock_waiters(pool: &PgPool, expected: i64) {
    timeout(Duration::from_secs(8), async {
        loop {
            let count = sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE datname = current_database() AND pid <> pg_backend_pid() \
                   AND wait_event_type = 'Lock'",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if count >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("PostgreSQL reports the required lock wait before the deadline");
}

async fn wait_for_deleted_code(pool: &PgPool, code: &str) {
    timeout(Duration::from_secs(5), async {
        loop {
            let count =
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links WHERE short_code = $1")
                    .bind(code)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            if count == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("deleted link disappears before deadline");
}

async fn wait_for_visit_total(pool: &PgPool, expected: i64) {
    timeout(Duration::from_secs(8), async {
        loop {
            let count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits")
                .fetch_one(pool)
                .await
                .unwrap();
            if count == expected {
                return;
            }
            assert!(count <= expected, "unexpected visit row or resurrection");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("buffered deletion race settles before deadline");
}
