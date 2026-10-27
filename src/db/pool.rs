use std::{str::FromStr, time::Duration};

use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::config::DatabaseConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolBuildError {
    InvalidConnectionOptions,
}

pub fn create_pool(config: &DatabaseConfig) -> Result<PgPool, PoolBuildError> {
    let options = sqlx::postgres::PgConnectOptions::from_str(config.url.expose_to_driver())
        .map_err(|_| PoolBuildError::InvalidConnectionOptions)?;
    let statement_timeout = duration_millis(config.statement_timeout);
    let lock_timeout = duration_millis(config.lock_timeout);

    // SQLx 0.9 uses one acquisition deadline for both pool waits and connection setup.
    let pool = PgPoolOptions::new()
        .min_connections(config.pool_min_connections)
        .max_connections(config.pool_max_connections)
        .acquire_timeout(config.acquire_timeout.min(config.connect_timeout))
        .after_connect(move |connection, _metadata| {
            let statement_timeout = statement_timeout.clone();
            let lock_timeout = lock_timeout.clone();
            Box::pin(async move {
                sqlx::query(
                    "SELECT set_config('statement_timeout', $1, false), \
                            set_config('lock_timeout', $2, false)",
                )
                .bind(statement_timeout)
                .bind(lock_timeout)
                .execute(connection)
                .await?;
                Ok(())
            })
        })
        .connect_lazy_with(options);

    Ok(pool)
}

fn duration_millis(duration: Duration) -> String {
    duration.as_millis().to_string()
}
