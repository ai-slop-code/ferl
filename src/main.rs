use std::{future::IntoFuture, process::ExitCode, time::Instant};

use clap::{Parser, Subcommand};
use tokio::{net::TcpListener, time::timeout};
use tokio_util::sync::CancellationToken;

use ferl_server::{
    BUILD_VERSION,
    app::{self, AppState},
    config::{Config, ConfigError, MigrationConfig},
    db::migrate::{self, MigrationFailure},
    db::pool::PoolBuildError,
    shutdown,
    telemetry::{
        logging,
        metrics::{self, Operation, Outcome},
    },
};

#[derive(Parser)]
#[command(name = "ferl-server", version = BUILD_VERSION, about = "ferl backend server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Serve,
    Migrate,
    BootstrapAdmin {
        #[arg(long)]
        username: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    logging::init_stdout();
    let command = Cli::parse().command.unwrap_or(Command::Serve);
    match command {
        Command::Serve => match Config::from_env() {
            Ok(config) => match serve(config).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    tracing::error!(
                        category = error.category(),
                        "server terminated with a sanitized failure"
                    );
                    ExitCode::FAILURE
                }
            },
            Err(error) => {
                log_config_error(&error);
                ExitCode::from(2)
            }
        },
        Command::Migrate => migrate().await,
        Command::BootstrapAdmin { username: _ } => unavailable_operator_command("bootstrap-admin"),
    }
}

async fn migrate() -> ExitCode {
    let config = match MigrationConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            log_config_error(&error);
            eprintln!("migrate configuration rejected ({})", error.setting);
            return ExitCode::from(2);
        }
    };

    match migrate::run_migrations(&config).await {
        Ok(()) => {
            tracing::info!(
                schema_version = migrate::LATEST_SCHEMA_VERSION,
                category = "migrations_complete",
                "PostgreSQL schema migrations completed"
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            log_migration_error(error);
            eprintln!("migrate failed ({})", error.safe_category());
            ExitCode::FAILURE
        }
    }
}

fn log_migration_error(error: MigrationFailure) {
    tracing::error!(
        category = error.safe_category(),
        "PostgreSQL migration command failed with a sanitized category"
    );
}

fn unavailable_operator_command(command: &'static str) -> ExitCode {
    eprintln!("{command} is not available in this implementation stage; no database was modified");
    tracing::warn!(
        command,
        category = "operator_command_unavailable",
        "operator command is not implemented"
    );
    ExitCode::from(2)
}

fn log_config_error(error: &ConfigError) {
    tracing::error!(
        category = "configuration_invalid",
        setting = error.setting,
        reason = error.reason,
        "startup configuration rejected"
    );
}

#[derive(Clone, Copy)]
enum ServerError {
    DatabaseConfiguration,
    ListenerBind,
    HttpServer,
}

impl ServerError {
    const fn category(self) -> &'static str {
        match self {
            Self::DatabaseConfiguration => "database_configuration_invalid",
            Self::ListenerBind => "listener_bind_failed",
            Self::HttpServer => "http_server_failed",
        }
    }
}

async fn serve(config: Config) -> Result<(), ServerError> {
    let state = AppState::from_config(&config).map_err(|error| match error {
        PoolBuildError::InvalidConnectionOptions => ServerError::DatabaseConfiguration,
    })?;
    let pool = state.pool.clone();
    let listener = TcpListener::bind(config.listen_addr)
        .await
        .map_err(|_| ServerError::ListenerBind)?;

    tracing::info!(
        service_name = "ferl",
        service_version = BUILD_VERSION,
        instance_id = %config.telemetry.instance_id,
        category = "server_started",
        "HTTP server is listening"
    );
    tracing::info!(
        schema_compatible = false,
        category = "readiness_schema_unverified",
        "instance remains unready until schema compatibility is verified"
    );

    let cancellation = CancellationToken::new();
    let mut schema_monitor = migrate::spawn_schema_monitor(
        state.pool.clone(),
        state.readiness.clone(),
        cancellation.clone(),
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(30),
    );
    let mut server = Box::pin(
        axum::serve(listener, app::router(state.clone()))
            .with_graceful_shutdown(cancellation.clone().cancelled_owned())
            .into_future(),
    );

    tokio::select! {
        result = &mut server => {
            cancellation.cancel();
            finish_schema_monitor(&mut schema_monitor).await;
            result.map_err(|_| ServerError::HttpServer)?;
            let pool_closed = close_pool_bounded(&pool, config.shutdown.total_timeout).await;
            tracing::info!(
                category = "server_stopped",
                outcome = if pool_closed { "success" } else { "pool_close_deadline" },
                "HTTP server stopped"
            );
        }
        _ = shutdown::wait_for_shutdown_signal() => {
            let shutdown_started = Instant::now();
            tracing::info!(
                category = "server_draining",
                "server is stopping acceptance and draining HTTP requests"
            );
            let drain = shutdown::stop_accepting_and_drain(
                &state.readiness,
                &cancellation,
                server.as_mut(),
                config.shutdown.http_drain_timeout,
            )
            .await;
            finish_schema_monitor(&mut schema_monitor).await;
            match drain {
                shutdown::HttpDrainOutcome::Drained => {}
                shutdown::HttpDrainOutcome::ServerFailed => {}
                shutdown::HttpDrainOutcome::DeadlineExceeded => tracing::warn!(
                    category = "http_drain_deadline_reached",
                    "HTTP drain deadline reached; active connections will be closed"
                ),
            }
            drop(server);

            let remaining = config
                .shutdown
                .total_timeout
                .saturating_sub(shutdown_started.elapsed());
            let pool_closed = if remaining.is_zero() {
                tracing::warn!(
                    category = "shutdown_deadline_reached",
                    "total shutdown deadline reached before database pool closure"
                );
                false
            } else {
                close_pool_bounded(&pool, remaining).await
            };
            let (outcome, metric_outcome) = match (drain, pool_closed) {
                (shutdown::HttpDrainOutcome::ServerFailed, _) => {
                    ("http_server_failed", Outcome::Failure)
                }
                (shutdown::HttpDrainOutcome::DeadlineExceeded, _) => {
                    ("http_drain_deadline", Outcome::Unavailable)
                }
                (shutdown::HttpDrainOutcome::Drained, false) => {
                    ("pool_close_deadline", Outcome::Unavailable)
                }
                (shutdown::HttpDrainOutcome::Drained, true) => ("success", Outcome::Success),
            };
            metrics::record_operation(Operation::Shutdown, metric_outcome);
            tracing::info!(
                category = "server_shutdown_complete",
                outcome,
                "server shutdown completed"
            );
            if drain == shutdown::HttpDrainOutcome::ServerFailed {
                return Err(ServerError::HttpServer);
            }
        }
    }

    Ok(())
}

async fn finish_schema_monitor(monitor: &mut tokio::task::JoinHandle<()>) {
    match timeout(std::time::Duration::from_secs(2), &mut *monitor).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => tracing::warn!(
            category = "schema_monitor_stopped_unexpectedly",
            "schema monitor task did not complete normally"
        ),
        Err(_) => {
            monitor.abort();
            let _ = monitor.await;
            tracing::warn!(
                category = "schema_monitor_shutdown_deadline",
                "schema monitor exceeded its shutdown bound and was cancelled"
            );
        }
    }
}

async fn close_pool_bounded(pool: &sqlx::PgPool, deadline: std::time::Duration) -> bool {
    if timeout(deadline, pool.close()).await.is_err() {
        tracing::warn!(
            category = "database_pool_close_deadline_reached",
            "database pool did not close before the shutdown deadline"
        );
        false
    } else {
        true
    }
}
