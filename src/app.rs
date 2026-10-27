use std::{sync::Arc, time::Duration};

use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{
    config::Config,
    db::{migrate::SchemaReadiness, pool},
    http,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub readiness: Arc<ReadinessState>,
    pub acquire_timeout: Duration,
    pub(crate) http_admission: HttpAdmission,
}

impl AppState {
    pub fn from_config(config: &Config) -> Result<Self, pool::PoolBuildError> {
        let pool = pool::create_pool(&config.database)?;
        Ok(Self::new(
            pool,
            config.database.acquire_timeout,
            config.http_max_in_flight,
            config.http_max_waiting,
        ))
    }

    pub fn new(
        pool: PgPool,
        acquire_timeout: Duration,
        http_max_in_flight: usize,
        http_max_waiting: usize,
    ) -> Self {
        Self {
            pool,
            readiness: Arc::new(ReadinessState::new_accepting()),
            acquire_timeout,
            http_admission: HttpAdmission::new(http_max_in_flight, http_max_waiting),
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

    pub fn mark_schema_compatible(&self) {
        self.schema.mark_compatible();
    }

    pub fn mark_schema_incompatible(&self) {
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
