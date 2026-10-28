//! Read-only report operations. Final authority validation and report SQL share
//! one PostgreSQL REPEATABLE READ snapshot on one held pool connection.

use sqlx::{Executor, FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::{
    auth::{AccountRole, Principal},
    db::{
        error::{DatabaseFailure, OperationStage, classify},
        records::VisitRecord,
    },
    domain::ids::LinkId,
    domain::time::TimeInterval,
    http::query::GroupBy,
    pagination::{breakdown_boundary::BreakdownBoundary, visit_boundary::VisitBoundary},
    telemetry::metrics::{DbOperation, DbTimer},
};

use super::{report_query, reports::ReportFailure};

const ACCOUNT_AUTHORITY_SQL: &str = "SELECT actor.id AS actor_id, actor.role AS actor_role, \
     actor.enabled AS actor_enabled, target.id AS target_id, target.owner_id, \
     owner.enabled AS owner_enabled, target.management_token_digest, \
     NULL::bytea AS actor_token_digest \
     FROM (SELECT 1) AS seed \
     LEFT JOIN accounts actor ON actor.id = $1 AND actor.api_key_digest = $2 \
     LEFT JOIN links target ON target.short_code = $3 \
     LEFT JOIN accounts owner ON owner.id = target.owner_id";

const TOKEN_AUTHORITY_SQL: &str = "SELECT actor.id AS actor_id, NULL::text AS actor_role, \
     NULL::boolean AS actor_enabled, target.id AS target_id, target.owner_id, \
     owner.enabled AS owner_enabled, target.management_token_digest, \
     actor.management_token_digest AS actor_token_digest \
     FROM (SELECT 1) AS seed \
     LEFT JOIN links actor ON actor.id = $1 AND actor.owner_id IS NULL \
       AND actor.management_token_digest = $2 \
     LEFT JOIN links target ON target.short_code = $3 \
     LEFT JOIN accounts owner ON owner.id = target.owner_id";

#[derive(FromRow)]
struct AuthorityRow {
    actor_id: Option<Uuid>,
    actor_role: Option<String>,
    actor_enabled: Option<bool>,
    target_id: Option<Uuid>,
    owner_id: Option<Uuid>,
    owner_enabled: Option<bool>,
    management_token_digest: Option<Vec<u8>>,
    actor_token_digest: Option<Vec<u8>>,
}

#[derive(FromRow)]
pub(crate) struct SummaryRecord {
    pub(crate) lifetime_visits: i64,
    pub(crate) matching_visits: i64,
}

#[derive(FromRow)]
pub(crate) struct BreakdownRecord {
    pub(crate) value: Option<String>,
    pub(crate) visits: i64,
}

/// A precedence-only check. It returns an immutable target ID and no report data.
#[tracing::instrument(skip_all, name = "db.report.authorize")]
pub(crate) async fn authorize(
    pool: &PgPool,
    principal: &Principal,
    code: &str,
) -> Result<LinkId, ReportFailure> {
    let mut timer = DbTimer::new(DbOperation::ReportQuery);
    let result = async {
        let row = authority(pool, principal, code).await.map_err(map_sql)?;
        authorized_target(row, principal)
    }
    .await;
    if matches!(
        &result,
        Ok(_) | Err(ReportFailure::InvalidCredentials | ReportFailure::NotFound)
    ) {
        timer.success();
    } else {
        timer.failure();
    }
    result
}

#[tracing::instrument(skip_all, name = "db.report.visits")]
pub(crate) async fn visits(
    pool: &PgPool,
    principal: &Principal,
    code: &str,
    interval: TimeInterval,
    boundary: Option<&VisitBoundary>,
    limit_plus_one: i64,
) -> Result<Vec<VisitRecord>, ReportFailure> {
    let mut timer = DbTimer::new(DbOperation::ReportQuery);
    let (mut transaction, link_id) = match begin_authorized_snapshot(pool, principal, code).await {
        Ok(snapshot) => snapshot,
        Err(failure) => {
            if matches!(
                failure,
                ReportFailure::InvalidCredentials | ReportFailure::NotFound
            ) {
                timer.success();
            }
            return Err(failure);
        }
    };
    let rows = report_query::visits(link_id.as_uuid(), interval, boundary, limit_plus_one, false)
        .build_query_as::<VisitRecord>()
        .fetch_all(&mut *transaction)
        .await
        .map_err(map_sql)?;
    commit(transaction).await?;
    timer.success();
    Ok(rows)
}

#[tracing::instrument(skip_all, name = "db.report.summary")]
pub(crate) async fn summary(
    pool: &PgPool,
    principal: &Principal,
    code: &str,
    interval: TimeInterval,
) -> Result<SummaryRecord, ReportFailure> {
    let mut timer = DbTimer::new(DbOperation::ReportQuery);
    let (mut transaction, link_id) = match begin_authorized_snapshot(pool, principal, code).await {
        Ok(snapshot) => snapshot,
        Err(failure) => {
            if matches!(
                failure,
                ReportFailure::InvalidCredentials | ReportFailure::NotFound
            ) {
                timer.success();
            }
            return Err(failure);
        }
    };
    let record = report_query::summary(link_id.as_uuid(), interval, false)
        .build_query_as::<SummaryRecord>()
        .fetch_optional(&mut *transaction)
        .await
        .map_err(map_sql)?
        .ok_or(ReportFailure::NotFound);
    let record = match record {
        Ok(record) => record,
        Err(ReportFailure::NotFound) => {
            timer.success();
            return Err(ReportFailure::NotFound);
        }
        Err(failure) => return Err(failure),
    };
    commit(transaction).await?;
    timer.success();
    Ok(record)
}

#[cfg(test)]
pub(crate) async fn explain_visits(
    pool: &PgPool,
    link_id: Uuid,
    interval: TimeInterval,
    boundary: Option<&VisitBoundary>,
    limit_plus_one: i64,
) -> Result<serde_json::Value, sqlx::Error> {
    use sqlx::Row;
    let row = report_query::visits(link_id, interval, boundary, limit_plus_one, true)
        .build()
        .fetch_one(pool)
        .await?;
    let plan: sqlx::types::Json<serde_json::Value> = row.try_get(0)?;
    Ok(plan.0)
}

#[cfg(test)]
pub(crate) async fn explain_summary(
    pool: &PgPool,
    link_id: Uuid,
    interval: TimeInterval,
) -> Result<serde_json::Value, sqlx::Error> {
    use sqlx::Row;
    let row = report_query::summary(link_id, interval, true)
        .build()
        .fetch_one(pool)
        .await?;
    let plan: sqlx::types::Json<serde_json::Value> = row.try_get(0)?;
    Ok(plan.0)
}

#[cfg(test)]
pub(crate) async fn explain_breakdown(
    pool: &PgPool,
    link_id: Uuid,
    interval: TimeInterval,
    group_by: GroupBy,
    boundary: Option<&BreakdownBoundary>,
    limit_plus_one: i64,
) -> Result<serde_json::Value, sqlx::Error> {
    use sqlx::Row;
    let row = report_query::breakdown(link_id, interval, group_by, boundary, limit_plus_one, true)
        .build()
        .fetch_one(pool)
        .await?;
    let plan: sqlx::types::Json<serde_json::Value> = row.try_get(0)?;
    Ok(plan.0)
}

#[tracing::instrument(skip_all, name = "db.report.breakdown")]
pub(crate) async fn breakdown(
    pool: &PgPool,
    principal: &Principal,
    code: &str,
    interval: TimeInterval,
    group_by: GroupBy,
    boundary: Option<&BreakdownBoundary>,
    limit_plus_one: i64,
) -> Result<Vec<BreakdownRecord>, ReportFailure> {
    let mut timer = DbTimer::new(DbOperation::ReportQuery);
    let (mut transaction, link_id) = match begin_authorized_snapshot(pool, principal, code).await {
        Ok(snapshot) => snapshot,
        Err(failure) => {
            if matches!(
                failure,
                ReportFailure::InvalidCredentials | ReportFailure::NotFound
            ) {
                timer.success();
            }
            return Err(failure);
        }
    };
    let rows = report_query::breakdown(
        link_id.as_uuid(),
        interval,
        group_by,
        boundary,
        limit_plus_one,
        false,
    )
    .build_query_as::<BreakdownRecord>()
    .fetch_all(&mut *transaction)
    .await
    .map_err(map_sql)?;
    commit(transaction).await?;
    timer.success();
    Ok(rows)
}

pub(crate) async fn begin_authorized_snapshot(
    pool: &PgPool,
    principal: &Principal,
    code: &str,
) -> Result<(Transaction<'static, Postgres>, LinkId), ReportFailure> {
    let mut transaction = pool.begin().await.map_err(map_sql)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *transaction)
        .await
        .map_err(map_sql)?;
    let row = authority(&mut *transaction, principal, code)
        .await
        .map_err(map_sql)?;
    let link_id = authorized_target(row, principal)?;
    Ok((transaction, link_id))
}

