#[path = "support/database.rs"]
mod database_support;

use std::{net::IpAddr, str::FromStr};

use ferl_server::db::error::{DatabaseFailure, OperationStage, RecognizedConflict, classify};
use sqlx::PgPool;
use uuid::Uuid;

use database_support::IsolatedTestDatabase;

#[tokio::test]
async fn accounts_reservations_roles_and_digest_constraints_are_authoritative() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    migrate(&database).await;
    let pool = database.pool();
    let first_id = Uuid::from_u128(1);
    insert_account(pool, first_id, "alice_1", "user", &[1; 32])
        .await
        .expect("valid account inserts");

    let duplicate_username = insert_account(pool, Uuid::from_u128(2), "alice_1", "user", &[2; 32])
        .await
        .expect_err("username is globally unique among live accounts");
    assert_eq!(
        classify(&duplicate_username, OperationStage::BeforeCommit),
        DatabaseFailure::Conflict(RecognizedConflict::Username)
    );

    let duplicate_digest =
        insert_account(pool, Uuid::from_u128(3), "another_user", "user", &[1; 32])
            .await
            .expect_err("account credential digests are authoritative and unique");
    assert_eq!(
        classify(&duplicate_digest, OperationStage::BeforeCommit),
        DatabaseFailure::Conflict(RecognizedConflict::AccountCredentialDigest)
    );

    let immutable_username =
        sqlx::query("UPDATE accounts SET username = 'changed_name' WHERE id = $1")
            .bind(first_id)
            .execute(pool)
            .await
            .expect_err("username identity is immutable through database writes");
    assert_eq!(
        constraint_name(&immutable_username),
        Some("accounts_identity_immutable")
    );

    let invalid_username = insert_account(pool, Uuid::from_u128(4), "Alice", "user", &[4; 32])
        .await
        .expect_err("username syntax is enforced in PostgreSQL");
    assert_eq!(
        constraint_name(&invalid_username),
        Some("accounts_username_syntax_check")
    );
    let invalid_role = insert_account(pool, Uuid::from_u128(5), "valid_user", "owner", &[5; 32])
        .await
        .expect_err("only documented roles are accepted");
    assert_eq!(constraint_name(&invalid_role), Some("accounts_role_check"));

    let bad_digest = sqlx::query(
        "INSERT INTO accounts (id, username, role, enabled, api_key_digest) \
         VALUES ($1, 'short_digest', 'user', true, $2)",
    )
    .bind(Uuid::from_u128(6))
    .bind(vec![6_u8; 31])
    .execute(pool)
    .await
    .expect_err("account credential digest must be exactly 32 bytes");
    assert_eq!(
        constraint_name(&bad_digest),
        Some("accounts_api_key_digest_length_check")
    );
    let null_enabled = sqlx::query(
        "INSERT INTO accounts (id, username, role, enabled, api_key_digest) \
         VALUES ($1, 'null_enabled', 'user', NULL, $2)",
    )
    .bind(Uuid::from_u128(7))
    .bind(vec![7_u8; 32])
    .execute(pool)
    .await
    .expect_err("enabled is NOT NULL; CHECK NULL semantics cannot admit it");
    assert_eq!(sqlstate(&null_enabled).as_deref(), Some("23502"));

    let reservation_conflict =
        sqlx::query("INSERT INTO code_reservations (short_code) VALUES ('api')")
            .execute(pool)
            .await
            .expect_err("the seeded API namespace remains reserved");
    assert_eq!(
        classify(&reservation_conflict, OperationStage::BeforeCommit),
        DatabaseFailure::Conflict(RecognizedConflict::CodeReservation)
    );
    for code in ["assets", "health", "api-sale"] {
        sqlx::query("INSERT INTO code_reservations (short_code) VALUES ($1)")
            .bind(code)
            .execute(pool)
            .await
            .expect("assets, health, and api-sale are not migration reservations");
    }

    let invalid_reservation =
        sqlx::query("INSERT INTO code_reservations (short_code) VALUES ('BadCode')")
            .execute(pool)
            .await
            .expect_err("reservation syntax matches the immutable short-code contract");
    assert_eq!(
        constraint_name(&invalid_reservation),
        Some("code_reservations_short_code_syntax_check")
    );

    sqlx::query("UPDATE system_state SET bootstrap_completed = true")
        .execute(pool)
        .await
        .expect_err("bootstrap cannot complete before an enabled admin exists");
    let admin_id = Uuid::from_u128(8);
    insert_account(pool, admin_id, "initial_admin", "admin", &[8; 32])
        .await
        .expect("test fixture creates the admin explicitly, not the migration");
    sqlx::query("UPDATE system_state SET bootstrap_completed = true")
        .execute(pool)
        .await
        .expect("bootstrap state can advance after an enabled admin is committed");
    let rollback_bootstrap = sqlx::query("UPDATE system_state SET bootstrap_completed = false")
        .execute(pool)
        .await
        .expect_err("bootstrap completion cannot be reversed");
    assert_eq!(
        constraint_name(&rollback_bootstrap),
        Some("system_state_transition_check")
    );
    assert_eq!(
        sqlx::query("DELETE FROM system_state")
            .execute(pool)
            .await
            .expect_err("the singleton bootstrap state cannot be deleted")
            .as_database_error()
            .and_then(|error| error.constraint()),
        Some("system_state_singleton_immutable")
    );

    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(first_id)
        .execute(pool)
        .await
        .expect("a deleted username can be reused");
    let replacement_id = Uuid::from_u128(9);
    insert_account(pool, replacement_id, "alice_1", "user", &[9; 32])
        .await
        .expect("recreated username has a new account UUID");
    assert_ne!(first_id, replacement_id);
    database.close().await;
}

