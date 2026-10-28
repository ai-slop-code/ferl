use sqlx::PgPool;

use crate::{
    auth::Principal,
    domain::{ids::LinkId, time::TimeInterval},
    http::query::GroupBy,
    pagination::{breakdown_boundary::BreakdownBoundary, visit_boundary::VisitBoundary},
};

use super::report_repository::{self, BreakdownRecord, SummaryRecord};

#[cfg(test)]
#[path = "reports/tests.rs"]
mod tests;

#[derive(Clone)]
pub(crate) struct ReportService {
    pool: PgPool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ReportFailure {
    InvalidCredentials,
    NotFound,
    StorageUnavailable,
    Internal,
}

impl ReportService {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Validate object authority before operation-specific query decoding. The
    /// final report operation repeats this check in its read snapshot.
    pub(crate) async fn authorize_link(
        &self,
        principal: &Principal,
        code: &str,
    ) -> Result<LinkId, ReportFailure> {
        report_repository::authorize(&self.pool, principal, code).await
    }

    pub(crate) async fn visits(
        &self,
        principal: &Principal,
        code: &str,
        interval: TimeInterval,
        boundary: Option<&VisitBoundary>,
        limit_plus_one: i64,
    ) -> Result<Vec<crate::db::records::VisitRecord>, ReportFailure> {
        report_repository::visits(
            &self.pool,
            principal,
            code,
            interval,
            boundary,
            limit_plus_one,
        )
        .await
    }

    pub(crate) async fn summary(
        &self,
        principal: &Principal,
        code: &str,
        interval: TimeInterval,
    ) -> Result<SummaryRecord, ReportFailure> {
        report_repository::summary(&self.pool, principal, code, interval).await
    }

    pub(crate) async fn breakdown(
        &self,
        principal: &Principal,
        code: &str,
        interval: TimeInterval,
        group_by: GroupBy,
        boundary: Option<&BreakdownBoundary>,
        limit_plus_one: i64,
    ) -> Result<Vec<BreakdownRecord>, ReportFailure> {
        report_repository::breakdown(
            &self.pool,
            principal,
            code,
            interval,
            group_by,
            boundary,
            limit_plus_one,
        )
        .await
    }
}
