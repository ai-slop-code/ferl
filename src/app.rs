use std::{sync::Arc, time::Duration};

use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{db::migrate::SchemaReadiness, http};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub readiness: Arc<ReadinessState>,
    pub acquire_timeout: Duration,
    pub http_max_in_flight: usize,
}

impl AppState {
    pub fn new(pool: PgPool, acquire_timeout: Duration, http_max_in_flight: usize) -> Self {
        Self {
            pool,
            readiness: Arc::new(ReadinessState::new_accepting()),
            acquire_timeout,
            http_max_in_flight,
        }
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