#[tokio::test]
async fn link_ownership_digest_counter_and_immutable_identity_constraints_hold() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    migrate(&database).await;
    let pool = database.pool();
    let owner_id = Uuid::from_u128(101);
    insert_account(pool, owner_id, "link_owner", "user", &[11; 32])
        .await
        .expect("link owner fixture inserts");
    for code in [
        "owned-one",
        "anonymous-one",
        "anonymous-two",
        "bad-digest",
        "negative-count",
        "orphan-link",
    ] {
        sqlx::query("INSERT INTO code_reservations (short_code) VALUES ($1)")
            .bind(code)
            .execute(pool)
            .await
            .expect("link fixture has its reservation");
    }

    insert_link(
        pool,
        Uuid::from_u128(201),
        "owned-one",
        Some(owner_id),
        None,
        2,
    )
    .await
    .expect("owned link has owner and no management-token digest");
    insert_link(
        pool,
        Uuid::from_u128(202),
        "anonymous-one",
        None,
        Some(vec![22; 32]),
        0,
    )
    .await
    .expect("anonymous link has a 32-byte management-token digest");

    let duplicate_code = insert_link(
        pool,
        Uuid::from_u128(209),
        "owned-one",
        Some(owner_id),
        None,
        0,
    )
    .await
    .expect_err("globally unique code constraint prevents a second live link");
    assert_eq!(
        classify(&duplicate_code, OperationStage::BeforeCommit),
        DatabaseFailure::Conflict(RecognizedConflict::ShortCode)
    );

    for (id, code, owner, token) in [
        (Uuid::from_u128(203), "anonymous-two", None, None),
        (Uuid::from_u128(204), "bad-digest", None, Some(vec![24; 31])),
    ] {
        let error = insert_link(pool, id, code, owner, token, 0)
            .await
            .expect_err("invalid ownership/token shape is rejected");
        assert_eq!(
            constraint_name(&error),
            Some(if id == Uuid::from_u128(203) {
                "links_ownership_mode_check"
            } else {
                "links_management_token_digest_length_check"
            })
        );
    }

    let owned_with_token = insert_link(
        pool,
        Uuid::from_u128(205),
        "anonymous-two",
        Some(owner_id),
        Some(vec![25; 32]),
        0,
    )
    .await
    .expect_err("owned link cannot also carry an anonymous management digest");
    assert_eq!(
        constraint_name(&owned_with_token),
        Some("links_ownership_mode_check")
    );
    let missing_reservation = insert_link(
        pool,
        Uuid::from_u128(206),
        "not-reserved",
        Some(owner_id),
        None,
        0,
    )
    .await
    .expect_err("every live link references a permanent reservation");
    assert_eq!(
        constraint_name(&missing_reservation),
        Some("links_code_reservation_fk")
    );
    let negative_count = insert_link(
        pool,
        Uuid::from_u128(207),
        "negative-count",
        Some(owner_id),
        None,
        -1,
    )
    .await
    .expect_err("visit_count cannot be negative");
    assert_eq!(
        constraint_name(&negative_count),
        Some("links_visit_count_nonnegative_check")
    );

    let title_mismatch =
        sqlx::query("UPDATE links SET title = 'title without search value' WHERE id = $1")
            .bind(Uuid::from_u128(201))
            .execute(pool)
            .await
            .expect_err("nullable title and title-search columns stay paired");
    assert_eq!(
        constraint_name(&title_mismatch),
        Some("links_title_search_consistency_check")
    );

    let owner_change = sqlx::query("UPDATE links SET owner_id = NULL WHERE id = $1")
        .bind(Uuid::from_u128(201))
        .execute(pool)
        .await
        .expect_err("link ownership is immutable");
    assert_eq!(
        constraint_name(&owner_change),
        Some("links_identity_immutable")
    );
    let code_change = sqlx::query("UPDATE links SET short_code = 'anonymous-two' WHERE id = $1")
        .bind(Uuid::from_u128(201))
        .execute(pool)
        .await
        .expect_err("short codes are immutable");
    assert_eq!(
        constraint_name(&code_change),
        Some("links_identity_immutable")
    );

    let link_updated_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT updated_at FROM links WHERE id = $1")
            .bind(Uuid::from_u128(201))
            .fetch_one(pool)
            .await
            .expect("link metadata timestamp exists");
    sqlx::query("UPDATE links SET visit_count = visit_count + 1 WHERE id = $1")
        .bind(Uuid::from_u128(201))
        .execute(pool)
        .await
        .expect("analytics counter can be changed independently");
    let unchanged_updated_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT updated_at FROM links WHERE id = $1")
            .bind(Uuid::from_u128(201))
            .fetch_one(pool)
            .await
            .expect("updated timestamp remains present");
    assert_eq!(link_updated_at, unchanged_updated_at);

    let account_bucket = Uuid::from_u128(301);
    sqlx::query(
        "INSERT INTO rate_buckets (id, bucket_kind, account_id) VALUES ($1, 'account', $2)",
    )
    .bind(account_bucket)
    .bind(Uuid::from_u128(999))
    .execute(pool)
    .await
    .expect("rate bucket identity has no account ownership cascade");
    let duplicate_token = insert_link(
        pool,
        Uuid::from_u128(208),
        "negative-count",
        None,
        Some(vec![22; 32]),
        0,
    )
    .await
    .expect_err("management-token digests are globally unique");
    assert_eq!(
        classify(&duplicate_token, OperationStage::BeforeCommit),
        DatabaseFailure::Conflict(RecognizedConflict::ManagementTokenDigest)
    );

    assert_eq!(
        sqlx::query("DELETE FROM code_reservations WHERE short_code = 'owned-one'")
            .execute(pool)
            .await
            .expect_err("reservation rows are permanent")
            .as_database_error()
            .and_then(|error| error.constraint()),
        Some("code_reservations_permanent")
    );
    database.close().await;
}

