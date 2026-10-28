//! Scope-correct tag counts computed over the complete authorized live-link set.

use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::{
    http::query::{OwnerFilter, Scope, TagDiscoveryQuery},
    links::list_repository,
    pagination::CursorBoundary,
};

#[derive(FromRow)]
pub(crate) struct TagDiscoveryRecord {
    pub name: String,
    pub link_count: i64,
}

pub(crate) async fn list(
    pool: &PgPool,
    actor_id: Uuid,
    query: &TagDiscoveryQuery,
    boundary: Option<&CursorBoundary>,
) -> Result<Vec<TagDiscoveryRecord>, sqlx::Error> {
    build_query(actor_id, query, boundary, false)
        .build_query_as::<TagDiscoveryRecord>()
        .fetch_all(pool)
        .await
}

fn build_query(
    actor_id: Uuid,
    query: &TagDiscoveryQuery,
    boundary: Option<&CursorBoundary>,
    explain: bool,
) -> QueryBuilder<Postgres> {
    let prefix = if explain {
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) "
    } else {
        ""
    };
    let mut sql = QueryBuilder::<Postgres>::new(prefix);
    sql.push(
        "WITH grouped AS (SELECT lt.name, count(DISTINCT lt.link_id)::bigint AS link_count \
         FROM link_tags lt JOIN links l ON l.id = lt.link_id WHERE ",
    );
    match query.scope() {
        Scope::Own => {
            sql.push("l.owner_id = ").push_bind(actor_id);
        }
        Scope::All => match query.owner() {
            Some(OwnerFilter::Account(owner)) => {
                sql.push("l.owner_id = ").push_bind(owner.as_uuid());
            }
            Some(OwnerFilter::Anonymous) => {
                sql.push("l.owner_id IS NULL");
            }
            None => {
                sql.push("TRUE");
            }
        },
    }
    if let Some(search) = query.search() {
        sql.push(" AND lt.name LIKE ")
            .push_bind(list_repository::literal_pattern(search.as_str()))
            .push(" ESCAPE '!'");
    }
    sql.push(
        " GROUP BY lt.name HAVING count(DISTINCT lt.link_id) > 0) \
             SELECT name, link_count FROM grouped WHERE TRUE",
    );
    if let Some(CursorBoundary::TagName { name }) = boundary {
        sql.push(" AND name COLLATE \"C\" > ")
            .push_bind(name)
            .push(" COLLATE \"C\"");
    } else if boundary.is_some() {
        unreachable!("cursor codec rejects a non-tag boundary for tag discovery");
    }
    sql.push(" ORDER BY name COLLATE \"C\" ASC LIMIT ")
        .push_bind(i64::from(query.page().limit()) + 1);
    sql
}

#[cfg(test)]
pub(crate) async fn explain(
    pool: &PgPool,
    actor_id: Uuid,
    query: &TagDiscoveryQuery,
    boundary: Option<&CursorBoundary>,
) -> Result<serde_json::Value, sqlx::Error> {
    use sqlx::Row;

    let row = build_query(actor_id, query, boundary, true)
        .build()
        .fetch_one(pool)
        .await?;
    let plan: sqlx::types::Json<serde_json::Value> = row.try_get(0)?;
    Ok(plan.0)
}
