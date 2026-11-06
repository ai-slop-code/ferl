#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/rate_limits.rs"]
#[allow(dead_code)]
mod rate_support;

use ferl_server::rate_limit::CreationFailure;
use rate_support::{close, count, parts, state, wait_for_query};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    task::JoinSet,
    time::timeout,
};

const TEST: &str = "independent_processes_share_allowance_and_restarts_preserve_durable_charges";

struct Worker {
    child: Child,
    output: Option<tokio::task::JoinHandle<Vec<u8>>>,
}
impl Worker {
    fn start(url: &str, credential: Option<&str>) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--nocapture"])
            .env("FERL_RATE_TEST_WORKER_URL", url)
            .env("FERL_RATE_TEST_CREDENTIAL", credential.unwrap_or(""))
            .env("RUST_LOG", "info,sqlx=trace")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let output = tokio::spawn(async move {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut stdout = stdout.take(1_048_576);
            let mut stderr = stderr.take(1_048_576);
            let (a, b) = tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
            a.unwrap();
            b.unwrap();
            out.extend(err);
            out
        });
        Self {
            child,
            output: Some(output),
        }
    }
    async fn finish(mut self) -> usize {
        let status = match timeout(Duration::from_secs(25), self.child.wait()).await {
            Ok(status) => status.unwrap(),
            Err(_) => {
                self.child.start_kill().unwrap();
                timeout(Duration::from_secs(5), self.child.wait())
                    .await
                    .unwrap()
                    .unwrap();
                panic!("rate worker exceeded its deadline");
            }
        };
        let output = timeout(Duration::from_secs(5), self.output.take().unwrap())
            .await
            .unwrap()
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(status.success(), "independent worker failed: {output}");
        assert!(
            output.contains("creation.allowance"),
            "production diagnostics were captured"
        );
        for forbidden in ["192.0.2.250", "postgres://", "SELECT ", "ferl_ak_v1_"] {
            assert!(!output.contains(forbidden));
        }
        output
            .lines()
            .find_map(|line| line.strip_prefix("RATE_WORKER_RESULT="))
            .unwrap()
            .parse()
            .unwrap()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        // Tokio's kill_on_drop child reaper owns termination on assertion/unwind.
        if let Some(task) = &self.output {
            task.abort();
        }
    }
}

async fn worker(url: &str) {
    ferl_server::telemetry::logging::init_stdout();
    let key = std::env::var("FERL_RATE_TEST_CREDENTIAL").unwrap();
    let state = state(
        url,
        &[
            ("FERL_ACCOUNT_CREATE_LIMIT", "5"),
            ("FERL_DB_POOL_MAX", "5"),
        ],
    );
    let mut tasks = JoinSet::new();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(6));
    for _ in 0..5 {
        let service = state.creation.clone();
        let key = key.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            timeout(Duration::from_secs(5), barrier.wait())
                .await
                .unwrap();
            service
                .prepare(&parts(
                    "192.0.2.250",
                    (!key.is_empty()).then_some(key.as_str()),
                ))
                .await
        });
    }
    timeout(Duration::from_secs(5), barrier.wait())
        .await
        .unwrap();
    let mut admitted = 0;
    while let Some(result) = timeout(Duration::from_secs(20), tasks.join_next())
        .await
        .unwrap()
    {
        match result.unwrap() {
            Ok(_) => admitted += 1,
            Err(CreationFailure::RateLimited(_)) => {}
            other => panic!("unexpected independent-worker result {other:?}"),
        }
    }
    close(&state.pool).await;
    ferl_server::telemetry::logging::finish_stdout(
        std::time::Instant::now() + Duration::from_secs(2),
    )
    .await;
    println!("RATE_WORKER_RESULT={admitted}");
}

#[tokio::test]
async fn independent_processes_share_allowance_and_restarts_preserve_durable_charges() {
    if let Ok(url) = std::env::var("FERL_RATE_TEST_WORKER_URL") {
        // Same executable test entry, independently executed by the parent;
        // never a production CLI command or public test route.
        worker(&url).await;
        return;
    }
    let db = account_support::migrated_database().await;
    db.verify(|db| Box::pin(async move {
    let (id, key) =
        account_support::seed_account(db.pool(), "process_creator", "admin", true).await;
    for account in [false, true] {
        let mut holder = db.pool().begin().await.unwrap();
        let bucket = uuid::Uuid::new_v4();
        if account {
            sqlx::query(
                "INSERT INTO rate_buckets (id, bucket_kind, account_id) VALUES ($1, 'account', $2)",
            )
            .bind(bucket)
            .bind(id.as_uuid())
            .execute(&mut *holder)
            .await
            .unwrap();
        } else {
            sqlx::query("INSERT INTO rate_buckets (id, bucket_kind, client_ip) VALUES ($1, 'anonymous_ip', '192.0.2.250')").bind(bucket).execute(&mut *holder).await.unwrap();
        }
        // Independent processes also perform bounded maintenance on this same
        // bucket. Two admitted attempts suffice to prune the 256 expired rows.
        sqlx::query("INSERT INTO rate_charges (id, bucket_id, charged_at) SELECT gen_random_uuid(), $1, clock_timestamp() - interval '120 seconds' FROM generate_series(1, 256)")
            .bind(bucket).execute(&mut *holder).await.unwrap();
        let first = Worker::start(db.schema_database_url(), account.then_some(key.as_str()));
        let second = Worker::start(db.schema_database_url(), account.then_some(key.as_str()));
        wait_for_query(db.pool(), "INSERT INTO rate_buckets%", 10).await;
        holder.commit().await.unwrap();
        let (first, second) = tokio::join!(first.finish(), second.finish());
        assert_eq!(
            first + second,
            5,
            "two independent processes share exactly one allowance"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges WHERE bucket_id = $1")
                .bind(bucket)
                .fetch_one(db.pool())
                .await
                .unwrap(),
            5
        );
        assert_eq!(
            Worker::start(db.schema_database_url(), account.then_some(key.as_str()))
                .finish()
                .await,
            0,
            "process restart does not reset allowance"
        );
    }
    assert_eq!(count(db.pool()).await, 10);
    let before: Vec<(uuid::Uuid, String)> =
        sqlx::query_as("SELECT id, charged_at::text FROM rate_charges ORDER BY id")
            .fetch_all(db.pool())
            .await
            .unwrap();
    // Owned fixture holds the interprocess harness lock; no recursively locking
    // script. Podman restart preserves the named volume and harness ownership.
    let restarted = database_support::command_output(
        "podman",
        &["restart", "--time", "10", database_support::CONTAINER_NAME],
    )
    .await;
    database_support::wait_until_postgres_is_ready().await;
    assert!(restarted.status.success());
    let state = state(
        db.schema_database_url(),
        &[
            ("FERL_ACCOUNT_CREATE_LIMIT", "5"),
            ("FERL_DB_POOL_MAX", "1"),
        ],
    );
    state.verify_schema().await.unwrap();
    assert!(matches!(
        state.creation.prepare(&parts("192.0.2.250", None)).await,
        Err(CreationFailure::RateLimited(_))
    ));
    assert!(matches!(
        state
            .creation
            .prepare(&parts("192.0.2.250", Some(&key)))
            .await,
        Err(CreationFailure::RateLimited(_))
    ));
    let after: Vec<(uuid::Uuid, String)> =
        sqlx::query_as("SELECT id, charged_at::text FROM rate_charges ORDER BY id")
            .fetch_all(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        before, after,
        "pool recreation and named-volume restart preserve charge identities and instants"
    );
    close(&state.pool).await;
    })).await;
}
