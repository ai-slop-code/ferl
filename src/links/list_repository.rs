//! Bounded, one-statement link listing. Candidate filters, cursor boundary,
//! ordering, tags and owner state share one PostgreSQL statement snapshot.

use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::{
    db::records::DatabaseTimestamp,
    domain::time::Timestamp,
    http::query::{LinkListQuery, LinkSort, OwnerFilter, Scope, SortOrder, TagMode},
    pagination::CursorBoundary,
};

use super::repository::LinkSnapshotRecord;

const ESCAPE_CHAR: char = '!';

pub(crate) async fn list(
    pool: &PgPool,
    actor_id: Uuid,
    query: &LinkListQuery,
    boundary: Option<&CursorBoundary>,
) -> Result<Vec<LinkSnapshotRecord>, sqlx::Error> {
    build_query(actor_id, query, boundary, false)
        .build_query_as::<LinkSnapshotRecord>()
        .fetch_all(pool)
        .await
}

fn build_query(
    actor_id: Uuid,
    query: &LinkListQuery,
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
        "SELECT l.id, l.short_code, l.owner_id, l.destination_url, \
         l.destination_search, l.title, l.title_search, l.enabled, l.visit_count, \
         l.management_token_digest, l.created_at, l.updated_at, a.enabled AS owner_enabled, \
         ARRAY(SELECT t.name FROM link_tags t WHERE t.link_id = l.id \
               ORDER BY t.name COLLATE \"C\") AS tag_names \
         FROM links l LEFT JOIN accounts a ON a.id = l.owner_id WHERE ",
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
    };

    if let Some(search) = query.search() {
        let pattern = literal_pattern(search.as_str());
        sql.push(" AND (l.destination_search LIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '!' OR l.short_code LIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '!' OR l.title_search LIKE ")
            .push_bind(pattern)
            .push(" ESCAPE '!')");
    }
    if let Some(enabled) = query.enabled() {
        sql.push(" AND l.enabled = ").push_bind(enabled);
    }
    if let Some(from) = query.created().from() {
        sql.push(" AND l.created_at >= ").push_bind(from.as_utc());
    }
    if let Some(to) = query.created().to() {
        sql.push(" AND l.created_at < ").push_bind(to.as_utc());
    }
    if !query.tags().as_slice().is_empty() {
        let tags = query
            .tags()
            .as_slice()
            .iter()
            .map(|tag| tag.as_str().to_owned())
            .collect::<Vec<_>>();
        match query.tag_mode().unwrap_or(TagMode::Any) {
            TagMode::Any => {
                sql.push(
                    " AND EXISTS (SELECT 1 FROM link_tags tf \
                          WHERE tf.link_id = l.id AND tf.name = ANY(",
                )
                .push_bind(tags)
                .push("::text[]))");
            }
            TagMode::All => {
                let expected = tags.len() as i64;
                sql.push(
                    " AND (SELECT count(*) FROM link_tags tf \
                          WHERE tf.link_id = l.id AND tf.name = ANY(",
                )
                .push_bind(tags)
                .push("::text[])) = ")
                .push_bind(expected);
            }
        }
    }

    if let Some(boundary) = boundary {
        push_boundary(&mut sql, query.sort(), query.order(), boundary);
    }

    sql.push(" ORDER BY ");
    match (query.sort(), query.order()) {
        (LinkSort::CreatedAt, SortOrder::Asc) => sql.push("l.created_at ASC, l.id ASC"),
        (LinkSort::CreatedAt, SortOrder::Desc) => sql.push("l.created_at DESC, l.id DESC"),
        (LinkSort::Title, SortOrder::Asc) => {
            sql.push("l.title_search COLLATE \"C\" ASC NULLS LAST, l.id ASC")
        }
        (LinkSort::Title, SortOrder::Desc) => {
            sql.push("l.title_search COLLATE \"C\" DESC NULLS LAST, l.id DESC")
        }
        (LinkSort::VisitCount, SortOrder::Asc) => sql.push("l.visit_count ASC, l.id ASC"),
        (LinkSort::VisitCount, SortOrder::Desc) => sql.push("l.visit_count DESC, l.id DESC"),
    };
    sql.push(" LIMIT ")
        .push_bind(i64::from(query.page().limit()) + 1);
    sql
}