#[tokio::test]
async fn link_and_account_deletion_cascade_children_but_rollback_is_atomic() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    migrate(&database).await;
    let pool = database.pool();
    let owner_id = Uuid::from_u128(401);
    let link_id = Uuid::from_u128(402);
    insert_account(pool, owner_id, "delete_owner", "user", &[41; 32])
        .await
        .expect("account fixture inserts");
    sqlx::query("INSERT INTO code_reservations (short_code) VALUES ('delete-link')")
        .execute(pool)
        .await
        .expect("code reservation inserts");
    insert_link(pool, link_id, "delete-link", Some(owner_id), None, 1)
        .await
        .expect("owned link inserts");
    sqlx::query("INSERT INTO link_tags (link_id, name) VALUES ($1, 'persistent-tag')")
        .bind(link_id)
        .execute(pool)
        .await
        .expect("tag association inserts");
    insert_visit(
        pool,
        Uuid::from_u128(403),
        link_id,
        "https://snapshot.example/a",
    )
    .await
    .expect("visit snapshot inserts");

    let mut transaction = pool.begin().await.expect("deletion transaction begins");
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(owner_id)
        .execute(&mut *transaction)
        .await
        .expect("account delete cascades owned links and visits in the transaction");
    let inside_transaction: (i64, i64, i64) = sqlx::query_as(
        "SELECT \
          (SELECT count(*) FROM accounts WHERE id = $1), \
          (SELECT count(*) FROM links WHERE id = $2), \
          (SELECT count(*) FROM visits WHERE link_id = $2)",
    )
    .bind(owner_id)
    .bind(link_id)
    .fetch_one(&mut *transaction)
    .await
    .expect("logical deletion is inspectable before commit");
    assert_eq!(inside_transaction, (0, 0, 0));
    transaction.rollback().await.expect("rollback succeeds");

    assert_eq!(row_count(pool, "accounts", "id", owner_id).await, 1);
    assert_eq!(row_count(pool, "links", "id", link_id).await, 1);
    assert_eq!(row_count(pool, "visits", "link_id", link_id).await, 1);
    assert_eq!(row_count(pool, "link_tags", "link_id", link_id).await, 1);

    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(owner_id)
        .execute(pool)
        .await
        .expect("account deletion commits and cascades its live links");
    assert_eq!(row_count(pool, "accounts", "id", owner_id).await, 0);
    assert_eq!(row_count(pool, "links", "id", link_id).await, 0);
    assert_eq!(row_count(pool, "link_tags", "link_id", link_id).await, 0);
    assert_eq!(row_count(pool, "visits", "link_id", link_id).await, 0);
    assert_eq!(
        row_count(pool, "code_reservations", "short_code", "delete-link").await,
        1
    );
    let reservation_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = 'code_reservations'",
    )
    .fetch_all(pool)
    .await
    .expect("reservation metadata can be checked after deletion");
    assert_eq!(reservation_columns, ["short_code"]);

    let new_owner = Uuid::from_u128(404);
    insert_account(pool, new_owner, "delete_owner", "user", &[44; 32])
        .await
        .expect("deleted account username can be recreated with a new UUID");
    assert_ne!(owner_id, new_owner);
    assert_eq!(
        row_count(pool, "code_reservations", "short_code", "delete-link").await,
        1
    );
    database.close().await;
}

