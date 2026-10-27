use std::{
    collections::BTreeMap,
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use sqlx::{Connection, PgConnection, PgPool, Row, migrate::Migrator, postgres::PgConnectOptions};
use tokio::{
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

use crate::{app::ReadinessState, config::MigrationConfig};

/// PostgreSQL advisory lock reserved for ferl's embedded schema migrations.
/// `FERLMIGR` (`5063543843715630930`); PostgreSQL scopes advisory locks to a DB.
pub const MIGRATION_LOCK_KEY: i64 = 0x4645_524c_4d49_4752;
pub const LATEST_SCHEMA_VERSION: i64 = 1;
pub const MINIMUM_SUPPORTED_SCHEMA_VERSION: i64 = 1;
pub const SUPPORTED_POSTGRES_MAJOR: i32 = 17;
const EMBEDDED_MIGRATOR: Migrator = sqlx::migrate!();

/// Process-local readiness state. Compatibility itself is always established by
/// checking PostgreSQL's migration history and the authoritative schema objects.
#[derive(Default)]
pub struct SchemaReadiness {
    /// High bits are a verification generation; bit zero is compatibility.
    /// Failed probes advance the generation so an older verifier result cannot
    /// make the instance ready again after a newer failure.
    state: AtomicU64,
}

impl SchemaReadiness {
    pub fn compatible(&self) -> bool {
        self.state.load(Ordering::Acquire) & 1 == 1
    }

    pub(crate) fn revision(&self) -> u64 {
        self.state.load(Ordering::Acquire) >> 1
    }

    pub(crate) fn mark_compatible_if_current(&self, revision: u64) -> bool {
        let incompatible_state = revision << 1;
        let compatible_state = incompatible_state | 1;
        match self.state.compare_exchange(
            incompatible_state,
            compatible_state,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(current) => current == compatible_state,
        }
    }

    pub(crate) fn mark_incompatible(&self) {
        loop {
            let current = self.state.load(Ordering::Acquire);
            let incompatible = ((current >> 1).wrapping_add(1)) << 1;
            if self
                .state
                .compare_exchange(current, incompatible, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaIssue {
    MissingHistory,
    DirtyMigration,
    HistoryMismatch,
    UnsupportedVersion,
    NewerVersion,
    UnsupportedDatabaseVersion,
    MissingTable,
    ColumnMismatch,
    MissingConstraint,
    MissingIndex,
    MissingTrigger,
    MissingSeed,
}

impl SchemaIssue {
    pub const fn safe_category(self) -> &'static str {
        match self {
            Self::MissingHistory => "schema_history_missing",
            Self::DirtyMigration => "schema_migration_dirty",
            Self::HistoryMismatch => "schema_history_mismatch",
            Self::UnsupportedVersion => "schema_version_unsupported",
            Self::NewerVersion => "schema_version_newer_than_binary",
            Self::UnsupportedDatabaseVersion => "postgres_version_unsupported",
            Self::MissingTable => "schema_table_missing",
            Self::ColumnMismatch => "schema_column_mismatch",
            Self::MissingConstraint => "schema_constraint_missing",
            Self::MissingIndex => "schema_index_missing",
            Self::MissingTrigger => "schema_trigger_missing",
            Self::MissingSeed => "schema_seed_missing",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerificationError {
    DatabaseUnavailable,
    Incompatible(SchemaIssue),
    DeadlineExceeded,
    Superseded,
}

impl VerificationError {
    pub const fn safe_category(self) -> &'static str {
        match self {
            Self::DatabaseUnavailable => "schema_verification_database_unavailable",
            Self::Incompatible(issue) => issue.safe_category(),
            Self::DeadlineExceeded => "schema_verification_deadline",
            Self::Superseded => "schema_verification_superseded",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationFailure {
    ConnectionUnavailable,
    LockTimeout,
    TotalDeadlineExceeded,
    DatabaseUnavailable,
    LockReleaseFailed,
    MigrationExecutionFailed,
    Incompatible(SchemaIssue),
}

impl MigrationFailure {
    pub const fn safe_category(self) -> &'static str {
        match self {
            Self::ConnectionUnavailable => "migration_connection_unavailable",
            Self::LockTimeout => "migration_lock_timeout",
            Self::TotalDeadlineExceeded => "migration_total_deadline",
            Self::DatabaseUnavailable => "migration_database_unavailable",
            Self::LockReleaseFailed => "migration_lock_release_failed",
            Self::MigrationExecutionFailed => "migration_execution_failed",
            Self::Incompatible(issue) => issue.safe_category(),
        }
    }
}

/// Apply embedded migrations on a dedicated connection. SQLx 0.9's PostgreSQL
/// migrator uses a session advisory lock and releases it only on successful
/// completion; errors/cancellation can otherwise return a still-locked pooled
/// connection. This command holds its own bounded `pg_try_advisory_lock` gate on
/// a standalone connection, disables SQLx's inner lock, and closes the session
/// on every failure/cancellation path. PostgreSQL transactional DDL and SQLx's
/// per-migration transaction keep migration SQL and history bookkeeping atomic.
pub async fn run_migrations(config: &MigrationConfig) -> Result<(), MigrationFailure> {
    match timeout(config.total_timeout, run_migrations_inner(config)).await {
        Ok(result) => result,
        Err(_) => Err(MigrationFailure::TotalDeadlineExceeded),
    }
}

async fn run_migrations_inner(config: &MigrationConfig) -> Result<(), MigrationFailure> {
    let options = PgConnectOptions::from_str(config.database_url.expose_to_driver())
        .map_err(|_| MigrationFailure::ConnectionUnavailable)?;
    let mut connection =
        match timeout(config.connect_timeout, PgConnection::connect_with(&options)).await {
            Ok(Ok(connection)) => connection,
            Ok(Err(_)) => return Err(MigrationFailure::ConnectionUnavailable),
            Err(_) => return Err(MigrationFailure::ConnectionUnavailable),
        };

    let statement_timeout_ms = config.statement_timeout.as_millis().to_string();
    let lock_timeout_ms = config.lock_timeout.as_millis().to_string();
    sqlx::query(
        "SELECT set_config('statement_timeout', $1, false), \
                set_config('lock_timeout', $2, false)",
    )
    .bind(statement_timeout_ms)
    .bind(lock_timeout_ms)
    .execute(&mut connection)
    .await
    .map_err(|_| MigrationFailure::DatabaseUnavailable)?;

    ensure_supported_postgres_version(&mut connection).await?;

    acquire_migration_lock(
        &mut connection,
        config.lock_timeout,
        config.lock_poll_interval,
    )
    .await?;

    let mut migrator = Migrator::with_migrations(EMBEDDED_MIGRATOR.iter().cloned().collect());
    migrator.set_locking(false);
    if let Err(error) = migrator.run(&mut connection).await {
        return Err(map_migrator_error(error));
    }

    if let Err(error) = verify_connection(&mut connection).await {
        return Err(match error {
            VerificationError::DatabaseUnavailable | VerificationError::DeadlineExceeded => {
                MigrationFailure::DatabaseUnavailable
            }
            VerificationError::Superseded => MigrationFailure::DatabaseUnavailable,
            VerificationError::Incompatible(issue) => MigrationFailure::Incompatible(issue),
        });
    }

    let released = sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .fetch_one(&mut connection)
        .await
        .map_err(|_| MigrationFailure::LockReleaseFailed)?;
    if !released {
        return Err(MigrationFailure::LockReleaseFailed);
    }
    connection
        .close()
        .await
        .map_err(|_| MigrationFailure::DatabaseUnavailable)?;
    Ok(())
}

async fn acquire_migration_lock(
    connection: &mut PgConnection,
    lock_timeout: Duration,
    poll_interval: Duration,
) -> Result<(), MigrationFailure> {
    let deadline = Instant::now() + lock_timeout;
    loop {
        let acquired = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
            .bind(MIGRATION_LOCK_KEY)
            .fetch_one(&mut *connection)
            .await
            .map_err(|_| MigrationFailure::DatabaseUnavailable)?;
        if acquired {
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(MigrationFailure::LockTimeout);
        }
        sleep(poll_interval.min(deadline - now)).await;
    }
}

async fn ensure_supported_postgres_version(
    connection: &mut PgConnection,
) -> Result<(), MigrationFailure> {
    let version =
        sqlx::query_scalar::<_, i32>("SELECT current_setting('server_version_num')::integer")
            .fetch_one(connection)
            .await
            .map_err(|error| map_database_error(&error))?;
    if is_supported_postgres_version_num(version) {
        Ok(())
    } else {
        Err(MigrationFailure::Incompatible(
            SchemaIssue::UnsupportedDatabaseVersion,
        ))
    }
}

/// PostgreSQL 17 minor releases share one supported schema compatibility line.
pub const fn is_supported_postgres_version_num(version: i32) -> bool {
    version >= SUPPORTED_POSTGRES_MAJOR * 10_000
        && version < (SUPPORTED_POSTGRES_MAJOR + 1) * 10_000
}

fn map_migrator_error(error: sqlx::migrate::MigrateError) -> MigrationFailure {
    use sqlx::migrate::MigrateError;

    match error {
        MigrateError::Dirty(_) => MigrationFailure::Incompatible(SchemaIssue::DirtyMigration),
        MigrateError::VersionMismatch(_) => {
            MigrationFailure::Incompatible(SchemaIssue::HistoryMismatch)
        }
        MigrateError::VersionMissing(version) => {
            MigrationFailure::Incompatible(if version > LATEST_SCHEMA_VERSION {
                SchemaIssue::NewerVersion
            } else {
                SchemaIssue::UnsupportedVersion
            })
        }
        MigrateError::VersionTooOld(_, _) | MigrateError::VersionTooNew(_, _) => {
            MigrationFailure::Incompatible(SchemaIssue::HistoryMismatch)
        }
        MigrateError::Execute(error) | MigrateError::ExecuteMigration(error, _) => {
            map_database_error(&error)
        }
        _ => MigrationFailure::MigrationExecutionFailed,
    }
}

fn map_database_error(error: &sqlx::Error) -> MigrationFailure {
    use super::error::{DatabaseFailure, OperationStage, classify};

    match classify(error, OperationStage::BeforeCommit) {
        DatabaseFailure::Unavailable(_) => MigrationFailure::DatabaseUnavailable,
        DatabaseFailure::CommitOutcomeUnknown => MigrationFailure::DatabaseUnavailable,
        DatabaseFailure::Internal | DatabaseFailure::Conflict(_) | DatabaseFailure::NotFound => {
            MigrationFailure::MigrationExecutionFailed
        }
    }
}

/// Validate SQLx history plus the schema objects, constraints, indexes, trigger
/// protections and seed rows that make version 1 safe to serve. A successful
/// network probe or a table count alone is deliberately insufficient.
pub async fn verify_compatibility(pool: &PgPool) -> Result<(), VerificationError> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|_| VerificationError::DatabaseUnavailable)?;
    verify_connection(&mut connection).await
}

async fn verify_connection(connection: &mut PgConnection) -> Result<(), VerificationError> {
    let server_version =
        sqlx::query_scalar::<_, i32>("SELECT current_setting('server_version_num')::integer")
            .fetch_one(&mut *connection)
            .await
            .map_err(classify_verification_query)?;
    if !is_supported_postgres_version_num(server_version) {
        return Err(VerificationError::Incompatible(
            SchemaIssue::UnsupportedDatabaseVersion,
        ));
    }

    let history_present =
        sqlx::query_scalar::<_, bool>("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
            .fetch_one(&mut *connection)
            .await
            .map_err(classify_verification_query)?;
    if !history_present {
        return Err(VerificationError::Incompatible(SchemaIssue::MissingHistory));
    }

    let migrations =
        sqlx::query("SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *connection)
            .await
            .map_err(classify_verification_query)?;
    let mut applied = BTreeMap::new();
    for row in migrations {
        let version: i64 = row
            .try_get("version")
            .map_err(classify_verification_query)?;
        let success: bool = row
            .try_get("success")
            .map_err(classify_verification_query)?;
        let checksum: Vec<u8> = row
            .try_get("checksum")
            .map_err(classify_verification_query)?;
        if !success {
            return Err(VerificationError::Incompatible(SchemaIssue::DirtyMigration));
        }
        let Some(migration) = EMBEDDED_MIGRATOR
            .iter()
            .find(|item| item.version == version)
        else {
            return Err(VerificationError::Incompatible(
                if version > LATEST_SCHEMA_VERSION {
                    SchemaIssue::NewerVersion
                } else {
                    SchemaIssue::UnsupportedVersion
                },
            ));
        };
        if checksum.as_slice() != migration.checksum.as_ref() {
            return Err(VerificationError::Incompatible(
                SchemaIssue::HistoryMismatch,
            ));
        }
        applied.insert(version, ());
    }
    for migration in EMBEDDED_MIGRATOR.iter() {
        if !applied.contains_key(&migration.version) {
            return Err(VerificationError::Incompatible(
                SchemaIssue::HistoryMismatch,
            ));
        }
    }
    if EMBEDDED_MIGRATOR.iter().any(|migration| {
        migration.version < MINIMUM_SUPPORTED_SCHEMA_VERSION
            || migration.version > LATEST_SCHEMA_VERSION
    }) {
        return Err(VerificationError::Incompatible(
            SchemaIssue::UnsupportedVersion,
        ));
    }

    let schema_name = sqlx::query_scalar::<_, Option<String>>("SELECT current_schema()")
        .fetch_one(&mut *connection)
        .await
        .map_err(classify_verification_query)?
        .ok_or(VerificationError::Incompatible(SchemaIssue::MissingTable))?;

    let table_names = [
        "system_state",
        "accounts",
        "code_reservations",
        "links",
        "link_tags",
        "visits",
        "rate_buckets",
        "rate_charges",
        "_sqlx_migrations",
    ];
    let table_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relkind IN ('r', 'p') AND c.relname = ANY($2)",
    )
    .bind(&schema_name)
    .bind(table_names.as_slice())
    .fetch_one(&mut *connection)
    .await
    .map_err(classify_verification_query)?;
    if table_count != table_names.len() as i64 {
        return Err(VerificationError::Incompatible(SchemaIssue::MissingTable));
    }

    let required_columns = [
        ("system_state", "singleton", "boolean", "NO"),
        ("system_state", "bootstrap_completed", "boolean", "NO"),
        ("accounts", "id", "uuid", "NO"),
        ("accounts", "username", "text", "NO"),
        ("accounts", "role", "text", "NO"),
        ("accounts", "enabled", "boolean", "NO"),
        ("accounts", "api_key_digest", "bytea", "NO"),
        ("accounts", "created_at", "timestamp with time zone", "NO"),
        ("accounts", "updated_at", "timestamp with time zone", "NO"),
        ("code_reservations", "short_code", "text", "NO"),
        ("links", "id", "uuid", "NO"),
        ("links", "short_code", "text", "NO"),
        ("links", "owner_id", "uuid", "YES"),
        ("links", "destination_url", "text", "NO"),
        ("links", "destination_search", "text", "NO"),
        ("links", "title", "text", "YES"),
        ("links", "title_search", "text", "YES"),
        ("links", "enabled", "boolean", "NO"),
        ("links", "visit_count", "bigint", "NO"),
        ("links", "management_token_digest", "bytea", "YES"),
        ("links", "created_at", "timestamp with time zone", "NO"),
        ("links", "updated_at", "timestamp with time zone", "NO"),
        ("link_tags", "link_id", "uuid", "NO"),
        ("link_tags", "name", "text", "NO"),
        ("visits", "id", "uuid", "NO"),
        ("visits", "link_id", "uuid", "NO"),
        ("visits", "redirect_url", "text", "NO"),
        ("visits", "occurred_at", "timestamp with time zone", "NO"),
        ("visits", "method", "text", "NO"),
        ("visits", "client_ip", "inet", "NO"),
        ("visits", "referrer", "text", "YES"),
        ("visits", "user_agent", "text", "YES"),
        ("visits", "device", "text", "NO"),
        ("visits", "browser", "text", "NO"),
        ("visits", "os", "text", "NO"),
        ("rate_buckets", "id", "uuid", "NO"),
        ("rate_buckets", "bucket_kind", "text", "NO"),
        ("rate_buckets", "account_id", "uuid", "YES"),
        ("rate_buckets", "client_ip", "inet", "YES"),
        (
            "rate_buckets",
            "created_at",
            "timestamp with time zone",
            "NO",
        ),
        ("rate_charges", "id", "uuid", "NO"),
        ("rate_charges", "bucket_id", "uuid", "NO"),
        (
            "rate_charges",
            "charged_at",
            "timestamp with time zone",
            "NO",
        ),
    ];
    let column_signatures = required_columns
        .iter()
        .map(|(table, column, kind, nullable)| format!("{table}:{column}:{kind}:{nullable}"))
        .collect::<Vec<_>>();
    let column_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM information_schema.columns \
         WHERE table_schema = $1 AND \
               (table_name || ':' || column_name || ':' || data_type || ':' || is_nullable) = ANY($2)",
    )
    .bind(&schema_name)
    .bind(column_signatures.as_slice())
    .fetch_one(&mut *connection)
    .await
    .map_err(classify_verification_query)?;
    if column_count != required_columns.len() as i64 {
        return Err(VerificationError::Incompatible(SchemaIssue::ColumnMismatch));
    }
    let core_table_names = [
        "system_state",
        "accounts",
        "code_reservations",
        "links",
        "link_tags",
        "visits",
        "rate_buckets",
        "rate_charges",
    ];
    let core_column_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM information_schema.columns \
         WHERE table_schema = $1 AND table_name = ANY($2)",
    )
    .bind(&schema_name)
    .bind(core_table_names.as_slice())
    .fetch_one(&mut *connection)
    .await
    .map_err(classify_verification_query)?;
    if core_column_count != required_columns.len() as i64 {
        return Err(VerificationError::Incompatible(SchemaIssue::ColumnMismatch));
    }

    let byte_collated_columns = [
        "accounts:username",
        "accounts:role",
        "code_reservations:short_code",
        "links:short_code",
        "links:destination_url",
        "links:destination_search",
        "links:title_search",
        "link_tags:name",
        "visits:redirect_url",
        "visits:method",
        "visits:device",
        "visits:browser",
        "visits:os",
        "rate_buckets:bucket_kind",
    ];
    let collation_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM information_schema.columns \
         WHERE table_schema = $1 AND collation_name = 'C' \
           AND (table_name || ':' || column_name) = ANY($2)",
    )
    .bind(&schema_name)
    .bind(byte_collated_columns.as_slice())
    .fetch_one(&mut *connection)
    .await
    .map_err(classify_verification_query)?;
    if collation_count != byte_collated_columns.len() as i64 {
        return Err(VerificationError::Incompatible(SchemaIssue::ColumnMismatch));
    }

    let constraint_pairs = [
        ("system_state", "system_state_pkey"),
        ("system_state", "system_state_singleton_check"),
        ("accounts", "accounts_pkey"),
        ("accounts", "accounts_username_unique"),
        ("accounts", "accounts_api_key_digest_unique"),
        ("accounts", "accounts_username_syntax_check"),
        ("accounts", "accounts_role_check"),
        ("accounts", "accounts_api_key_digest_length_check"),
        ("code_reservations", "code_reservations_pkey"),
        (
            "code_reservations",
            "code_reservations_short_code_syntax_check",
        ),
        ("links", "links_pkey"),
        ("links", "links_short_code_unique"),
        ("links", "links_code_reservation_fk"),
        ("links", "links_owner_fk"),
        ("links", "links_short_code_syntax_check"),
        ("links", "links_destination_length_check"),
        ("links", "links_destination_scheme_check"),
        ("links", "links_title_search_consistency_check"),
        ("links", "links_visit_count_nonnegative_check"),
        ("links", "links_management_token_digest_length_check"),
        ("links", "links_ownership_mode_check"),
        ("link_tags", "link_tags_pkey"),
        ("link_tags", "link_tags_link_fk"),
        ("link_tags", "link_tags_name_check"),
        ("visits", "visits_pkey"),
        ("visits", "visits_link_fk"),
        ("visits", "visits_redirect_url_length_check"),
        ("visits", "visits_redirect_url_scheme_check"),
        ("visits", "visits_method_check"),
        ("visits", "visits_client_ip_host_check"),
        ("visits", "visits_referrer_length_check"),
        ("visits", "visits_user_agent_length_check"),
        ("visits", "visits_device_check"),
        ("visits", "visits_browser_nonempty_check"),
        ("visits", "visits_os_nonempty_check"),
        ("rate_buckets", "rate_buckets_pkey"),
        ("rate_buckets", "rate_buckets_kind_check"),
        ("rate_buckets", "rate_buckets_identity_check"),
        ("rate_charges", "rate_charges_pkey"),
        ("rate_charges", "rate_charges_bucket_fk"),
    ];
    let constraint_names = constraint_pairs
        .iter()
        .map(|(table, name)| format!("{table}:{name}"))
        .collect::<Vec<_>>();
    let constraint_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(DISTINCT r.relname || ':' || c.conname) FROM pg_catalog.pg_constraint c \
         JOIN pg_catalog.pg_class r ON r.oid = c.conrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = r.relnamespace \
         WHERE n.nspname = $1 AND (r.relname || ':' || c.conname) = ANY($2)",
    )
    .bind(&schema_name)
    .bind(constraint_names.as_slice())
    .fetch_one(&mut *connection)
    .await
    .map_err(classify_verification_query)?;
    if constraint_count != constraint_pairs.len() as i64 {
        return Err(VerificationError::Incompatible(
            SchemaIssue::MissingConstraint,
        ));
    }

    let index_pairs = [
        ("accounts", "accounts_api_key_digest_unique"),
        ("links", "links_short_code_unique"),
        ("links", "links_owner_id_idx"),
        ("links", "links_management_token_digest_unique"),
        ("visits", "visits_link_occurred_id_idx"),
        ("link_tags", "link_tags_name_link_idx"),
        ("rate_buckets", "rate_buckets_account_identity_unique"),
        ("rate_buckets", "rate_buckets_client_ip_identity_unique"),
        ("rate_charges", "rate_charges_bucket_time_idx"),
    ];
    let index_names = index_pairs
        .iter()
        .map(|(table, name)| format!("{table}:{name}"))
        .collect::<Vec<_>>();
    let index_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(DISTINCT t.relname || ':' || i.relname) FROM pg_catalog.pg_class i \
         JOIN pg_catalog.pg_namespace n ON n.oid = i.relnamespace \
         JOIN pg_catalog.pg_index x ON x.indexrelid = i.oid \
         JOIN pg_catalog.pg_class t ON t.oid = x.indrelid \
         WHERE n.nspname = $1 AND i.relkind = 'i' \
           AND (t.relname || ':' || i.relname) = ANY($2)",
    )
    .bind(&schema_name)
    .bind(index_names.as_slice())
    .fetch_one(&mut *connection)
    .await
    .map_err(classify_verification_query)?;
    if index_count != index_pairs.len() as i64 {
        return Err(VerificationError::Incompatible(SchemaIssue::MissingIndex));
    }

    let trigger_pairs = [
        ("system_state", "system_state_immutable_trigger"),
        ("accounts", "accounts_identity_immutable_trigger"),
        ("code_reservations", "code_reservations_permanent_trigger"),
        ("links", "links_identity_immutable_trigger"),
        ("visits", "visits_immutable_trigger"),
    ];
    let trigger_names = trigger_pairs
        .iter()
        .map(|(table, name)| format!("{table}:{name}"))
        .collect::<Vec<_>>();
    let trigger_count = sqlx::query_scalar::<_, i64>(
        "SELECT count(DISTINCT r.relname || ':' || t.tgname) FROM pg_catalog.pg_trigger t \
         JOIN pg_catalog.pg_class r ON r.oid = t.tgrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = r.relnamespace \
         WHERE n.nspname = $1 AND NOT t.tgisinternal \
           AND (r.relname || ':' || t.tgname) = ANY($2)",
    )
    .bind(&schema_name)
    .bind(trigger_names.as_slice())
    .fetch_one(&mut *connection)
    .await
    .map_err(classify_verification_query)?;
    if trigger_count != trigger_pairs.len() as i64 {
        return Err(VerificationError::Incompatible(SchemaIssue::MissingTrigger));
    }

    let seeded = sqlx::query_scalar::<_, bool>(
        "SELECT (SELECT count(*) = 1 FROM system_state WHERE singleton) \
         AND EXISTS (SELECT 1 FROM code_reservations WHERE short_code = 'api')",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(classify_verification_query)?;
    if !seeded {
        return Err(VerificationError::Incompatible(SchemaIssue::MissingSeed));
    }

    Ok(())
}

fn classify_verification_query(error: sqlx::Error) -> VerificationError {
    if let Some(database_error) = error.as_database_error()
        && matches!(
            database_error.code().as_deref(),
            Some("42P01" | "42703" | "42883" | "3F000")
        )
    {
        return VerificationError::Incompatible(SchemaIssue::MissingTable);
    }
    VerificationError::DatabaseUnavailable
}

/// Keep schema compatibility fresh without blocking liveness. The loop performs
/// one bounded real verification immediately, then rechecks on a fixed interval;
/// recovery after PostgreSQL/schema repair becomes visible without restart.
pub fn spawn_schema_monitor(
    pool: PgPool,
    readiness: std::sync::Arc<ReadinessState>,
    cancellation: CancellationToken,
    recheck_interval: Duration,
    check_timeout: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let revision = readiness.schema_revision();
            let result = tokio::select! {
                _ = cancellation.cancelled() => return,
                result = timeout(check_timeout, verify_compatibility(&pool)) => result,
            };
            match result {
                Ok(Ok(())) => {
                    readiness.mark_schema_compatible_if_current(revision);
                }
                Ok(Err(error)) => {
                    readiness.mark_schema_incompatible();
                    tracing::warn!(
                        category = error.safe_category(),
                        "schema compatibility verification did not pass"
                    );
                }
                Err(_) => {
                    readiness.mark_schema_incompatible();
                    tracing::warn!(
                        category = "schema_verification_deadline",
                        "schema compatibility verification exceeded its deadline"
                    );
                }
            }
            tokio::select! {
                _ = cancellation.cancelled() => return,
                _ = sleep(recheck_interval) => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{SchemaReadiness, is_supported_postgres_version_num};

    #[test]
    fn only_postgres_17_minor_releases_are_supported() {
        assert!(is_supported_postgres_version_num(170000));
        assert!(is_supported_postgres_version_num(170006));
        assert!(!is_supported_postgres_version_num(169999));
        assert!(!is_supported_postgres_version_num(180000));
    }

    #[test]
    fn a_stale_schema_verification_cannot_override_a_newer_database_failure() {
        let readiness = SchemaReadiness::default();
        let earlier_revision = readiness.revision();
        readiness.mark_incompatible();
        assert!(!readiness.mark_compatible_if_current(earlier_revision));
        assert!(!readiness.compatible());
    }
}
