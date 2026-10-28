#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use ferl_server::{
    app,
    auth::{
        AccountRole, AuthFailure, GuardFailure, LinkAuthorityFailure, authenticate,
        authorize_link_access, begin_account_mutation, begin_account_write,
        begin_anonymous_link_mutation,
    },
    config::Config,
    db::coordination::{
        ACCOUNT_AUTHORITY_LOCK_KEY, acquire_account_authority_exclusive, begin_read_committed,
    },
    domain::{
        ids::{AccountId, LinkId},
        secret::{AccountApiKey, ManagementTokenSecret},
        text::ComparisonText,
    },
};
use sqlx::PgPool;
use tokio::{
    sync::Barrier,
    time::{sleep, timeout},
};
use tower::ServiceExt;
use uuid::Uuid;

use account_support::{
    bytes, call, json as response_json, migrated_database, production_router, request,
    seed_account, seed_anonymous_link,
};

const LOCK_DEADLINE: Duration = Duration::from_secs(5);

async fn seed_owned_link(pool: &PgPool, code: &str, owner: AccountId, enabled: bool) -> LinkId {
    let id = LinkId::new(Uuid::new_v4());
    let destination = "https://owned.example.test/path";
    let search = ComparisonText::from_persisted(destination);
    sqlx::query("INSERT INTO code_reservations (short_code) VALUES ($1)")
        .bind(code)
        .execute(pool)
        .await
        .expect("owned fixture code is reserved");
    sqlx::query(
        "INSERT INTO links (id, short_code, owner_id, destination_url, destination_search, enabled, visit_count) \
         VALUES ($1, $2, $3, $4, $5, $6, 0)",
    )
    .bind(id.as_uuid())
    .bind(code)
    .bind(owner.as_uuid())
    .bind(destination)
    .bind(search.as_str())
    .bind(enabled)
    .execute(pool)
    .await
    .expect("owned fixture link is persisted");
    id
}

