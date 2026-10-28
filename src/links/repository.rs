//! Link SQL. Transaction-taking operations use the caller's connection and do
//! not check out another pool connection.

use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    db::records::{DatabaseTimestamp, LinkRecord},
    domain::ids::LinkId,
};

#[derive(FromRow)]
pub(crate) struct LinkSnapshotRecord {
    pub id: Uuid,
    pub short_code: String,
    pub owner_id: Option<Uuid>,
    pub destination_url: String,
    pub destination_search: String,
    pub title: Option<String>,
    pub title_search: Option<String>,
    pub enabled: bool,
    pub visit_count: i64,
    pub management_token_digest: Option<Vec<u8>>,
    pub created_at: DatabaseTimestamp,
    pub updated_at: DatabaseTimestamp,
    pub owner_enabled: Option<bool>,
    pub tag_names: Vec<String>,
}

pub(crate) struct NewLink<'a> {
    pub id: Uuid,
    pub code: &'a str,
    pub owner_id: Option<Uuid>,
    pub destination_url: &'a str,
    pub destination_search: &'a str,
    pub title: Option<&'a str>,
    pub title_search: Option<&'a str>,
    pub token_digest: Option<&'a [u8; 32]>,
}

impl LinkSnapshotRecord {
    pub fn into_parts(self) -> (LinkRecord, Vec<String>, Option<bool>) {
        (
            LinkRecord {
                id: self.id,
                short_code: self.short_code,
                owner_id: self.owner_id,
                destination_url: self.destination_url,
                destination_search: self.destination_search,
                title: self.title,
                title_search: self.title_search,
                enabled: self.enabled,
                visit_count: self.visit_count,
                management_token_digest: self.management_token_digest,
                created_at: self.created_at,
                updated_at: self.updated_at,
            },
            self.tag_names,
            self.owner_enabled,
        )
    }
}

pub(crate) async fn reserve_code(
    connection: &mut PgConnection,
    code: &str,
) -> Result<bool, sqlx::Error> {
    let inserted: Option<String> = sqlx::query_scalar(
        "INSERT INTO code_reservations (short_code) VALUES ($1) \
         ON CONFLICT (short_code) DO NOTHING RETURNING short_code",
    )
    .bind(code)
    .fetch_optional(&mut *connection)
    .await?;
    Ok(inserted.is_some())
}

pub(crate) async fn insert_link(
    connection: &mut PgConnection,
    link: NewLink<'_>,
) -> Result<LinkRecord, sqlx::Error> {
    sqlx::query_as::<_, LinkRecord>(
        "INSERT INTO links \
         (id, short_code, owner_id, destination_url, destination_search, title, title_search, \
          enabled, visit_count, management_token_digest) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, true, 0, $8) \
         RETURNING id, short_code, owner_id, destination_url, destination_search, title, \
                   title_search, enabled, visit_count, management_token_digest, created_at, updated_at",
    )
    .bind(link.id)
    .bind(link.code)
    .bind(link.owner_id)
    .bind(link.destination_url)
    .bind(link.destination_search)
    .bind(link.title)
    .bind(link.title_search)
    .bind(link.token_digest.map(<[u8; 32]>::as_slice))
    .fetch_one(&mut *connection)
    .await
}