#[cfg(test)]
pub(crate) async fn explain(
    pool: &PgPool,
    actor_id: Uuid,
    query: &LinkListQuery,
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

fn push_boundary(
    sql: &mut QueryBuilder<Postgres>,
    sort: LinkSort,
    order: SortOrder,
    boundary: &CursorBoundary,
) {
    sql.push(" AND ");
    let comparison = match order {
        SortOrder::Asc => ">",
        SortOrder::Desc => "<",
    };
    match (sort, boundary) {
        (LinkSort::CreatedAt, CursorBoundary::LinkCreatedAt { at, id }) => {
            sql.push("(l.created_at, l.id) ")
                .push(comparison)
                .push(" (")
                .push_bind(at.as_utc())
                .push(", ")
                .push_bind(id.as_uuid())
                .push(")");
        }
        (LinkSort::VisitCount, CursorBoundary::LinkVisitCount { count, id }) => {
            sql.push("(l.visit_count, l.id) ")
                .push(comparison)
                .push(" (")
                .push_bind(*count)
                .push(", ")
                .push_bind(id.as_uuid())
                .push(")");
        }
        (
            LinkSort::Title,
            CursorBoundary::LinkTitle {
                normalized_title: Some(title),
                id,
            },
        ) => {
            sql.push("((")
                .push("l.title_search COLLATE \"C\" ")
                .push(comparison)
                .push(" ")
                .push_bind(title)
                .push(" COLLATE \"C\") OR (l.title_search COLLATE \"C\" = ")
                .push_bind(title)
                .push(" COLLATE \"C\" AND l.id ")
                .push(comparison)
                .push(" ")
                .push_bind(id.as_uuid())
                .push(") OR l.title_search IS NULL)");
        }
        (
            LinkSort::Title,
            CursorBoundary::LinkTitle {
                normalized_title: None,
                id,
            },
        ) => {
            sql.push("l.title_search IS NULL AND l.id ")
                .push(comparison)
                .push(" ")
                .push_bind(id.as_uuid());
        }
        _ => unreachable!("cursor codec rejects a boundary for another sort"),
    }
}

pub(crate) fn literal_pattern(value: &str) -> String {
    let mut pattern = String::with_capacity(value.len() + 2);
    pattern.push('%');
    for character in value.chars() {
        if matches!(character, ESCAPE_CHAR | '%' | '_') {
            pattern.push(ESCAPE_CHAR);
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

pub(crate) fn boundary_timestamp(timestamp: DatabaseTimestamp) -> Result<Timestamp, ()> {
    let unix_micros = timestamp
        .postgres_micros()
        .checked_add(946_684_800_000_000)
        .ok_or(())?;
    Timestamp::from_unix_micros(unix_micros).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        config::MigrationConfig,
        db::migrate,
        domain::ids::LinkId,
        http::query::{LinkListQuery, TagDiscoveryQuery},
        pagination::CursorBoundary,
    };
    use uuid::Uuid;

    use super::{boundary_timestamp, explain, list, literal_pattern};

    #[test]
    fn literal_like_pattern_escapes_its_escape_character_and_wildcards() {
        assert_eq!(literal_pattern(r"a!%_\z"), r"%a!!!%!_\z%");
        assert_eq!(literal_pattern("a/b"), "%a/b%");
    }

    #[tokio::test]
    async fn production_keyset_query_plans_cover_scope_sort_filter_and_tag_paths() {
        let database = crate::database_support::IsolatedTestDatabase::connect_from_harness().await;
        database
            .verify_owned(|pool, schema_url| {
                Box::pin(async move {
                    let config = MigrationConfig::from_map(BTreeMap::from([(
                        "FERL_DATABASE_URL".to_owned(),
                        schema_url,
                    )]))
                    .unwrap();
                    migrate::run_migrations(&config).await.unwrap();

                    let owner_a = Uuid::from_u128(0x901);
                    let owner_b = Uuid::from_u128(0x902);
                    for (id, username, digest) in [
                        (owner_a, "plan_owner_a", 0x41_u8),
                        (owner_b, "plan_owner_b", 0x42_u8),
                    ] {
                        sqlx::query(
                            "INSERT INTO accounts (id, username, role, enabled, api_key_digest) \
                             VALUES ($1, $2, 'user', true, $3)",
                        )
                        .bind(id)
                        .bind(username)
                        .bind(vec![digest; 32])
                        .execute(&pool)
                        .await
                        .unwrap();
                    }
                    sqlx::query(
                        "INSERT INTO code_reservations (short_code) \
                         SELECT 'plan-' || lpad(i::text, 6, '0') \
                         FROM generate_series(1, 20000) AS series(i)",
                    )
                    .execute(&pool)
                    .await
                    .unwrap();
                    sqlx::query(
                        "INSERT INTO links \
                         (id, short_code, owner_id, destination_url, destination_search, title, \
                          title_search, enabled, visit_count, management_token_digest, created_at) \
                         SELECT md5('plan-link:' || i::text)::uuid, \
                                'plan-' || lpad(i::text, 6, '0'), \
                                CASE WHEN i % 5 = 0 THEN $1 ELSE $2 END, \
                                'https://plan.example.test/' || i::text, \
                                'https://plan.example.test/' || i::text, \
                                CASE WHEN i % 7 = 0 THEN NULL ELSE 'title-' || lpad(i::text, 6, '0') END, \
                                CASE WHEN i % 7 = 0 THEN NULL ELSE 'title-' || lpad(i::text, 6, '0') END, \
                                i % 3 <> 0, (i % 101)::bigint, NULL, \
                                '2026-01-01T00:00:00Z'::timestamptz + i * interval '1 second' \
                         FROM generate_series(1, 20000) AS series(i)",
                    )
                    .bind(owner_a)
                    .bind(owner_b)
                    .execute(&pool)
                    .await
                    .unwrap();
                    sqlx::query(
                        "INSERT INTO link_tags (link_id, name) \
                         SELECT md5('plan-link:' || i::text)::uuid, \
                                CASE WHEN i % 2 = 0 THEN 'common' ELSE 'scope-' || i::text END \
                         FROM generate_series(1, 20000) AS series(i) \
                         UNION ALL \
                         SELECT md5('plan-link:' || i::text)::uuid, 'needle-tag' \
                         FROM generate_series(10, 20000, 10) AS series(i)",
                    )
                    .execute(&pool)
                    .await
                    .unwrap();
                    sqlx::query("ANALYZE links")
                        .execute(&pool)
                        .await
                        .unwrap();
                    sqlx::query("ANALYZE link_tags")
                        .execute(&pool)
                        .await
                        .unwrap();

                    for (scope, owner, prefix) in [
                        ("own", owner_a, "owner"),
                        ("all", owner_a, "global"),
                    ] {
                        for sort in ["created_at", "title", "visit_count"] {
                            for order in ["asc", "desc"] {
                                let expected_index = match (scope, sort, order) {
                                    ("own", "created_at", _) => "links_owner_created_at_id_idx",
                                    ("all", "created_at", _) => "links_created_at_id_idx",
                                    ("own", "visit_count", _) => "links_owner_visit_count_id_idx",
                                    ("all", "visit_count", _) => "links_visit_count_id_idx",
                                    ("own", "title", "asc") => {
                                        "links_owner_title_search_id_asc_idx"
                                    }
                                    ("own", "title", "desc") => {
                                        "links_owner_title_search_id_desc_idx"
                                    }
                                    ("all", "title", "asc") => "links_title_search_id_asc_idx",
                                    ("all", "title", "desc") => {
                                        "links_title_search_id_desc_idx"
                                    }
                                    _ => unreachable!(),
                                };
                                let query = LinkListQuery::parse(Some(&format!(
                                    "scope={scope}&sort={sort}&order={order}&limit=50"
                                )))
                                .unwrap();
                                let first_plan = explain(&pool, owner, &query, None)
                                    .await
                                    .unwrap();
                                assert_plan_uses(&first_plan, expected_index, &format!("{prefix} {sort} {order} first"));
                                print_plan(&format!("{prefix} {sort} {order} first"), &first_plan);

                                let rows = list(&pool, owner, &query, None).await.unwrap();
                                let boundary_row = rows.get(49).expect("first page has 50 results");
                                let boundary = match sort {
                                    "created_at" => CursorBoundary::LinkCreatedAt {
                                        at: boundary_timestamp(boundary_row.created_at).unwrap(),
                                        id: LinkId::new(boundary_row.id),
                                    },
                                    "title" => CursorBoundary::LinkTitle {
                                        normalized_title: boundary_row.title_search.clone(),
                                        id: LinkId::new(boundary_row.id),
                                    },
                                    "visit_count" => CursorBoundary::LinkVisitCount {
                                        count: boundary_row.visit_count,
                                        id: LinkId::new(boundary_row.id),
                                    },
                                    _ => unreachable!(),
                                };
                                let continuation = explain(
                                    &pool,
                                    owner,
                                    &query,
                                    Some(&boundary),
                                )
                                    .await
                                    .unwrap();
                                assert_plan_uses(
                                    &continuation,
                                    expected_index,
                                    &format!("{prefix} {sort} {order} continuation"),
                                );
                                print_plan(
                                    &format!("{prefix} {sort} {order} continuation"),
                                    &continuation,
                                );
                            }
                        }
                    }

                    let literal = LinkListQuery::parse(Some(
                        "scope=own&q=%25%5F%5C&sort=created_at&order=desc&limit=50",
                    ))
                    .unwrap();
                    let literal_plan = explain(&pool, owner_a, &literal, None)
                        .await
                        .unwrap();
                    assert_plan_uses(
                        &literal_plan,
                        "links_owner_id_idx",
                        "literal substring scope candidate",
                    );
                    print_plan("selective literal substring; bounded page", &literal_plan);

                    for (filters, mode) in [
                        ("tag=common&tag=unknown&tag_mode=any", "any"),
                        ("tag=common&tag=needle-tag&tag_mode=all", "all"),
                    ] {
                        let query = LinkListQuery::parse(Some(&format!(
                            "scope=own&{filters}&sort=created_at&order=desc&limit=50"
                        )))
                        .unwrap();
                        let plan = explain(&pool, owner_a, &query, None)
                            .await
                            .unwrap();
                        assert_plan_uses(
                            &plan,
                            if mode == "any" {
                                "link_tags_pkey"
                            } else {
                                "link_tags_name_link_idx"
                            },
                            &format!("tag {mode} association lookup"),
                        );
                        print_plan(&format!("tag {mode} predicate"), &plan);
                    }

                    for (scope, query_text) in [
                        ("owner", "scope=own&limit=50"),
                        ("all", "scope=all&limit=50"),
                        ("name-search", "scope=all&q=needle&limit=50"),
                    ] {
                        let query = TagDiscoveryQuery::parse(Some(query_text)).unwrap();
                        let plan = crate::tags::repository::explain(
                            &pool,
                            owner_a,
                            &query,
                            None,
                        )
                        .await
                        .unwrap();
                        print_plan(&format!("tag discovery {scope}"), &plan);
                    }
                    let tag_query = TagDiscoveryQuery::parse(Some("scope=all&q=needle&limit=50"))
                        .unwrap();
                    let tag_boundary = CursorBoundary::TagName {
                        name: "needle".to_owned(),
                    };
                    let plan = crate::tags::repository::explain(
                        &pool,
                        owner_a,
                        &tag_query,
                        Some(&tag_boundary),
                    )
                    .await
                    .unwrap();
                    print_plan("tag discovery grouped continuation", &plan);
                })
            })
            .await;
    }

    fn assert_plan_uses(plan: &serde_json::Value, index: &str, label: &str) {
        let indexes = plan_index_names(plan);
        assert!(
            indexes.iter().any(|name| name == index),
            "{label} expected index {index}; actual plan indexes: {indexes:?}; plan: {plan}"
        );
    }

    fn plan_index_names(plan: &serde_json::Value) -> Vec<String> {
        fn walk(node: &serde_json::Value, output: &mut Vec<String>) {
            if let Some(name) = node.get("Index Name").and_then(serde_json::Value::as_str) {
                output.push(name.to_owned());
            }
            if let Some(children) = node.get("Plans").and_then(serde_json::Value::as_array) {
                for child in children {
                    walk(child, output);
                }
            }
        }
        let mut output = Vec::new();
        if let Some(root) = plan.as_array().and_then(|plans| plans.first()) {
            walk(&root["Plan"], &mut output);
        }
        output
    }

    fn print_plan(label: &str, plan: &serde_json::Value) {
        fn walk(node: &serde_json::Value, output: &mut Vec<String>) {
            if let Some(kind) = node.get("Node Type").and_then(serde_json::Value::as_str) {
                let index = node
                    .get("Index Name")
                    .and_then(serde_json::Value::as_str)
                    .map(|name| format!("[{name}]"))
                    .unwrap_or_default();
                let actual = node
                    .get("Actual Rows")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default();
                let planned = node
                    .get("Plan Rows")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default();
                let loops = node
                    .get("Actual Loops")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default();
                let hits = node
                    .get("Shared Hit Blocks")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default();
                let reads = node
                    .get("Shared Read Blocks")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default();
                let sort = node
                    .get("Sort Method")
                    .and_then(serde_json::Value::as_str)
                    .map(|method| format!(" sort={method}"))
                    .unwrap_or_default();
                let removed = node
                    .get("Rows Removed by Filter")
                    .and_then(serde_json::Value::as_i64)
                    .map(|count| format!(" removed={count}"))
                    .unwrap_or_default();
                output.push(format!(
                    "{kind}{index} rows={actual}/{planned} loops={loops} hit={hits} read={reads}{sort}{removed}"
                ));
            }
            if let Some(children) = node.get("Plans").and_then(serde_json::Value::as_array) {
                for child in children {
                    walk(child, output);
                }
            }
        }
        let mut summary = Vec::new();
        if let Some(root) = plan.as_array().and_then(|plans| plans.first()) {
            walk(&root["Plan"], &mut summary);
        }
        let execution_ms = plan
            .as_array()
            .and_then(|plans| plans.first())
            .and_then(|root| root.get("Execution Time"))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or_default();
        eprintln!(
            "DISCOVERY_EXPLAIN {label}: execution_ms={execution_ms:.3} {}",
            summary.join(" -> ")
        );
    }
}