async fn authority<'e, E>(
    executor: E,
    principal: &Principal,
    code: &str,
) -> Result<AuthorityRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    let query = match principal {
        Principal::Account(account) => sqlx::query_as::<_, AuthorityRow>(ACCOUNT_AUTHORITY_SQL)
            .bind(account.id().as_uuid())
            .bind(account.digest().as_bytes().as_slice())
            .bind(code.to_owned()),
        Principal::ManagementToken(token) => sqlx::query_as::<_, AuthorityRow>(TOKEN_AUTHORITY_SQL)
            .bind(token.link_id().as_uuid())
            .bind(token.digest().as_bytes().as_slice())
            .bind(code.to_owned()),
    };
    query.fetch_one(executor).await
}

fn authorized_target(row: AuthorityRow, principal: &Principal) -> Result<LinkId, ReportFailure> {
    let actor_id = row.actor_id.ok_or(ReportFailure::InvalidCredentials)?;
    match principal {
        Principal::Account(account) => {
            if row.actor_enabled != Some(true) {
                return Err(ReportFailure::InvalidCredentials);
            }
            let role = row
                .actor_role
                .as_deref()
                .and_then(AccountRole::parse_database)
                .ok_or(ReportFailure::Internal)?;
            let target_id = row.target_id.ok_or(ReportFailure::NotFound)?;
            validate_target_owner(&row)?;
            let authorized = role == AccountRole::Admin
                || (role == AccountRole::User
                    && row.owner_id == Some(account.id().as_uuid())
                    && row.owner_enabled == Some(true));
            if authorized {
                Ok(LinkId::new(target_id))
            } else {
                Err(ReportFailure::NotFound)
            }
        }
        Principal::ManagementToken(token) => {
            let actor_digest = row
                .actor_token_digest
                .as_deref()
                .ok_or(ReportFailure::Internal)?;
            if crate::domain::secret::SecretDigest::from_database_bytes(actor_digest).is_none() {
                return Err(ReportFailure::Internal);
            }
            let target_id = row.target_id.ok_or(ReportFailure::NotFound)?;
            validate_target_owner(&row)?;
            if actor_id == token.link_id().as_uuid()
                && target_id == token.link_id().as_uuid()
                && row.owner_id.is_none()
                && row.management_token_digest.as_deref()
                    == Some(token.digest().as_bytes().as_slice())
            {
                Ok(LinkId::new(target_id))
            } else {
                Err(ReportFailure::NotFound)
            }
        }
    }
}

