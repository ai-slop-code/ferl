use std::{sync::Arc, time::Duration};

use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{
    config::Config,
    db::{
        migrate::{self, SchemaReadiness, VerificationError},
        pool,
    },
    http,
    pagination::CursorCodec,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub readiness: Arc<ReadinessState>,
    pub acquire_timeout: Duration,
    pub lock_timeout: Duration,
    pub creation: crate::rate_limit::CreationService,
    pub links: crate::links::LinkService,
    pub client_ip: crate::client_ip::ClientIpResolver,
    pub(crate) analytics: crate::analytics::VisitQueue,
    analytics_config: crate::config::AnalyticsConfig,
    pub(crate) link_operation_timeout: Duration,
    pub(crate) cursor_codec: CursorCodec,
    pub(crate) http_admission: HttpAdmission,
}

impl AppState {
    pub fn from_config(config: &Config) -> Result<Self, pool::PoolBuildError> {
        let pool = pool::create_pool(&config.database)?;
        let analytics = crate::analytics::VisitQueue::new(config.analytics.queue_capacity);
        Ok(Self {
            creation: crate::rate_limit::CreationService::from_config(pool.clone(), config),
            links: crate::links::LinkService::from_config(pool.clone(), config),
            client_ip: crate::client_ip::ClientIpResolver::new(&config.trusted_proxies),
            analytics,
            analytics_config: config.analytics,
            link_operation_timeout: config.link_operation_timeout,
            pool,
            readiness: Arc::new(ReadinessState::new_accepting()),
            acquire_timeout: config.database.acquire_timeout,
            lock_timeout: config.database.lock_timeout,
            cursor_codec: CursorCodec::new(config.cursor_key.copy_bytes()),
            http_admission: HttpAdmission::new(config.http_max_in_flight, config.http_max_waiting),
        })
    }

    /// Start the instance-local bounded analytics workers exactly once. The
    /// returned guard owns their tasks and must be drained before closing the
    /// shared database pool.
    pub fn start_analytics_workers(
        &self,
    ) -> Result<crate::analytics::AnalyticsWorkers, crate::analytics::WorkerStartError> {
        self.analytics
            .start_workers(self.pool.clone(), self.analytics_config)
    }

    /// Run the production schema verifier and update this instance's readiness
    /// state from the result. There is intentionally no public method that can
    /// mark an unverified schema compatible.
    pub async fn verify_schema(&self) -> Result<(), VerificationError> {
        let revision = self.readiness.schema_revision();
        match migrate::verify_compatibility(&self.pool).await {
            Ok(()) => {
                if self.readiness.mark_schema_compatible_if_current(revision) {
                    Ok(())
                } else {
                    Err(VerificationError::Superseded)
                }
            }
            Err(error) => {
                self.readiness.mark_schema_incompatible();
                Err(error)
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct HttpAdmission {
    admitted: Arc<Semaphore>,
    in_flight: Arc<Semaphore>,
}

impl HttpAdmission {
    pub(crate) fn new(max_in_flight: usize, max_waiting: usize) -> Self {
        debug_assert!((1..=4096).contains(&max_in_flight));
        debug_assert!(max_waiting <= 4096);
        Self {
            admitted: Arc::new(Semaphore::new(max_in_flight + max_waiting)),
            in_flight: Arc::new(Semaphore::new(max_in_flight)),
        }
    }

    pub(crate) fn try_admit(&self) -> Option<OwnedSemaphorePermit> {
        self.admitted.clone().try_acquire_owned().ok()
    }

    pub(crate) async fn acquire_in_flight(&self) -> OwnedSemaphorePermit {
        self.in_flight
            .clone()
            .acquire_owned()
            .await
            .expect("HTTP admission semaphore remains open for the router lifetime")
    }

    #[cfg(test)]
    pub(crate) fn available_admissions(&self) -> usize {
        self.admitted.available_permits()
    }
}

#[derive(Default)]
pub struct ReadinessState {
    accepting_work: AtomicBool,
    schema: SchemaReadiness,
}

impl ReadinessState {
    pub fn new_accepting() -> Self {
        Self {
            accepting_work: AtomicBool::new(true),
            schema: SchemaReadiness::default(),
        }
    }

    pub(crate) fn schema_revision(&self) -> u64 {
        self.schema.revision()
    }

    pub(crate) fn mark_schema_compatible_if_current(&self, revision: u64) -> bool {
        self.schema.mark_compatible_if_current(revision)
    }

    pub(crate) fn mark_schema_incompatible(&self) {
        self.schema.mark_incompatible();
    }

    pub fn begin_draining(&self) {
        self.accepting_work.store(false, Ordering::Release);
    }

    pub fn is_accepting_work(&self) -> bool {
        self.accepting_work.load(Ordering::Acquire)
    }

    pub fn is_schema_compatible(&self) -> bool {
        self.schema.compatible()
    }
}

pub fn router(state: AppState) -> axum::Router {
    http::router(state)
}

/// Transport composition shared by the executable and TCP integration harness.
pub fn transport_service(
    router: axum::Router,
) -> axum::extract::connect_info::IntoMakeServiceWithConnectInfo<axum::Router, std::net::SocketAddr>
{
    router.into_make_service_with_connect_info::<std::net::SocketAddr>()
}