async fn wait_for_advisory_waiters(pool: &PgPool, expected: i64) {
    let deadline = tokio::time::Instant::now() + LOCK_DEADLINE;
    loop {
        let (_, waiting): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE granted), count(*) FILTER (WHERE NOT granted) \
             FROM pg_catalog.pg_locks WHERE locktype = 'advisory' \
               AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) \
               AND classid::bigint = $1 AND objid::bigint = $2 AND objsubid = 2",
        )
        .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.0))
        .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.1))
        .fetch_one(pool)
        .await
        .expect("test observes the PostgreSQL advisory lock state");
        if waiting == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {expected} waiting account-authority lock(s), observed {waiting}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_row_update_waiter(pool: &PgPool) {
    let deadline = tokio::time::Instant::now() + LOCK_DEADLINE;
    loop {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
             WHERE datname = current_database() AND wait_event_type = 'Lock' \
               AND query LIKE 'UPDATE links SET management_token_digest%')",
        )
        .fetch_one(pool)
        .await
        .expect("test can observe a fixture token-row update waiting");
        if waiting {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "token digest update did not become blocked by the row guard"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn account_authority_revalidates_after_wait_and_holds_shared_guard_through_commit() {
    let database = migrated_database().await;
    let (owner_id, key) = seed_account(database.pool(), "guard_owner", "admin", true).await;
    let link_id = seed_owned_link(database.pool(), "guard-link", owner_id, true).await;
    let principal = authenticate(
        database.pool(),
        &http::HeaderMap::from_iter([(
            http::header::AUTHORIZATION,
            format!("Bearer {key}").parse().unwrap(),
        )]),
        Duration::from_secs(2),
    )
    .await
    .expect("credential lookup succeeds")
    .expect("account key authenticates");

    // This transaction holds the exclusive gate after authentication. The
    // guard waiter is observed in pg_locks before the credential is invalidated.
    let mut invalidation = begin_read_committed(database.pool()).await.unwrap();
    acquire_account_authority_exclusive(&mut invalidation)
        .await
        .unwrap();
    let pool = database.pool().clone();
    let stale_principal = principal.clone();
    let waiter =
        tokio::spawn(
            async move { begin_account_write(&pool, &stale_principal, LOCK_DEADLINE).await },
        );
    wait_for_advisory_waiters(database.pool(), 1).await;
    let replacement = AccountApiKey::generate().unwrap();
    sqlx::query("UPDATE accounts SET api_key_digest = $2 WHERE id = $1")
        .bind(owner_id.as_uuid())
        .bind(replacement.digest().as_bytes().as_slice())
        .execute(&mut *invalidation)
        .await
        .unwrap();
    invalidation.commit().await.unwrap();
    let result = timeout(LOCK_DEADLINE, waiter)
        .await
        .expect("guard resumes after exclusive invalidation commits")
        .expect("guard task joins");
    assert!(matches!(result, Err(GuardFailure::InvalidCredentials)));

    let fresh = authenticate(
        database.pool(),
        &http::HeaderMap::from_iter([(
            http::header::AUTHORIZATION,
            format!("Bearer {}", replacement.as_str()).parse().unwrap(),
        )]),
        Duration::from_secs(2),
    )
    .await
    .unwrap()
    .unwrap();
    let mut shared = begin_account_mutation(database.pool(), &fresh, LOCK_DEADLINE)
        .await
        .expect("fresh caller acquires the shared link-mutation guard");

    // A real protected fixture mutation holds the shared gate while an account
    // invalidation is observed waiting on the same deployment-wide key.
    let pool = database.pool().clone();
    let invalidator = tokio::spawn(async move {
        let mut guard = begin_account_write(&pool, &fresh, LOCK_DEADLINE)
            .await
            .expect("exclusive write follows the shared guard");
        sqlx::query("UPDATE accounts SET enabled = false WHERE id = $1")
            .bind(owner_id.as_uuid())
            .execute(guard.connection())
            .await
            .expect("account suspension is protected by the exclusive gate");
        guard.commit().await
    });
    wait_for_advisory_waiters(database.pool(), 1).await;
    sqlx::query("UPDATE links SET enabled = false WHERE id = $1")
        .bind(link_id.as_uuid())
        .execute(shared.connection())
        .await
        .expect("fixture link mutation executes under shared account authority");
    shared
        .commit()
        .await
        .expect("protected mutation commits before account suspension proceeds");
    timeout(LOCK_DEADLINE, invalidator)
        .await
        .expect("exclusive invalidation completes after shared commit")
        .expect("invalidation task joins")
        .expect("invalidation transaction commits");
    let (link_enabled, owner_enabled): (bool, bool) = sqlx::query_as(
        "SELECT l.enabled, a.enabled FROM links l JOIN accounts a ON a.id = l.owner_id WHERE l.id = $1",
    )
    .bind(link_id.as_uuid())
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!((link_enabled, owner_enabled), (false, false));
    database.close().await;
}

#[tokio::test]
async fn authority_guard_timeout_and_cancellation_roll_back_and_release_waiters() {
    let database = migrated_database().await;
    let (_owner_id, key) = seed_account(database.pool(), "cancel_guard", "user", true).await;
    let principal = authenticate_key(database.pool(), &key).await;
    let mut holder = begin_read_committed(database.pool()).await.unwrap();
    acquire_account_authority_exclusive(&mut holder)
        .await
        .unwrap();
    let timeout_result =
        begin_account_mutation(database.pool(), &principal, Duration::from_millis(100)).await;
    assert!(matches!(
        timeout_result,
        Err(GuardFailure::DeadlineExceeded)
    ));
    let waiting: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_locks WHERE locktype = 'advisory' AND NOT granted \
           AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) \
           AND classid::bigint = $1 AND objid::bigint = $2 AND objsubid = 2",
    )
    .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.0))
    .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.1))
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert_eq!(waiting, 0, "timed-out guard dropped its lock request");

    let pool = database.pool().clone();
    let principal = principal.clone();
    let cancelled =
        tokio::spawn(async move { begin_account_mutation(&pool, &principal, LOCK_DEADLINE).await });
    wait_for_advisory_waiters(database.pool(), 1).await;
    cancelled.abort();
    let cancelled_result = timeout(LOCK_DEADLINE, cancelled)
        .await
        .expect("cancelled authority guard task is reaped")
        .expect_err("aborted guard task reports cancellation");
    assert!(cancelled_result.is_cancelled());
    let deadline = tokio::time::Instant::now() + LOCK_DEADLINE;
    loop {
        let (granted, waiting): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE granted), count(*) FILTER (WHERE NOT granted) \
             FROM pg_catalog.pg_locks WHERE locktype = 'advisory' \
               AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) \
               AND classid::bigint = $1 AND objid::bigint = $2 AND objsubid = 2",
        )
        .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.0))
        .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.1))
        .fetch_one(database.pool())
        .await
        .unwrap();
        if waiting == 0 {
            assert_eq!(
                granted, 1,
                "only the test holder retains the exclusive gate"
            );
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        sleep(Duration::from_millis(10)).await;
    }
    holder.rollback().await.unwrap();
    wait_for_advisory_waiters(database.pool(), 0).await;
    database.close().await;
}

