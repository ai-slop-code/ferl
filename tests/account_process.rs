#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/serving_process.rs"]
mod serving_process;

use std::time::Duration;

use ferl_server::db::coordination::{
    ACCOUNT_AUTHORITY_LOCK_KEY, acquire_account_authority_exclusive, begin_read_committed,
};
use tokio::{
    sync::Barrier,
    time::{sleep, timeout},
};

use account_support::{migrated_database, seed_account};
use serving_process::{ServingProcess, assert_headers, json, send};

async fn wait_for_two_gate_waiters(database: &sqlx::PgPool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let (_, waiting): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE granted), count(*) FILTER (WHERE NOT granted) \
             FROM pg_catalog.pg_locks WHERE locktype = 'advisory' \
               AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) \
               AND classid::bigint = $1 AND objid::bigint = $2 AND objsubid = 2",
        )
        .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.0))
        .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.1))
        .fetch_one(database)
        .await
        .expect("shared PostgreSQL exposes the cross-process authority waiters");
        if waiting == 2 {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "both independent server processes did not reach the authority gate"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn independent_servers_share_fresh_credentials_account_cursors_and_last_admin_gate() {
    let database = migrated_database().await;
    let (root_id, root_key) =
        seed_account(database.pool(), "process_admin_one", "admin", true).await;
    let (second_id, second_key) =
        seed_account(database.pool(), "process_admin_two", "admin", true).await;
    for index in 0..6 {
        let username = format!("page_user_{index}");
        let _ = seed_account(database.pool(), &username, "user", true).await;
    }
    let cursor_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let url = database.schema_database_url().to_owned();
    let server_a = ServingProcess::start(&url, cursor_key).await;
    let server_b = ServingProcess::start(&url, cursor_key).await;

    let first = send(
        server_a.address,
        "GET",
        "/api/v1/admin/accounts?limit=1",
        Some(&root_key),
        "",
    )
    .await
    .unwrap();
    assert_eq!(first.status, 200);
    assert_headers(&first);
    let first_json = json(&first);
    let cursor = first_json["next_cursor"].as_str().unwrap().to_owned();

    let rotation = send(
        server_a.address,
        "POST",
        "/api/v1/me/api-key",
        Some(&root_key),
        "",
    )
    .await
    .unwrap();
    assert_eq!(rotation.status, 200);
    assert_headers(&rotation);
    let replacement = json(&rotation);
    assert_eq!(
        replacement
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        ["api_key"].into_iter().collect()
    );
    let new_key = replacement["api_key"].as_str().unwrap();
    assert!(new_key.starts_with("ferl_ak_v1_"));

    let stale = send(server_b.address, "GET", "/api/v1/me", Some(&root_key), "")
        .await
        .unwrap();
    assert_eq!(stale.status, 401);
    assert_eq!(
        stale.headers.get("www-authenticate").map(String::as_str),
        Some("Bearer")
    );
    assert_headers(&stale);
    assert_eq!(json(&stale)["error"]["code"], "invalid_credentials");
    let fresh = send(server_b.address, "GET", "/api/v1/me", Some(new_key), "")
        .await
        .unwrap();
    assert_eq!(fresh.status, 200);
    assert_eq!(json(&fresh)["id"], root_id.as_uuid().to_string());

    let continued = send(
        server_b.address,
        "GET",
        &format!("/api/v1/admin/accounts?limit=3&cursor={cursor}"),
        Some(new_key),
        "",
    )
    .await
    .unwrap();
    assert_eq!(
        continued.status, 200,
        "cursor continues across instances and key rotation"
    );
    assert_headers(&continued);
    assert_eq!(json(&continued)["items"].as_array().unwrap().len(), 3);

    let mut holder = begin_read_committed(database.pool()).await.unwrap();
    acquire_account_authority_exclusive(&mut holder)
        .await
        .unwrap();
    let barrier = std::sync::Arc::new(Barrier::new(3));
    let mut tasks = Vec::new();
    for (server, id, key) in [
        (server_a.address, root_id, new_key.to_owned()),
        (server_b.address, second_id, second_key),
    ] {
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            send(
                server,
                "PATCH",
                &format!("/api/v1/admin/accounts/{}", id.as_uuid()),
                Some(&key),
                r#"{"enabled":false}"#,
            )
            .await
            .expect("independent process mutation returns an HTTP response")
        }));
    }
    barrier.wait().await;
    wait_for_two_gate_waiters(database.pool()).await;
    holder
        .commit()
        .await
        .expect("test releases cross-process admin requests");
    let first = timeout(Duration::from_secs(10), tasks.remove(0))
        .await
        .expect("first independent request completes")
        .expect("first process task joins");
    let second = timeout(Duration::from_secs(10), tasks.remove(0))
        .await
        .expect("second independent request completes")
        .expect("second process task joins");
    assert_eq!(
        [first.status, second.status]
            .into_iter()
            .filter(|status| *status == 200)
            .count(),
        1,
        "exactly one self-disable commits"
    );
    for response in [&first, &second] {
        assert_headers(response);
        if response.status == 409 {
            assert_eq!(json(response)["error"]["code"], "last_admin_required");
        } else {
            assert!(matches!(response.status, 200 | 401));
            if response.status == 401 {
                assert_eq!(json(response)["error"]["code"], "invalid_credentials");
            }
        }
    }
    let enabled_admins: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin' AND enabled")
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(enabled_admins, 1);

    server_a.stop().await;
    server_b.stop().await;
    database.close().await;
}
