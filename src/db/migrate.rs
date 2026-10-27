use std::sync::atomic::{AtomicBool, Ordering};

/// Process-local readiness state; PostgreSQL remains authoritative for schema compatibility.
/// Part 02 supplies the verifier that sets compatibility only after inspecting migrations.
#[derive(Default)]
pub struct SchemaReadiness {
    compatible: AtomicBool,
}

impl SchemaReadiness {
    pub fn compatible(&self) -> bool {
        self.compatible.load(Ordering::Acquire)
    }

    pub fn mark_compatible(&self) {
        self.compatible.store(true, Ordering::Release);
    }

    pub fn mark_incompatible(&self) {
        self.compatible.store(false, Ordering::Release);
    }
}
