#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/serving_process.rs"]
mod serving_process;

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use account_support::{migrated_database, seed_account};
use serde_json::Value;
use serving_process::{ServingProcess, assert_headers, send};
use tokio::time::sleep;

const CURSOR_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(3);
/// Scheduler, process-spawn and signal-delivery allowance above the configured
/// total budget. Exceeding this bound is a shutdown failure, not a retry case.
const SHUTDOWN_TOLERANCE: Duration = Duration::from_millis(1500);
/// Roughly 200 KB of request-completion records: well beyond a default pipe
/// buffer plus the bounded in-process diagnostic queue.
const STALL_REQUESTS: usize = 600;
const RECORD_DEADLINE: Duration = Duration::from_secs(15);
const SETTINGS: &[(&str, &str)] = &[
    ("FERL_SHUTDOWN_TIMEOUT_SECS", "3"),
    ("FERL_HTTP_DRAIN_TIMEOUT_SECS", "1"),
    ("FERL_READINESS_WITHDRAWAL_SECS", "0"),
    ("FERL_ACCOUNT_CREATE_LIMIT", "60"),
    ("FERL_ANONYMOUS_CREATE_LIMIT", "5"),
    ("FERL_DB_POOL_MAX", "4"),
];

#[tokio::test]
async fn withheld_stdout_consumer_keeps_requests_business_success_and_shutdown_bounded() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_account_id, key) =
                    seed_account(database.pool(), "stdout_stall_owner", "user", true).await;
                let database_url = database.schema_database_url().to_owned();
                let process =
                    ServingProcess::start_with_withheld_stdout(&database_url, CURSOR_KEY, SETTINGS)
                        .await;

                liveness_request_ids(&process, STALL_REQUESTS, "while stdout is withheld").await;
                let ready = send(process.address, "GET", "/api/health/ready", None, "")
                    .await
                    .expect("readiness completes while stdout is withheld");
                assert_eq!(ready.status, 200);
                assert_headers(&ready);

                let created = send(
                    process.address,
                    "POST",
                    "/api/v1/links",
                    Some(&key),
                    r#"{"destination_url":"https://stdout-stall.example.test/path","short_code":"stdout-stall"}"#,
                )
                .await
                .expect("link creation completes while stdout is withheld");
                assert_eq!(created.status, 201);
                assert_headers(&created);

                let redirect = send(process.address, "GET", "/stdout-stall", None, "")
                    .await
                    .expect("redirect completes while stdout is withheld");
                assert_eq!(redirect.status, 302);
                assert_eq!(
                    redirect.headers.get("location").map(String::as_str),
                    Some("https://stdout-stall.example.test/path")
                );

                // The consumer is still withheld when SIGTERM is delivered and
                // the child is reaped; that is the case under test.
                let output = process.stop_and_capture().await;
                assert!(
                    output.exit_success,
                    "process exits successfully after SIGTERM with stdout withheld"
                );
                assert!(
                    output.shutdown_elapsed <= SHUTDOWN_BUDGET + SHUTDOWN_TOLERANCE,
                    "exit after SIGTERM took {:?}, beyond the {:?} total budget plus tolerance",
                    output.shutdown_elapsed,
                    SHUTDOWN_BUDGET
                );
            })
        })
        .await;
}