#[tokio::test]
async fn visits_tags_and_rate_accounting_have_authoritative_shapes_and_deletion_rules() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    migrate(&database).await;
    let pool = database.pool();
    let link_id = Uuid::from_u128(501);
    sqlx::query("INSERT INTO code_reservations (short_code) VALUES ('visit-link')")
        .execute(pool)
        .await
        .expect("link reservation inserts");
    insert_link(pool, link_id, "visit-link", None, Some(vec![51; 32]), 0)
        .await
        .expect("anonymous link fixture inserts");

    insert_visit(
        pool,
        Uuid::from_u128(502),
        link_id,
        "https://snapshot.example/path?q=1#frag",
    )
    .await
    .expect("GET/HEAD visit with optional null headers is valid");
    let duplicate_event_id = insert_visit(
        pool,
        Uuid::from_u128(502),
        link_id,
        "https://snapshot.example/different-event-payload",
    )
    .await
    .expect_err("visit event UUID is globally unique for persistence idempotency");
    assert_eq!(sqlstate(&duplicate_event_id).as_deref(), Some("23505"));
    let bad_method = sqlx::query(
        "INSERT INTO visits (id, link_id, redirect_url, occurred_at, method, client_ip, device, browser, os) \
         VALUES ($1, $2, 'https://snapshot.example/', clock_timestamp(), 'POST', '192.0.2.2', 'unknown', 'unknown', 'unknown')",
    )
    .bind(Uuid::from_u128(503))
    .bind(link_id)
    .execute(pool)
    .await
    .expect_err("visit method is limited to GET and HEAD");
    assert_eq!(constraint_name(&bad_method), Some("visits_method_check"));
    let bad_device = sqlx::query(
        "INSERT INTO visits (id, link_id, redirect_url, occurred_at, method, client_ip, device, browser, os) \
         VALUES ($1, $2, 'https://snapshot.example/', clock_timestamp(), 'GET', '192.0.2.2', 'robot', 'unknown', 'unknown')",
    )
    .bind(Uuid::from_u128(504))
    .bind(link_id)
    .execute(pool)
    .await
    .expect_err("device enum is constrained");
    assert_eq!(constraint_name(&bad_device), Some("visits_device_check"));
    let bad_mask = sqlx::query(
        "INSERT INTO visits (id, link_id, redirect_url, occurred_at, method, client_ip, device, browser, os) \
         VALUES ($1, $2, 'https://snapshot.example/', clock_timestamp(), 'GET', '192.0.2.0/24', 'unknown', 'unknown', 'unknown')",
    )
    .bind(Uuid::from_u128(505))
    .bind(link_id)
    .execute(pool)
    .await
    .expect_err("inet visit identity must preserve one host address, not a network");
    assert_eq!(
        constraint_name(&bad_mask),
        Some("visits_client_ip_host_check")
    );
    let orphan_visit = sqlx::query(
        "INSERT INTO visits (id, link_id, redirect_url, occurred_at, method, client_ip, device, browser, os) \
         VALUES ($1, $2, 'https://snapshot.example/', clock_timestamp(), 'GET', '192.0.2.2', 'unknown', 'unknown', 'unknown')",
    )
    .bind(Uuid::from_u128(506))
    .bind(Uuid::from_u128(507))
    .execute(pool)
    .await
    .expect_err("visit must reference a live link");
    assert_eq!(constraint_name(&orphan_visit), Some("visits_link_fk"));
    let snapshot_update =
        sqlx::query("UPDATE visits SET redirect_url = 'https://changed.example/' WHERE id = $1")
            .bind(Uuid::from_u128(502))
            .execute(pool)
            .await
            .expect_err("redirect destination snapshots are immutable");
    assert_eq!(constraint_name(&snapshot_update), Some("visits_immutable"));

    let orphan_tag = sqlx::query("INSERT INTO link_tags (link_id, name) VALUES ($1, 'rust')")
        .bind(Uuid::from_u128(508))
        .execute(pool)
        .await
        .expect_err("tag associations cannot be orphaned");
    assert_eq!(constraint_name(&orphan_tag), Some("link_tags_link_fk"));
    sqlx::query("INSERT INTO link_tags (link_id, name) VALUES ($1, 'rust')")
        .bind(link_id)
        .execute(pool)
        .await
        .expect("canonical name associations insert");
    let duplicate_tag = sqlx::query("INSERT INTO link_tags (link_id, name) VALUES ($1, 'rust')")
        .bind(link_id)
        .execute(pool)
        .await
        .expect_err("composite key prevents duplicate canonical associations");
    assert_eq!(sqlstate(&duplicate_tag).as_deref(), Some("23505"));

    let account_bucket = Uuid::from_u128(509);
    let bucket_account_id = Uuid::from_u128(510);
    sqlx::query(
        "INSERT INTO rate_buckets (id, bucket_kind, account_id) VALUES ($1, 'account', $2)",
    )
    .bind(account_bucket)
    .bind(bucket_account_id)
    .execute(pool)
    .await
    .expect("account bucket has a stable typed identity");
    let duplicate_account_bucket = sqlx::query(
        "INSERT INTO rate_buckets (id, bucket_kind, account_id) VALUES ($1, 'account', $2)",
    )
    .bind(Uuid::from_u128(514))
    .bind(bucket_account_id)
    .execute(pool)
    .await
    .expect_err("account UUID, not credential material, uniquely identifies its bucket");
    assert_eq!(
        constraint_name(&duplicate_account_bucket),
        Some("rate_buckets_account_identity_unique")
    );
    sqlx::query(
        "INSERT INTO rate_buckets (id, bucket_kind, client_ip) \
         VALUES ($1, 'anonymous_ip', '192.0.2.77')",
    )
    .bind(Uuid::from_u128(515))
    .execute(pool)
    .await
    .expect("anonymous host IP bucket inserts");
    let duplicate_ip_bucket = sqlx::query(
        "INSERT INTO rate_buckets (id, bucket_kind, client_ip) \
         VALUES ($1, 'anonymous_ip', '192.0.2.77')",
    )
    .bind(Uuid::from_u128(516))
    .execute(pool)
    .await
    .expect_err("canonical host IP uniquely identifies its anonymous bucket");
    assert_eq!(
        constraint_name(&duplicate_ip_bucket),
        Some("rate_buckets_client_ip_identity_unique")
    );
    let charge_id = Uuid::from_u128(511);
    sqlx::query("INSERT INTO rate_charges (id, bucket_id) VALUES ($1, $2)")
        .bind(charge_id)
        .bind(account_bucket)
        .execute(pool)
        .await
        .expect("charge gets its database timestamp");
    let charged_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT charged_at FROM rate_charges WHERE id = $1")
            .bind(charge_id)
            .fetch_one(pool)
            .await
            .expect("database timestamp is UTC-capable");
    assert!(charged_at.timestamp() > 0);

    let invalid_bucket = sqlx::query(
        "INSERT INTO rate_buckets (id, bucket_kind, account_id, client_ip) \
         VALUES ($1, 'account', $2, '192.0.2.1')",
    )
    .bind(Uuid::from_u128(512))
    .bind(Uuid::from_u128(513))
    .execute(pool)
    .await
    .expect_err("typed bucket identity cannot combine account and IP fields");
    assert_eq!(
        constraint_name(&invalid_bucket),
        Some("rate_buckets_identity_check")
    );

    // Account bucket identities are not FK-owned by accounts, so their charges
    // remain durable through account deletion and credential rotation.
    insert_account(pool, bucket_account_id, "rate_owner", "user", &[52; 32])
        .await
        .expect("bucket's account fixture inserts");
    sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(bucket_account_id)
        .execute(pool)
        .await
        .expect("deleting an account does not erase its rate history");
    assert_eq!(row_count(pool, "rate_charges", "id", charge_id).await, 1);

    sqlx::query("DELETE FROM links WHERE id = $1")
        .bind(link_id)
        .execute(pool)
        .await
        .expect("link deletion cascades tags and visits without deleting its code reservation");
    assert_eq!(row_count(pool, "link_tags", "link_id", link_id).await, 0);
    assert_eq!(row_count(pool, "visits", "link_id", link_id).await, 0);
    assert_eq!(
        row_count(pool, "code_reservations", "short_code", "visit-link").await,
        1
    );

    // The Rust IP parser used by later persistence callers preserves host
    // semantics and canonical address identity independently of URL strings.
    assert_eq!(
        IpAddr::from_str("192.0.2.10")
            .expect("valid test IP")
            .to_string(),
        "192.0.2.10"
    );
    database.close().await;
}

