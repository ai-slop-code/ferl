use std::{process::Output, time::Duration};

use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{process::Command, time::timeout};
use url::Url;
use uuid::Uuid;

pub const CONTAINER_NAME: &str = "ferl-stage01-postgres";
const DATABASE_NAME: &str = "ferl_test";
const POSTGRES_IMAGE_DIGEST: &str =
    "sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94";
const POSTGRES_VOLUME: &str = "ferl-stage01-postgres-data";

pub struct IsolatedTestDatabase {
    pool: PgPool,
    admin_pool: PgPool,
    schema: String,
}

impl IsolatedTestDatabase {
    pub async fn connect_from_harness() -> Self {
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

        let search_path = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(2))
            .after_connect(move |connection, _metadata| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query("SELECT set_config('search_path', $1, false)")
                        .bind(search_path)
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&database_url)
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
        }
    }

    pub async fn close(self) {
        timeout(Duration::from_secs(5), self.pool.close())
            .await
            .expect("isolated test pool closes before its deadline");
        // The schema name remains the locally generated fixed-prefix UUID identifier.
        timeout(
            Duration::from_secs(5),
            sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {}", self.schema)))
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

pub fn validate_database_url(database_url: &str) {
    let parsed = Url::parse(database_url).expect("test database URL is valid");
    assert_eq!(parsed.scheme(), "postgres");
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    assert_eq!(parsed.path(), "/ferl_test");
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