#[tokio::test]
async fn temporary_stdout_stall_is_bounded_and_then_delivers_complete_records() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let database_url = database.schema_database_url().to_owned();
                let process =
                    ServingProcess::start_with_withheld_stdout(&database_url, CURSOR_KEY, SETTINGS)
                        .await;

                let stalled_ids =
                    liveness_request_ids(&process, STALL_REQUESTS, "during the temporary stall")
                        .await;
                process.release_stdout();

                // Observed overflow: the bounded queue reports the records it
                // actually discarded once the consumer resumes.
                let summary = wait_for_record(&process, "diagnostic loss summary", |record| {
                    record["fields"]["category"] == "diagnostic_records_dropped"
                })
                .await;
                let dropped = summary["fields"]["dropped_records"]
                    .as_u64()
                    .expect("loss summary records a numeric count");
                assert!(dropped >= 1, "the finite diagnostic buffer overflowed");

                let recovered = send(process.address, "GET", "/api/health/live", None, "")
                    .await
                    .expect("liveness completes after the stall is released");
                assert_eq!(recovered.status, 200);
                let recovered_id = recovered
                    .headers
                    .get("x-request-id")
                    .expect("response has a request ID")
                    .clone();
                wait_for_record(&process, "post-stall request record", |record| {
                    record["fields"]["request_id"] == recovered_id.as_str()
                })
                .await;

                let records = delivered_records(&process.stdout_snapshot());
                for record in &records {
                    assert!(
                        record["fields"].is_object(),
                        "every delivered record keeps its structured fields"
                    );
                }
                let delivered_stalled = records
                    .iter()
                    .filter(|record| is_liveness_completion(record))
                    .filter_map(|record| record["fields"]["request_id"].as_str())
                    .filter(|id| stalled_ids.iter().any(|stalled| stalled == id))
                    .count();
                assert!(
                    delivered_stalled < stalled_ids.len(),
                    "overflow dropped at least some stalled request records"
                );

                let output = process.stop_and_capture().await;
                assert!(output.exit_success, "process exits after recovery");
            })
        })
        .await;
}

#[tokio::test]
async fn healthy_stdout_consumer_control_retains_every_request_record() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let database_url = database.schema_database_url().to_owned();
                let process = ServingProcess::start_with(&database_url, CURSOR_KEY, SETTINGS).await;

                let ids = liveness_request_ids(&process, STALL_REQUESTS, "healthy control").await;
                let output = process.stop_and_capture().await;
                assert!(output.exit_success, "healthy control exits successfully");
                assert!(
                    output.stdout.contains("server_shutdown_complete"),
                    "bounded shutdown drains the lifecycle record to a healthy consumer"
                );
                assert!(
                    !output.stdout.contains("diagnostic_records_dropped"),
                    "a healthy consumer receives no loss summary"
                );

                let records = delivered_records(&output.stdout);
                let delivered: HashSet<&str> = records
                    .iter()
                    .filter(|record| is_liveness_completion(record))
                    .filter_map(|record| record["fields"]["request_id"].as_str())
                    .collect();
                for id in &ids {
                    assert!(
                        delivered.contains(id.as_str()),
                        "healthy consumer retains the record for request {id}"
                    );
                }
            })
        })
        .await;
}

async fn liveness_request_ids(process: &ServingProcess, count: usize, phase: &str) -> Vec<String> {
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let response = send(process.address, "GET", "/api/health/live", None, "")
            .await
            .unwrap_or_else(|error| {
                panic!("liveness request {index} {phase} did not complete: {error}")
            });
        assert_eq!(response.status, 200, "liveness request {index} {phase}");
        assert_headers(&response);
        ids.push(
            response
                .headers
                .get("x-request-id")
                .expect("liveness response has a request ID")
                .clone(),
        );
    }
    ids
}

fn is_liveness_completion(record: &Value) -> bool {
    record["fields"]["message"] == "request completed"
        && record["fields"]["route"] == "/api/health/live"
}

/// Parse only newline-terminated records. A final partial line is still being
/// written by the child and is not a delivered record.
fn delivered_records(stdout: &str) -> Vec<Value> {
    let complete = stdout.rfind('\n').map_or("", |end| &stdout[..=end]);
    complete
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).expect("each delivered stdout record is JSON")
        })
        .collect()
}

async fn wait_for_record(
    process: &ServingProcess,
    description: &str,
    matches: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + RECORD_DEADLINE;
    loop {
        if let Some(record) = delivered_records(&process.stdout_snapshot())
            .into_iter()
            .find(|record| matches(record))
        {
            return record;
        }
        assert!(
            Instant::now() < deadline,
            "{description} was not delivered before the deadline"
        );
        sleep(Duration::from_millis(20)).await;
    }
}
