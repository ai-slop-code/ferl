#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/serving_process.rs"]
mod serving_process;

use account_support::{migrated_database, seed_account};
use database_support::wait_until_postgres_is_ready;
use serving_process::{
    ServingProcess, assert_headers, json, send, send_chunked, send_with_headers,
};
use std::{process::Output, time::Duration};
use tokio::{
    process::Command,
    sync::Barrier,
    task::JoinSet,
    time::{sleep, timeout},
};

const CURSOR_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const PROCESS_SETTINGS: &[(&str, &str)] = &[
    ("FERL_ANONYMOUS_CREATION", "true"),
    ("FERL_ANONYMOUS_CREATE_LIMIT", "5"),
    ("FERL_ACCOUNT_CREATE_LIMIT", "60"),
    ("FERL_TRUSTED_PROXIES", "127.0.0.1"),
    ("FERL_DB_POOL_MAX", "4"),
];

#[tokio::test]
async fn independent_production_servers_share_link_state_allowance_and_restart_persistence() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_account_id, initial_key) =
                    seed_account(database.pool(), "independent_link_owner", "user", true).await;
                let database_url = database.schema_database_url().to_owned();
                let first =
                    ServingProcess::start_with(&database_url, CURSOR_KEY, PROCESS_SETTINGS).await;
                let second =
                    ServingProcess::start_with(&database_url, CURSOR_KEY, PROCESS_SETTINGS).await;

                // Independent executable processes race the database's one
                // permanent reservation key; no in-process router state is shared.
                let barrier = std::sync::Arc::new(Barrier::new(3));
                let mut race = JoinSet::new();
                for (address, destination) in [
                    (first.address, "https://process-left.invalid/path"),
                    (second.address, "https://process-right.invalid/path"),
                ] {
                    let barrier = barrier.clone();
                    let credential = initial_key.clone();
                    race.spawn(async move {
                        barrier.wait().await;
                        send(
                            address,
                            "POST",
                            "/api/v1/links",
                            Some(&credential),
                            &format!(
                                r#"{{"destination_url":"{destination}","short_code":"cross-process-code","tags":["Cross Process"]}}"#
                            ),
                        )
                        .await
                        .expect("independent process returns an HTTP response")
                    });
                }
                timeout(Duration::from_secs(5), barrier.wait())
                    .await
                    .expect("both actual server processes reach the race barrier");
                let mut created = 0;
                let mut conflicts = 0;
                for _ in 0..2 {
                    let response = timeout(Duration::from_secs(8), race.join_next())
                        .await
                        .expect("cross-process custom allocation finishes")
                        .unwrap()
                        .unwrap();
                    assert_headers(&response);
                    match response.status {
                        201 => {
                            created += 1;
                            assert_eq!(
                                json(&response)["link"]["short_code"],
                                "cross-process-code"
                            );
                        }
                        409 => {
                            conflicts += 1;
                            assert_eq!(json(&response)["error"]["code"], "short_code_unavailable");
                        }
                        status => panic!("unexpected cross-process code status: {status}"),
                    }
                }
                assert_eq!((created, conflicts), (1, 1));
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links WHERE short_code = 'cross-process-code'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations WHERE short_code = 'cross-process-code'")
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
                    1
                );

                let detail = send(
                    second.address,
                    "GET",
                    "/api/v1/links/cross-process-code",
                    Some(&initial_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&detail);
                assert_eq!(detail.status, 200);
                assert_eq!(json(&detail)["tags"], serde_json::json!(["cross process"]));
                let patched = send(
                    second.address,
                    "PATCH",
                    "/api/v1/links/cross-process-code",
                    Some(&initial_key),
                    r#"{"destination_url":"https://edited-on-b.invalid/path?query=1#frag","title":"Process title"}"#,
                )
                .await
                .unwrap();
                assert_headers(&patched);
                assert_eq!(patched.status, 200);
                let patched = json(&patched);
                assert_eq!(patched["destination_url"], "https://edited-on-b.invalid/path?query=1#frag");
                let visible_on_a = send(
                    first.address,
                    "GET",
                    "/api/v1/links/cross-process-code",
                    Some(&initial_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&visible_on_a);
                assert_eq!(json(&visible_on_a)["title"], "Process title");
                let deleted = send(
                    second.address,
                    "DELETE",
                    "/api/v1/links/cross-process-code",
                    Some(&initial_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&deleted);
                assert_eq!(deleted.status, 204);
                assert!(deleted.body.is_empty());
                let not_found = send(
                    first.address,
                    "GET",
                    "/api/v1/links/cross-process-code",
                    Some(&initial_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&not_found);
                assert_eq!(not_found.status, 404);

                let persisted = send(
                    first.address,
                    "POST",
                    "/api/v1/links",
                    Some(&initial_key),
                    r#"{"destination_url":"https://persisted-between-processes.invalid/","short_code":"persist-process","title":"Persisted","tags":["RestartTag"]}"#,
                )
                .await
                .unwrap();
                assert_headers(&persisted);
                assert_eq!(persisted.status, 201);
                let _ = json(&persisted);

                let referer = "https://referrer.process-private-canary.invalid/path?q=secret#frag";
                let user_agent = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36";
                let get_visit_headers = [
                    ("Referer", referer),
                    ("User-Agent", user_agent),
                    ("X-Forwarded-For", "203.0.113.77"),
                ];
                let get = send_with_headers(
                    first.address,
                    "GET",
                    "/persist-process?source=redirect-query-private-canary",
                    None,
                    "",
                    &get_visit_headers,
                )
                .await
                .unwrap();
                assert_headers(&get);
                assert_eq!(get.status, 302);
                assert_eq!(
                    get.headers.get("location").map(String::as_str),
                    Some("https://persisted-between-processes.invalid/")
                );
                assert!(get.body.is_empty());
                let head_visit_headers = [
                    ("Referer", referer),
                    ("User-Agent", user_agent),
                    ("X-Forwarded-For", "203.0.113.80,broken"),
                ];
                let head = send_with_headers(
                    second.address,
                    "HEAD",
                    "/persist-process?campaign=ignored",
                    None,
                    "",
                    &head_visit_headers,
                )
                .await
                .unwrap();
                assert_headers(&head);
                assert_eq!(head.status, 302);
                assert_eq!(
                    head.headers.get("location").map(String::as_str),
                    Some("https://persisted-between-processes.invalid/")
                );
                assert!(head.body.is_empty());
                wait_for_visit_count(database.pool(), "persist-process", 2).await;
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT visit_count FROM links WHERE short_code = 'persist-process'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    2
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM visits WHERE redirect_url = 'https://persisted-between-processes.invalid/' \
                         AND method IN ('GET', 'HEAD') AND referrer = $1 AND user_agent = $2",
                    )
                    .bind(referer)
                    .bind(user_agent)
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    2,
                    "independent TCP processes persist the same full request analytics"
                );
                let visit_ips = sqlx::query_as::<_, (String, String)>(
                    "SELECT method, host(client_ip) FROM visits v \
                     JOIN links l ON l.id = v.link_id WHERE l.short_code = 'persist-process' \
                     ORDER BY method",
                )
                .fetch_all(database.pool())
                .await
                .unwrap();
                assert_eq!(
                    visit_ips,
                    [
                        ("GET".to_owned(), "203.0.113.77".to_owned()),
                        ("HEAD".to_owned(), "127.0.0.1".to_owned()),
                    ],
                    "actual trusted TCP peer chain and malformed-chain fallback persist"
                );

                let chunked_body = vec![b'x'; 65_537];
                let oversized = send_chunked(
                    first.address,
                    "POST",
                    "/api/v1/links",
                    None,
                    &chunked_body,
                    4096,
                    &[("X-Forwarded-For", "203.0.113.202")],
                )
                .await
                .unwrap();
                assert_headers(&oversized);
                assert_eq!(oversized.status, 413);
                assert_eq!(json(&oversized)["error"]["code"], "request_too_large");

                // Trusted transport peer identity and proxy resolution feed the
                // same durable IP allowance from both processes.
                let forwarded = [("X-Forwarded-For", "203.0.113.201")];
                let mut anonymous_code = String::new();
                let mut anonymous_token = String::new();
                let mut anonymous_codes = Vec::new();
                let mut anonymous_tokens = Vec::new();
                for index in 0..5 {
                    let address = if index % 2 == 0 {
                        first.address
                    } else {
                        second.address
                    };
                    let response = send_with_headers(
                        address,
                        "POST",
                        "/api/v1/links",
                        None,
                        &format!(
                            r#"{{"destination_url":"https://shared-anonymous-{index}.invalid/"}}"#
                        ),
                        &forwarded,
                    )
                    .await
                    .unwrap();
                    assert_headers(&response);
                    assert_eq!(response.status, 201, "anonymous charge {index}");
                    let body = json(&response);
                    anonymous_codes.push(body["link"]["short_code"].as_str().unwrap().to_owned());
                    anonymous_tokens.push(body["management_token"].as_str().unwrap().to_owned());
                    if index == 0 {
                        anonymous_code = body["link"]["short_code"]
                            .as_str()
                            .unwrap()
                            .to_owned();
                        anonymous_token = body["management_token"]
                            .as_str()
                            .unwrap()
                            .to_owned();
                    }
                }
                let sixth = send_with_headers(
                    second.address,
                    "POST",
                    "/api/v1/links",
                    None,
                    r#"{"destination_url":"https://sixth-shared-anonymous.invalid/"}"#,
                    &forwarded,
                )
                .await
                .unwrap();
                assert_headers(&sixth);
                assert_eq!(sixth.status, 429);
                assert!(sixth.headers["retry-after"].parse::<u32>().unwrap() > 0);
                assert_eq!(json(&sixth)["error"]["code"], "rate_limited");

                let anon_uri = format!("/api/v1/links/{anonymous_code}");
                let anonymous_visible = send(
                    second.address,
                    "GET",
                    &anon_uri,
                    Some(&anonymous_token),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&anonymous_visible);
                assert_eq!(anonymous_visible.status, 200);
                let rotated = send(
                    second.address,
                    "POST",
                    &format!("{anon_uri}/management-token"),
                    Some(&anonymous_token),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&rotated);
                assert_eq!(rotated.status, 200);
                let rotated_token = json(&rotated)["management_token"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let old_token = send(
                    first.address,
                    "GET",
                    &anon_uri,
                    Some(&anonymous_token),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&old_token);
                assert_eq!(old_token.status, 401);
                let new_token = send(
                    first.address,
                    "GET",
                    &anon_uri,
                    Some(&rotated_token),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&new_token);
                assert_eq!(new_token.status, 200);

                let replacement = send(
                    second.address,
                    "POST",
                    "/api/v1/me/api-key",
                    Some(&initial_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&replacement);
                assert_eq!(replacement.status, 200);
                let new_key = json(&replacement)["api_key"].as_str().unwrap().to_owned();
                let old_key = send(first.address, "GET", "/api/v1/me", Some(&initial_key), "")
                    .await
                    .unwrap();
                assert_headers(&old_key);
                assert_eq!(old_key.status, 401);
                let new_key_works = send(first.address, "GET", "/api/v1/me", Some(&new_key), "")
                    .await
                    .unwrap();
                assert_headers(&new_key_works);
                assert_eq!(new_key_works.status, 200);
                assert_eq!(
                    send(
                        first.address,
                        "GET",
                        "/api/v1/links/persist-process",
                        Some(&new_key),
                        "",
                    )
                    .await
                    .unwrap()
                    .status,
                    200,
                    "the same account owns links created through its previous key"
                );

                let first_output = first.stop_and_capture().await;
                let second_output = second.stop_and_capture().await;
                let mut canaries = vec![
                    database_url.clone(),
                    initial_key.clone(),
                    new_key.clone(),
                    anonymous_token.clone(),
                    rotated_token.clone(),
                    anonymous_code.clone(),
                    "203.0.113.202".to_owned(),
                    _account_id.as_uuid().to_string(),
                    "cross-process-code".to_owned(),
                    "persist-process".to_owned(),
                    "independent_link_owner".to_owned(),
                    "cross process".to_owned(),
                    "restarttag".to_owned(),
                    "Process title".to_owned(),
                    "https://process-left.invalid/path".to_owned(),
                    "https://process-right.invalid/path".to_owned(),
                    "203.0.113.201".to_owned(),
                    "https://edited-on-b.invalid/path?query=1#frag".to_owned(),
                    "https://persisted-between-processes.invalid/".to_owned(),
                    referer.to_owned(),
                    user_agent.to_owned(),
                    "redirect-query-private-canary".to_owned(),
                    "campaign=ignored".to_owned(),
                    "restart-revisit-private-canary".to_owned(),
                    "https://shared-anonymous-0.invalid/".to_owned(),
                    "https://shared-anonymous-1.invalid/".to_owned(),
                    "https://shared-anonymous-2.invalid/".to_owned(),
                    "https://shared-anonymous-3.invalid/".to_owned(),
                    "https://shared-anonymous-4.invalid/".to_owned(),
                    "https://sixth-shared-anonymous.invalid/".to_owned(),
                ];
                canaries.extend(anonymous_codes);
                canaries.extend(anonymous_tokens);
                canaries.extend(
                    sqlx::query_scalar::<_, String>(
                        "SELECT id::text FROM links UNION ALL SELECT id::text FROM visits \
                         UNION ALL SELECT id::text FROM rate_buckets UNION ALL SELECT id::text FROM rate_charges",
                    )
                    .fetch_all(database.pool())
                    .await
                    .unwrap(),
                );
                assert!(
                    [
                        &first_output.stdout,
                        &first_output.stderr,
                        &second_output.stdout,
                        &second_output.stderr,
                    ]
                    .iter()
                    .any(|output| output.contains("request completed")),
                    "safe request-completion diagnostic is present in captured process output"
                );
                for output in [
                    &first_output.stdout,
                    &first_output.stderr,
                    &second_output.stdout,
                    &second_output.stderr,
                ] {
                    for canary in &canaries {
                        assert!(!output.contains(canary), "process diagnostics leaked canary {canary}");
                    }
                }

                let stopped = timeout(Duration::from_secs(15), async {
                    let mut command = Command::new("podman");
                    command
                        .kill_on_drop(true)
                        .args(["stop", "--time", "10", database_support::CONTAINER_NAME]);
                    command.output().await
                })
                .await;
                // Always attempt restart before asserting the stop result, so a
                // failed/timed-out stop cannot strand the shared named volume.
                let started = timeout(Duration::from_secs(20), async {
                    let mut command = Command::new("podman");
                    command
                        .kill_on_drop(true)
                        .args(["start", database_support::CONTAINER_NAME]);
                    command.output().await
                })
                .await;
                wait_until_postgres_is_ready().await;
                assert!(matches!(stopped, Ok(Ok(Output { status, .. })) if status.success()), "owned PostgreSQL container stops before its deadline");
                assert!(matches!(started, Ok(Ok(Output { status, .. })) if status.success()), "owned PostgreSQL container restarts before its deadline");

                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    6,
                    "live owned/anonymous link rows survive named-volume restart"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM link_tags")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    9
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_buckets")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    3,
                    "two effective forwarded IPs and the account share durable buckets"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM visits v JOIN links l ON l.id = v.link_id \
                         WHERE l.short_code = 'persist-process' AND v.redirect_url = 'https://persisted-between-processes.invalid/'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    2,
                    "GET/HEAD visit rows and snapshots survive the owned volume restart"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT visit_count FROM links WHERE short_code = 'persist-process'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    2
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations WHERE short_code = 'cross-process-code'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1,
                    "deleted code reservation also persists across restart"
                );
                let token_digest: Vec<u8> = sqlx::query_scalar(
                    "SELECT management_token_digest FROM links WHERE short_code = $1",
                )
                .bind(&anonymous_code)
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(
                    token_digest,
                    ferl_server::domain::secret::SecretDigest::from_presented_secret(
                        &rotated_token,
                    )
                    .as_bytes()
                    .to_vec()
                );
                let api_digest: Vec<u8> = sqlx::query_scalar(
                    "SELECT api_key_digest FROM accounts WHERE id = $1",
                )
                .bind(_account_id.as_uuid())
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(
                    api_digest,
                    ferl_server::domain::secret::SecretDigest::from_presented_secret(&new_key)
                        .as_bytes()
                        .to_vec()
                );

                let replacement =
                    ServingProcess::start_with(&database_url, CURSOR_KEY, PROCESS_SETTINGS).await;
                let after_restart = send(
                    replacement.address,
                    "GET",
                    "/api/v1/links/persist-process",
                    Some(&new_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&after_restart);
                assert_eq!(after_restart.status, 200);
                assert_eq!(json(&after_restart)["tags"], serde_json::json!(["restarttag"]));
                let after_restart_redirect = send(
                    replacement.address,
                    "GET",
                    "/persist-process?source=restart-revisit-private-canary",
                    None,
                    "",
                )
                .await
                .unwrap();
                assert_headers(&after_restart_redirect);
                assert_eq!(after_restart_redirect.status, 302);
                assert_eq!(
                    after_restart_redirect.headers.get("location").map(String::as_str),
                    Some("https://persisted-between-processes.invalid/")
                );
                assert!(after_restart_redirect.body.is_empty());
                wait_for_visit_count(database.pool(), "persist-process", 3).await;
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT visit_count FROM links WHERE short_code = 'persist-process'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    3
                );
                let token_after_restart = send(
                    replacement.address,
                    "GET",
                    &anon_uri,
                    Some(&rotated_token),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&token_after_restart);
                assert_eq!(token_after_restart.status, 200);
                let replacement_output = replacement.stop_and_capture().await;
                assert!(
                    replacement_output.stdout.contains("request completed")
                        || replacement_output.stderr.contains("request completed")
                );
                assert!(!replacement_output.stdout.contains(&new_key));
                assert!(!replacement_output.stdout.contains(&rotated_token));
                for output in [&replacement_output.stdout, &replacement_output.stderr] {
                    for canary in &canaries {
                        assert!(
                            !output.contains(canary),
                            "replacement diagnostics leaked canary {canary}"
                        );
                    }
                }
            })
        })
        .await;
}

async fn wait_for_visit_count(pool: &sqlx::PgPool, code: &str, expected: i64) {
    timeout(Duration::from_secs(8), async {
        loop {
            let count = sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM visits v JOIN links l ON l.id = v.link_id \
                 WHERE l.short_code = $1",
            )
            .bind(code)
            .fetch_one(pool)
            .await
            .unwrap();
            if count == expected {
                return;
            }
            assert!(
                count < expected,
                "visit worker produced unexpected duplicate rows"
            );
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("visit persistence completes before its deadline");
}
