//! Static production SQL builders for per-link analytics reports.

use sqlx::{Postgres, QueryBuilder};

use crate::{
    domain::time::TimeInterval,
    http::query::GroupBy,
    pagination::{breakdown_boundary::BreakdownBoundary, visit_boundary::VisitBoundary},
};

pub(crate) fn visits(
    link_id: uuid::Uuid,
    interval: TimeInterval,
    boundary: Option<&VisitBoundary>,
    limit_plus_one: i64,
    explain: bool,
) -> QueryBuilder<Postgres> {
    let mut query = QueryBuilder::<Postgres>::new(if explain {
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) "
    } else {
        ""
    });
    query.push(
        "SELECT id, link_id, redirect_url, occurred_at, method, client_ip, \
         referrer, user_agent, device, browser, os FROM visits WHERE link_id = ",
    );
    query.push_bind(link_id);
    push_interval(&mut query, interval);
    if let Some(boundary) = boundary {
        query
            .push(" AND (occurred_at, id) < (")
            .push_bind(boundary.occurred_at.as_utc())
            .push(", ")
            .push_bind(boundary.id.as_uuid())
            .push(")");
    }
    query
        .push(" ORDER BY occurred_at DESC, id DESC LIMIT ")
        .push_bind(limit_plus_one);
    query
}

pub(crate) fn summary(
    link_id: uuid::Uuid,
    interval: TimeInterval,
    explain: bool,
) -> QueryBuilder<Postgres> {
    let prefix = if explain {
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) "
    } else {
        ""
    };
    let mut query = QueryBuilder::<Postgres>::new(prefix);
    query.push(
        "SELECT l.visit_count AS lifetime_visits, \
         (SELECT count(*)::bigint FROM visits v WHERE v.link_id = l.id",
    );
    push_interval_qualified(&mut query, interval, "v.occurred_at");
    query
        .push(") AS matching_visits FROM links l WHERE l.id = ")
        .push_bind(link_id);
    query
}

pub(crate) fn breakdown(
    link_id: uuid::Uuid,
    interval: TimeInterval,
    group_by: GroupBy,
    boundary: Option<&BreakdownBoundary>,
    limit_plus_one: i64,
    explain: bool,
) -> QueryBuilder<Postgres> {
    let expression = grouping_expression(group_by);
    let mut query = QueryBuilder::<Postgres>::new(if explain {
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) "
    } else {
        ""
    });
    query
        // MATERIALIZED is deliberate: page boundaries apply only after complete
        // per-group counts exist, never to raw rows before aggregation.
        .push("WITH grouped AS MATERIALIZED (SELECT ")
        .push(expression)
        .push(" AS value, count(*)::bigint AS visits FROM visits v WHERE v.link_id = ")
        .push_bind(link_id);
    push_interval_qualified(&mut query, interval, "v.occurred_at");
    query.push(" GROUP BY 1) SELECT value, visits FROM grouped WHERE TRUE");
    if let Some(boundary) = boundary {
        match boundary.value.as_deref() {
            None => {
                query.push(" AND value IS NOT NULL");
            }
            Some(value) => {
                query
                    .push(" AND value IS NOT NULL AND value COLLATE \"C\" > ")
                    .push_bind(value.to_owned())
                    .push("::text COLLATE \"C\"");
            }
        };
    }
    query
        .push(" ORDER BY value COLLATE \"C\" ASC NULLS FIRST LIMIT ")
        .push_bind(limit_plus_one);
    query
}

pub(crate) const fn grouping_expression(group_by: GroupBy) -> &'static str {
    match group_by {
        GroupBy::Day => {
            "(CASE WHEN EXTRACT(YEAR FROM (v.occurred_at AT TIME ZONE 'UTC')::date)::int \
                         BETWEEN 1 AND 9999 \
               THEN to_char((v.occurred_at AT TIME ZONE 'UTC')::date, 'YYYY-MM-DD') \
               WHEN EXTRACT(YEAR FROM (v.occurred_at AT TIME ZONE 'UTC')::date)::int = -1 \
               THEN '0000-' || to_char((v.occurred_at AT TIME ZONE 'UTC')::date, 'MM-DD') \
               ELSE (v.id::text)::integer::text END) COLLATE \"C\""
        }
        GroupBy::Method => {
            "(CASE WHEN v.method IN ('GET', 'HEAD') THEN v.method \
             ELSE (v.id::text)::integer::text END) COLLATE \"C\""
        }
        GroupBy::Referrer => {
            "(CASE WHEN v.referrer IS NULL OR octet_length(v.referrer) <= 8192 \
             THEN v.referrer ELSE (v.id::text)::integer::text END) COLLATE \"C\""
        }
        GroupBy::ClientIp => {
            "(CASE WHEN (family(v.client_ip) = 4 AND masklen(v.client_ip) = 32) \
               OR (family(v.client_ip) = 6 AND masklen(v.client_ip) = 128) THEN \
               CASE WHEN family(v.client_ip) = 6 AND host(v.client_ip) LIKE '::ffff:%' \
                 THEN substring(host(v.client_ip) FROM 8) ELSE host(v.client_ip) END \
             ELSE (v.id::text)::integer::text END) COLLATE \"C\""
        }
        GroupBy::UserAgent => {
            "(CASE WHEN v.user_agent IS NULL OR octet_length(v.user_agent) <= 8192 \
             THEN v.user_agent ELSE (v.id::text)::integer::text END) COLLATE \"C\""
        }
        GroupBy::Device => {
            "(CASE WHEN v.device IN ('desktop', 'mobile', 'tablet', 'other', 'unknown') \
             THEN v.device ELSE (v.id::text)::integer::text END) COLLATE \"C\""
        }
        GroupBy::Browser => {
            "(CASE WHEN char_length(v.browser) > 0 AND octet_length(v.browser) <= 8192 \
             THEN v.browser \
             ELSE (v.id::text)::integer::text END) COLLATE \"C\""
        }
        GroupBy::Os => {
            "(CASE WHEN char_length(v.os) > 0 AND octet_length(v.os) <= 8192 \
             THEN v.os \
             ELSE (v.id::text)::integer::text END) COLLATE \"C\""
        }
    }
}

fn push_interval(query: &mut QueryBuilder<Postgres>, interval: TimeInterval) {
    push_interval_qualified(query, interval, "occurred_at");
}

fn push_interval_qualified(
    query: &mut QueryBuilder<Postgres>,
    interval: TimeInterval,
    column: &str,
) {
    if let Some(from) = interval.from() {
        query
            .push(" AND ")
            .push(column)
            .push(" >= ")
            .push_bind(from.as_utc());
    }
    if let Some(to) = interval.to() {
        query
            .push(" AND ")
            .push(column)
            .push(" < ")
            .push_bind(to.as_utc());
    }
}