#[tokio::test]
async fn waiting_admin_self_action_observes_the_committed_demotion() {
    let database = migrated_database().await;
    let (admin_id, key) = seed_account(database.pool(), "waiting_admin", "admin", true).await;
    let principal = authenticate_key(database.pool(), &key).await;
    let mut demotion = begin_read_committed(database.pool()).await.unwrap();
    acquire_account_authority_exclusive(&mut demotion)
        .await
        .unwrap();
    let pool = database.pool().clone();
    let stale = principal.clone();
    let action =
        tokio::spawn(async move { begin_account_write(&pool, &stale, LOCK_DEADLINE).await });
    wait_for_advisory_waiters(database.pool(), 1).await;
    sqlx::query("UPDATE accounts SET role = 'user' WHERE id = $1")
        .bind(admin_id.as_uuid())
        .execute(&mut *demotion)
        .await
        .unwrap();
    demotion.commit().await.unwrap();
    let fresh = timeout(LOCK_DEADLINE, action)
        .await
        .expect("waiting self-action resumes after demotion commit")
        .expect("self-action task joins")
        .expect("valid key revalidates with its new role");
    assert_eq!(fresh.caller().role(), AccountRole::User);
    assert_eq!(fresh.require_admin(), Err(GuardFailure::Forbidden));
    fresh.rollback().await.unwrap();
    database.close().await;
}

#[tokio::test]
async fn anonymous_token_row_guard_revalidates_and_conceals_other_links() {
    let database = migrated_database().await;
    let (first_link, token) = seed_anonymous_link(database.pool(), "token-one").await;
    let (other_link, _) = seed_anonymous_link(database.pool(), "token-two").await;
    let authorization = http::HeaderMap::from_iter([(
        http::header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    )]);
    let principal = authenticate(database.pool(), &authorization, Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        begin_anonymous_link_mutation(database.pool(), &principal, other_link, LOCK_DEADLINE)
            .await
            .err(),
        Some(GuardFailure::NotFound)
    );

    let guard =
        begin_anonymous_link_mutation(database.pool(), &principal, first_link, LOCK_DEADLINE)
            .await
            .expect("current token row locks and revalidates");
    let replacement = ManagementTokenSecret::generate().unwrap();
    let pool = database.pool().clone();
    let old_link = first_link;
    let replacement_digest = replacement.digest();
    let updater = tokio::spawn(async move {
        sqlx::query("UPDATE links SET management_token_digest = $2 WHERE id = $1")
            .bind(old_link.as_uuid())
            .bind(replacement_digest.as_bytes().as_slice())
            .execute(&pool)
            .await
    });
    wait_for_row_update_waiter(database.pool()).await;
    guard
        .commit()
        .await
        .expect("token mutation guard releases only after transaction commit");
    timeout(LOCK_DEADLINE, updater)
        .await
        .expect("digest replacement proceeds after row guard commit")
        .expect("digest updater joins")
        .expect("token digest update succeeds");
    assert!(matches!(
        authenticate(database.pool(), &authorization, Duration::from_secs(2)).await,
        Err(AuthFailure::Invalid)
    ));
    let new_authorization = http::HeaderMap::from_iter([(
        http::header::AUTHORIZATION,
        format!("Bearer {}", replacement.as_str()).parse().unwrap(),
    )]);
    let new_principal = authenticate(database.pool(), &new_authorization, Duration::from_secs(2))
        .await
        .unwrap()
        .unwrap();
    assert!(
        begin_anonymous_link_mutation(database.pool(), &new_principal, first_link, LOCK_DEADLINE)
            .await
            .is_ok()
    );
    database.close().await;
}