fn validate_target_owner(row: &AuthorityRow) -> Result<(), ReportFailure> {
    match (
        row.owner_id,
        row.owner_enabled,
        row.management_token_digest.as_deref(),
    ) {
        (None, None, Some(digest))
            if crate::domain::secret::SecretDigest::from_database_bytes(digest).is_some() =>
        {
            Ok(())
        }
        (Some(_), Some(_), None) => Ok(()),
        _ => Err(ReportFailure::Internal),
    }
}

async fn commit(transaction: Transaction<'static, Postgres>) -> Result<(), ReportFailure> {
    transaction
        .commit()
        .await
        .map_err(|error| match classify(&error, OperationStage::Commit) {
            DatabaseFailure::Unavailable(_) | DatabaseFailure::CommitOutcomeUnknown => {
                ReportFailure::StorageUnavailable
            }
            DatabaseFailure::Conflict(_)
            | DatabaseFailure::NotFound
            | DatabaseFailure::Internal => ReportFailure::Internal,
        })
}

fn map_sql(error: sqlx::Error) -> ReportFailure {
    match classify(&error, OperationStage::BeforeCommit) {
        DatabaseFailure::Unavailable(_) | DatabaseFailure::CommitOutcomeUnknown => {
            ReportFailure::StorageUnavailable
        }
        DatabaseFailure::Conflict(_) | DatabaseFailure::NotFound | DatabaseFailure::Internal => {
            ReportFailure::Internal
        }
    }
}
