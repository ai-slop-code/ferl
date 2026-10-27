use std::{
    fs::{File, OpenOptions},
    io,
    process::Output,
    time::Duration,
};

use fs2::FileExt;
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{process::Command, time::timeout};
use url::Url;
use uuid::Uuid;

pub const CONTAINER_NAME: &str = "ferl-stage01-postgres";
#[allow(dead_code)]
const DATABASE_NAME: &str = "ferl_test";
const POSTGRES_IMAGE_DIGEST: &str =
    "sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94";
const POSTGRES_VOLUME: &str = "ferl-stage01-postgres-data";

pub struct IsolatedTestDatabase {
    pool: PgPool,
    admin_pool: PgPool,
    schema: String,
    schema_database_url: String,
    _harness_lock: File,
}

impl IsolatedTestDatabase {
    pub async fn connect_from_harness() -> Self {
        let harness_lock = acquire_harness_lock().await;
        let database_url = std::env::var("FERL_TEST_DATABASE_URL")
            .expect("FERL_TEST_DATABASE_URL is required; run scripts/postgres.sh start");
        let container_name = std::env::var("FERL_TEST_POSTGRES_CONTAINER")
            .expect("FERL_TEST_POSTGRES_CONTAINER must identify the harness container");
        assert_eq!(
            container_name, CONTAINER_NAME,
            "only the test container is allowed"
        );
        validate_database_url(&database_url);
        assert_harness_container().await;

        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
            .connect(&database_url)
            .await
            .expect("host test process connects to the harness PostgreSQL database");
        timeout(
            Duration::from_secs(5),
            sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&admin_pool),
        )
        .await
        .expect("initial PostgreSQL probe finishes before its deadline")
        .expect("PostgreSQL probe succeeds from the host process");

        let schema = format!("ferl_stage01_{}", Uuid::new_v4().simple());
        // The identifier is generated locally and consists only of this fixed prefix and UUID hex.
        timeout(
            Duration::from_secs(5),
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
                .execute(&admin_pool),
        )
        .await
        .expect("isolated schema creation finishes before its deadline")
        .expect("isolated Stage 01 test schema is created");

        let mut isolated_url = Url::parse(&database_url).expect("validated test URL parses");
        isolated_url
            .query_pairs_mut()
            .append_pair("options[search_path]", &schema);
        let schema_database_url = isolated_url.to_string();
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(2))
            .connect(&schema_database_url)
            .await
            .expect("isolated test pool connects to the harness database");
        let current_schema = timeout(
            Duration::from_secs(5),
            sqlx::query_scalar::<_, String>("SELECT current_schema()").fetch_one(&pool),
        )
        .await
        .expect("isolated schema probe finishes before its deadline")
        .expect("test pool has a current schema");
        assert_eq!(
            current_schema, schema,
            "test pool is isolated to its schema"
        );

        Self {
            pool,
            admin_pool,
            schema,
            schema_database_url,
            _harness_lock: harness_lock,
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Connection URL whose startup search_path is the fixture-owned schema.
    /// Use it for production `AppState` composition and CLI migration processes.
    pub fn schema_database_url(&self) -> &str {
        &self.schema_database_url
    }

    pub async fn close(self) {
        timeout(Duration::from_secs(5), self.pool.close())
            .await
            .expect("isolated test pool closes before its deadline");
        // The schema name remains the locally generated fixed-prefix UUID identifier.
        timeout(
            Duration::from_secs(5),
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA {} CASCADE",
                self.schema
            )))
            .execute(&self.admin_pool),
        )
        .await
        .expect("isolated schema cleanup finishes before its deadline")
        .expect("only the uniquely named test schema is removed");
        timeout(Duration::from_secs(5), self.admin_pool.close())
            .await
            .expect("test database admin pool closes before its deadline");
    }
}

/// A process-wide file lock coordinates every database test and scripts that
/// may start/stop the shared Podman container. Unlike a Rust mutex, `flock`
/// coordinates independent test binaries and concurrent harness invocations.
async fn acquire_harness_lock() -> File {
    let path = std::env::var_os("FERL_TEST_HARNESS_LOCK_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("ferl-stage01-postgres.lock"));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .expect("the shared PostgreSQL harness lock file can be opened");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return file,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => panic!("the shared PostgreSQL harness lock can be acquired"),
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the shared PostgreSQL harness lock"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub fn validate_database_url(database_url: &str) {
    let parsed = Url::parse(database_url).expect("test database URL is valid");
    assert_eq!(parsed.scheme(), "postgres");
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    assert_eq!(parsed.path(), "/ferl_test");
    assert!(
        !parsed
            .query_pairs()
            .any(|(name, _)| { name == "options" || name.starts_with("options[") }),
        "base test URL cannot override the fixture-owned PostgreSQL search_path"
    );
    let configured_port = std::env::var("FERL_TEST_POSTGRES_PORT")
        .expect("FERL_TEST_POSTGRES_PORT must be set by the harness")
        .parse::<u16>()
        .expect("test port is a valid port");
    assert_eq!(parsed.port(), Some(configured_port));
}

async fn assert_harness_container() {
    let output = command_output(
        "podman",
        &[
            "inspect",
            "--format",
            "{{ index .Config.Labels \"io.ferl.harness\" }}",
            CONTAINER_NAME,
        ],
    )
    .await;
    assert!(
        output.status.success(),
        "expected the Stage 01 Podman container"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "stage01",
        "refusing to restart a container not owned by the Stage 01 harness"
    );

    let configuration = command_output(
        "podman",
        &[
            "inspect",
            "--format",
            "{{.ImageDigest}}|{{range .Mounts}}{{.Name}}:{{.Destination}}{{end}}",
            CONTAINER_NAME,
        ],
    )
    .await;
    assert!(
        configuration.status.success(),
        "container configuration is inspectable"
    );
    assert_eq!(
        String::from_utf8_lossy(&configuration.stdout).trim(),
        format!("{POSTGRES_IMAGE_DIGEST}|{POSTGRES_VOLUME}:/var/lib/postgresql/data")
    );

    let port = std::env::var("FERL_TEST_POSTGRES_PORT").expect("test port is configured");
    let published = command_output("podman", &["port", CONTAINER_NAME, "5432/tcp"]).await;
    assert!(published.status.success(), "container exposes PostgreSQL");
    assert_eq!(
        String::from_utf8_lossy(&published.stdout).lines().last(),
        Some(format!("127.0.0.1:{port}").as_str()),
        "the harness container is published only on the configured loopback port"
    );
}

#[allow(dead_code)]
pub async fn wait_until_postgres_is_ready() {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let result = timeout(Duration::from_secs(3), {
            let mut command = Command::new("podman");
            command.kill_on_drop(true).args([
                "exec",
                CONTAINER_NAME,
                "pg_isready",
                "-q",
                "-t",
                "1",
                "-U",
                "ferl",
                "-d",
                DATABASE_NAME,
            ]);
            command.output()
        })
        .await;
        if matches!(result, Ok(Ok(output)) if output.status.success()) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "PostgreSQL did not recover before the bounded deadline"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

pub async fn command_output(program: &str, arguments: &[&str]) -> Output {
    let mut command = Command::new(program);
    command.kill_on_drop(true).args(arguments);
    timeout(Duration::from_secs(20), command.output())
        .await
        .expect("harness command completes before its deadline")
        .expect("harness command can be executed")
}