#[tokio::test]
async fn link_authority_helpers_check_live_fixture_ownership_without_leaking_details() {
    let database = migrated_database().await;
    let (_admin_id, admin_key) = seed_account(database.pool(), "link_admin", "admin", true).await;
    let (owner_a, key_a) = seed_account(database.pool(), "link_owner_a", "user", true).await;
    let (owner_b, key_b) = seed_account(database.pool(), "link_owner_b", "user", true).await;
    let (disabled_owner, _disabled_key) =
        seed_account(database.pool(), "disabled_owner", "user", false).await;
    let owned_a = seed_owned_link(database.pool(), "owner-a-link", owner_a, false).await;
    let owned_b = seed_owned_link(database.pool(), "owner-b-link", owner_b, true).await;
    let disabled_owner_link =
        seed_owned_link(database.pool(), "disabled-owner-link", disabled_owner, true).await;
    let (anonymous, token) = seed_anonymous_link(database.pool(), "anonymous-auth").await;
    let admin = authenticate_key(database.pool(), &admin_key).await;
    let user_a = authenticate_key(database.pool(), &key_a).await;
    let user_b = authenticate_key(database.pool(), &key_b).await;
    let anonymous_token = authenticate_key(database.pool(), &token).await;

    assert!(
        authorize_link_access(database.pool(), &admin, owned_a, LOCK_DEADLINE)
            .await
            .is_ok()
    );
    assert!(
        authorize_link_access(database.pool(), &admin, owned_b, LOCK_DEADLINE)
            .await
            .is_ok()
    );
    assert!(
        authorize_link_access(database.pool(), &admin, disabled_owner_link, LOCK_DEADLINE)
            .await
            .is_ok(),
        "enabled admins may manage links owned by disabled accounts"
    );
    assert!(
        authorize_link_access(database.pool(), &admin, anonymous, LOCK_DEADLINE)
            .await
            .is_ok()
    );
    assert!(
        authorize_link_access(database.pool(), &user_a, owned_a, LOCK_DEADLINE)
            .await
            .is_ok(),
        "disabled links remain manageable by their enabled owner"
    );
    assert_eq!(
        authorize_link_access(database.pool(), &user_a, owned_b, LOCK_DEADLINE).await,
        Err(LinkAuthorityFailure::NotFound)
    );
    assert_eq!(
        authorize_link_access(database.pool(), &user_b, owned_a, LOCK_DEADLINE).await,
        Err(LinkAuthorityFailure::NotFound)
    );
    assert!(
        authorize_link_access(database.pool(), &anonymous_token, anonymous, LOCK_DEADLINE)
            .await
            .is_ok()
    );
    assert_eq!(
        authorize_link_access(database.pool(), &anonymous_token, owned_a, LOCK_DEADLINE).await,
        Err(LinkAuthorityFailure::NotFound)
    );
    sqlx::query("DELETE FROM links WHERE id = $1")
        .bind(owned_b.as_uuid())
        .execute(database.pool())
        .await
        .unwrap();
    assert_eq!(
        authorize_link_access(database.pool(), &admin, owned_b, LOCK_DEADLINE).await,
        Err(LinkAuthorityFailure::NotFound)
    );
    database.close().await;
}