async fn migrate(database: &IsolatedTestDatabase) {
    let config =
        ferl_server::config::MigrationConfig::from_map(std::collections::BTreeMap::from([(
            "FERL_DATABASE_URL".to_owned(),
            database.schema_database_url().to_owned(),
        )]))
        .expect("valid isolated migration configuration");
    ferl_server::db::migrate::run_migrations(&config)
        .await
        .expect("isolated PostgreSQL fixture migrates");
}

async fn insert_account(
    pool: &PgPool,
    id: Uuid,
    username: &str,
    role: &str,
    digest: &[u8],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO accounts (id, username, role, enabled, api_key_digest) \
         VALUES ($1, $2, $3, true, $4)",
    )
    .bind(id)
    .bind(username)
    .bind(role)
    .bind(digest)
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_link(
    pool: &PgPool,
    id: Uuid,
    code: &str,
    owner: Option<Uuid>,
    token_digest: Option<Vec<u8>>,
    visit_count: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO links \
         (id, short_code, owner_id, destination_url, destination_search, title, title_search, \
          enabled, visit_count, management_token_digest) \
         VALUES ($1, $2, $3, 'https://destination.example/path', 'https://destination.example/path', \
                 NULL, NULL, true, $4, $5)",
    )
    .bind(id)
    .bind(code)
    .bind(owner)
    .bind(visit_count)
    .bind(token_digest)
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_visit(
    pool: &PgPool,
    id: Uuid,
    link_id: Uuid,
    redirect_url: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO visits \
         (id, link_id, redirect_url, occurred_at, method, client_ip, referrer, user_agent, device, browser, os) \
         VALUES ($1, $2, $3, clock_timestamp(), 'HEAD', '2001:db8::10', NULL, NULL, \
                 'unknown', 'unknown', 'unknown')",
    )
    .bind(id)
    .bind(link_id)
    .bind(redirect_url)
    .execute(pool)
    .await?;
    Ok(())
}

async fn row_count<T>(pool: &PgPool, table: &str, column: &str, value: T) -> i64
where
    T: for<'q> sqlx::Encode<'q, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send,
{
    let query = format!("SELECT count(*) FROM {table} WHERE {column} = $1");
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(query))
        .bind(value)
        .fetch_one(pool)
        .await
        .expect("test-owned row count query succeeds")
}

fn constraint_name(error: &sqlx::Error) -> Option<&str> {
    error
        .as_database_error()
        .and_then(|error| error.constraint())
}

fn sqlstate(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .map(|code| code.into_owned())
}