pub(crate) async fn replace_tags(
    connection: &mut PgConnection,
    link_id: LinkId,
    names: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM link_tags WHERE link_id = $1")
        .bind(link_id.as_uuid())
        .execute(&mut *connection)
        .await?;
    if !names.is_empty() {
        sqlx::query(
            "INSERT INTO link_tags (link_id, name) \
             SELECT $1, name FROM unnest($2::text[]) AS names(name)",
        )
        .bind(link_id.as_uuid())
        .bind(names)
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

pub(crate) async fn find_snapshot_by_code(
    pool: &PgPool,
    code: &str,
) -> Result<Option<LinkSnapshotRecord>, sqlx::Error> {
    sqlx::query_as::<_, LinkSnapshotRecord>(SNAPSHOT_SELECT)
        .bind(code)
        .fetch_optional(pool)
        .await
}

pub(crate) async fn find_snapshot_by_id(
    connection: &mut PgConnection,
    link_id: LinkId,
) -> Result<Option<LinkSnapshotRecord>, sqlx::Error> {
    sqlx::query_as::<_, LinkSnapshotRecord>(SNAPSHOT_SELECT_BY_ID)
        .bind(link_id.as_uuid())
        .fetch_optional(&mut *connection)
        .await
}

pub(crate) async fn lock_by_code(
    connection: &mut PgConnection,
    code: &str,
) -> Result<Option<LinkRecord>, sqlx::Error> {
    sqlx::query_as::<_, LinkRecord>(
        "SELECT id, short_code, owner_id, destination_url, destination_search, title, \
                title_search, enabled, visit_count, management_token_digest, created_at, updated_at \
         FROM links WHERE short_code = $1 FOR UPDATE",
    )
    .bind(code)
    .fetch_optional(&mut *connection)
    .await
}

pub(crate) async fn update_metadata(
    connection: &mut PgConnection,
    current: &LinkRecord,
    destination_url: &str,
    destination_search: &str,
    title: Option<&str>,
    title_search: Option<&str>,
    enabled: bool,
) -> Result<LinkRecord, sqlx::Error> {
    sqlx::query_as::<_, LinkRecord>(
        "UPDATE links SET destination_url = $2, destination_search = $3, title = $4, \
                title_search = $5, enabled = $6, updated_at = clock_timestamp() \
         WHERE id = $1 \
         RETURNING id, short_code, owner_id, destination_url, destination_search, title, \
                   title_search, enabled, visit_count, management_token_digest, created_at, updated_at",
    )
    .bind(current.id)
    .bind(destination_url)
    .bind(destination_search)
    .bind(title)
    .bind(title_search)
    .bind(enabled)
    .fetch_one(&mut *connection)
    .await
}

pub(crate) async fn delete(
    connection: &mut PgConnection,
    link_id: LinkId,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM links WHERE id = $1")
        .bind(link_id.as_uuid())
        .execute(&mut *connection)
        .await?
        .rows_affected())
}

pub(crate) async fn replace_token_digest(
    connection: &mut PgConnection,
    link_id: LinkId,
    digest: &[u8; 32],
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE links SET management_token_digest = $2 WHERE id = $1 AND owner_id IS NULL",
    )
    .bind(link_id.as_uuid())
    .bind(digest.as_slice())
    .execute(&mut *connection)
    .await?
    .rows_affected())
}

pub(crate) async fn active_resolution(
    pool: &PgPool,
    code: &str,
) -> Result<Option<sqlx::postgres::PgRow>, sqlx::Error> {
    sqlx::query(
        "SELECT l.id, l.short_code, l.destination_url, l.enabled, l.owner_id, \
                l.management_token_digest IS NULL AS token_digest_absent, \
                (l.management_token_digest IS NOT NULL AND octet_length(l.management_token_digest) = 32) AS token_digest_valid, \
                a.id AS owner_row_id, a.enabled AS owner_enabled \
         FROM links l LEFT JOIN accounts a ON a.id = l.owner_id \
         WHERE l.short_code = $1",
    )
    .bind(code)
    .fetch_optional(pool)
    .await
}

const SNAPSHOT_SELECT: &str = "SELECT \
    l.id, l.short_code, l.owner_id, l.destination_url, l.destination_search, l.title, \
    l.title_search, l.enabled, l.visit_count, l.management_token_digest, l.created_at, l.updated_at, \
    a.enabled AS owner_enabled, \
    ARRAY(SELECT t.name FROM link_tags t WHERE t.link_id = l.id ORDER BY t.name COLLATE \"C\") AS tag_names \
    FROM links l LEFT JOIN accounts a ON a.id = l.owner_id WHERE l.short_code = $1";

const SNAPSHOT_SELECT_BY_ID: &str = "SELECT \
    l.id, l.short_code, l.owner_id, l.destination_url, l.destination_search, l.title, \
    l.title_search, l.enabled, l.visit_count, l.management_token_digest, l.created_at, l.updated_at, \
    a.enabled AS owner_enabled, \
    ARRAY(SELECT t.name FROM link_tags t WHERE t.link_id = l.id ORDER BY t.name COLLATE \"C\") AS tag_names \
    FROM links l LEFT JOIN accounts a ON a.id = l.owner_id WHERE l.id = $1";