async fn authenticate_key(pool: &PgPool, key: &str) -> ferl_server::auth::Principal {
    let headers = http::HeaderMap::from_iter([(
        http::header::AUTHORIZATION,
        format!("Bearer {key}").parse().unwrap(),
    )]);
    authenticate(pool, &headers, Duration::from_secs(2))
        .await
        .expect("fixture credential lookup succeeds")
        .expect("fixture credential authenticates")
}

#[tokio::test]
async fn controlled_last_admin_removal_races_never_commit_zero_enabled_admins() {
    for (name, first_action, second_action) in [
        ("disable-disable", "disable", "disable"),
        ("demote-demote", "demote", "demote"),
        ("delete-delete", "delete", "delete"),
        ("disable-delete", "disable", "delete"),
    ] {
        let database = migrated_database().await;
        let (first_id, first_key) =
            seed_account(database.pool(), "race_admin_one", "admin", true).await;
        let (second_id, second_key) =
            seed_account(database.pool(), "race_admin_two", "admin", true).await;
        let app = production_router(&database).await;
        let mut holder = begin_read_committed(database.pool()).await.unwrap();
        acquire_account_authority_exclusive(&mut holder)
            .await
            .unwrap();
        let barrier = std::sync::Arc::new(Barrier::new(3));
        let mut tasks = Vec::new();
        for (actor_id, key, action) in [
            (first_id, first_key, first_action),
            (second_id, second_key, second_action),
        ] {
            let app = app.clone();
            let barrier = barrier.clone();
            let uri = format!("/api/v1/admin/accounts/{}", actor_id.as_uuid());
            let (method, body) = match action {
                "disable" => (Method::PATCH, r#"{"enabled":false}"#),
                "demote" => (Method::PATCH, r#"{"role":"user"}"#),
                "delete" => (Method::DELETE, ""),
                _ => unreachable!(),
            };
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                call(app, request(method, &uri, Some(&key), body)).await
            }));
        }
        barrier.wait().await;
        wait_for_advisory_waiters(database.pool(), 2).await;
        holder
            .commit()
            .await
            .expect("test releases both independently routed mutations together");
        let mut responses = Vec::new();
        for task in tasks {
            responses.push(
                timeout(LOCK_DEADLINE, task)
                    .await
                    .expect("account removal request finishes before its deadline")
                    .expect("request task joins"),
            );
        }
        let successes = responses
            .iter()
            .filter(|response| matches!(response.status(), StatusCode::OK | StatusCode::NO_CONTENT))
            .count();
        assert_eq!(successes, 1, "race case {name}");
        for response in responses {
            if matches!(response.status(), StatusCode::OK | StatusCode::NO_CONTENT) {
                if response.status() == StatusCode::NO_CONTENT {
                    assert!(bytes(response).await.is_empty());
                } else {
                    let _ = response_json(response).await;
                }
            } else {
                assert_eq!(response.status(), StatusCode::CONFLICT, "race case {name}");
                assert_eq!(
                    response_json(response).await["error"]["code"],
                    "last_admin_required",
                    "race case {name}"
                );
            }
        }
        let enabled_admins: i64 =
            sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin' AND enabled")
                .fetch_one(database.pool())
                .await
                .unwrap();
        assert_eq!(enabled_admins, 1, "race case {name}");
        database.close().await;
    }
}

#[tokio::test]
async fn lost_commit_acknowledgement_is_not_replayed_as_a_second_account_creation() {
    let database = migrated_database().await;
    let (_admin_id, admin_key) =
        seed_account(database.pool(), "uncertain_admin", "admin", true).await;
    sqlx::query(
        "CREATE FUNCTION ferl_test_delay_account_commit() RETURNS trigger \
         LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(20); RETURN NEW; END $$",
    )
    .execute(database.pool())
    .await
    .expect("only the isolated schema receives the commit-delay fixture");
    sqlx::query(
        "CREATE CONSTRAINT TRIGGER ferl_test_delay_commit \
         AFTER INSERT ON accounts DEFERRABLE INITIALLY DEFERRED \
         FOR EACH ROW EXECUTE FUNCTION ferl_test_delay_account_commit()",
    )
    .execute(database.pool())
    .await
    .expect("deferred fixture trigger runs during COMMIT");

    let config = Config::from_map(std::collections::BTreeMap::from([
        (
            "FERL_DATABASE_URL".to_owned(),
            database.schema_database_url().to_owned(),
        ),
        (
            "FERL_PUBLIC_BASE_URL".to_owned(),
            "https://short.example.test".to_owned(),
        ),
        (
            "FERL_CURSOR_KEY".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ),
        ("FERL_DB_STATEMENT_TIMEOUT_SECS".to_owned(), "30".to_owned()),
    ]))
    .expect("test serving profile bounds statements through the deferred trigger");
    let state = app::AppState::from_config(&config).unwrap();
    state.verify_schema().await.unwrap();
    let application = app::router(state);
    let request = account_support::request(
        Method::POST,
        "/api/v1/admin/accounts",
        Some(&admin_key),
        r#"{"username":"uncertain_user"}"#,
    );
    let request_task = tokio::spawn(application.oneshot(request));

    let observed_pid = timeout(Duration::from_secs(8), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_stat_activity WHERE datname = current_database() \
                 AND pid <> pg_backend_pid() AND state = 'active' AND query LIKE 'COMMIT%' \
                 ORDER BY backend_start DESC LIMIT 1",
            )
            .fetch_optional(database.pool())
            .await
            .expect("the isolated PostgreSQL server exposes the active commit");
            if let Some(pid) = pid {
                break pid;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let pid = match observed_pid {
        Ok(pid) => pid,
        Err(_) => {
            request_task.abort();
            let _ = request_task.await;
            panic!("account creation did not reach its deferred commit trigger");
        }
    };
    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(database.pool())
        .await
        .expect("test terminates only the observed account-creation backend");
    assert!(terminated);
    let response = timeout(Duration::from_secs(10), request_task)
        .await
        .expect("commit-uncertain request returns before its deadline")
        .expect("request task joins")
        .expect("router is infallible");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = account_support::json(response).await;
    assert_eq!(body["error"]["code"], "storage_unavailable");
    assert!(body.get("api_key").is_none());

    sqlx::query("DROP TRIGGER ferl_test_delay_commit ON accounts")
        .execute(database.pool())
        .await
        .expect("commit uncertainty trigger is removed from the owned schema");
    sqlx::query("DROP FUNCTION ferl_test_delay_account_commit()")
        .execute(database.pool())
        .await
        .expect("commit uncertainty function is removed from the owned schema");
    let first_outcome: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts WHERE username = 'uncertain_user'")
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(
        first_outcome, 0,
        "terminated commit did not leave a partial row"
    );

    let app = account_support::production_router(&database).await;
    let retry = account_support::call(
        app,
        account_support::request(
            Method::POST,
            "/api/v1/admin/accounts",
            Some(&admin_key),
            r#"{"username":"uncertain_user"}"#,
        ),
    )
    .await;
    assert_eq!(retry.status(), StatusCode::CREATED);
    let retry_body = account_support::json(retry).await;
    assert!(
        retry_body["api_key"]
            .as_str()
            .unwrap()
            .starts_with("ferl_ak_v1_")
    );
    let final_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts WHERE username = 'uncertain_user'")
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(
        final_count, 1,
        "one explicit later request creates one account"
    );
    database.close().await;
}
