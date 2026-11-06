use std::{future::IntoFuture, io::Write, process::ExitCode, time::Duration, time::Instant};

use clap::{Parser, Subcommand};
use tokio::{net::TcpListener, time::timeout};
use tokio_util::sync::CancellationToken;

use ferl_server::{
    BUILD_VERSION,
    accounts::bootstrap::{self, BootstrapFailure},
    analytics::WorkerStopOutcome,
    app::{self, AppState},
    config::{BootstrapConfig, Config, ConfigError, MigrationConfig},
    db::migrate::{self, MigrationFailure},
    db::pool::PoolBuildError,
    shutdown,
    telemetry::{
        logging,
        metrics::{self, Operation, Outcome},
        providers::{Telemetry, TelemetryInitError},
    },
};

/// Bounded stdout drain for commands without a serving shutdown budget. Serving
/// drains inside its own total deadline before this call.
const COMMAND_STDOUT_FLUSH: Duration = Duration::from_secs(1);

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
    let command = Cli::parse().command.unwrap_or(Command::Serve);
    let exit = match command {
        Command::Serve => serve_command().await,
        Command::Migrate => {
            logging::init_stdout();
            migrate().await
        }
        Command::BootstrapAdmin { username } => {
            logging::init_stderr();
            bootstrap_admin(&username).await
        }
    };
    logging::finish_stdout(Instant::now() + COMMAND_STDOUT_FLUSH).await;
    exit
}

async fn serve_command() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            logging::init_stdout();
            log_config_error(&error);
            return ExitCode::from(2);
        }
    };
    let mut telemetry =
        match Telemetry::initialize(&config.telemetry, config.shutdown.total_timeout).await {
            Ok(telemetry) => telemetry,
            Err(error) => {
                logging::init_stdout();
                match error {
                    TelemetryInitError::ExporterConfiguration => tracing::error!(
                        category = "telemetry_configuration_invalid",
                        "telemetry provider configuration could not be initialized safely"
                    ),
                    TelemetryInitError::SubscriberAlreadyInstalled => tracing::error!(
                        category = "telemetry_subscriber_initialization_failed",
                        "production telemetry subscriber could not be installed"
                    ),
                }
                return ExitCode::FAILURE;
            }
        };
    match serve(config, &mut telemetry).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(
                category = error.category(),
                "server terminated with a sanitized failure"
            );
            ExitCode::FAILURE
        }
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

async fn bootstrap_admin(username: &str) -> ExitCode {
    let config = match BootstrapConfig::from_env() {
        Ok(config) => config,
        Err(error) => {
            log_config_error(&error);
            eprintln!("bootstrap-admin configuration rejected ({})", error.setting);
            return ExitCode::from(2);
        }
    };
    match bootstrap::run(&config, username).await {
        Ok(result) => {
            tracing::info!(
                target: "ferl_server",
                category = "bootstrap_committed",
                "bootstrap transaction committed; writing one-time operator result"
            );
            eprintln!("bootstrap-admin committed; writing the one-time operator result to stdout");
            let mut bytes = match serde_json::to_vec(&result) {
                Ok(bytes) => bytes,
                Err(_) => {
                    tracing::error!(
                        target: "ferl_server",
                        category = "bootstrap_result_serialization_failed",
                        "committed bootstrap result could not be serialized"
                    );
                    eprintln!("bootstrap committed but operator result could not be written");
                    return ExitCode::FAILURE;
                }
            };
            bytes.push(b'\n');
            let stdout = std::io::stdout();
            let mut result_stream = stdout.lock();
            let write_result = result_stream
                .write_all(&bytes)
                .and_then(|()| result_stream.flush());
            if write_result.is_err() {
                tracing::error!(
                    target: "ferl_server",
                    category = "bootstrap_result_output_failed",
                    "bootstrap committed but the one-time result stream failed"
                );
                eprintln!(
                    "bootstrap committed but operator result output failed; recover through the documented operator procedure"
                );
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            let category = error.safe_category();
            tracing::error!(category, "bootstrap-admin failed with a sanitized category");
            eprintln!("bootstrap-admin failed ({category})");
            if error == BootstrapFailure::InvalidUsername {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn log_config_error(error: &ConfigError) {
    tracing::error!(
        target: "ferl_server",
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
    AnalyticsWorkerStart,
}

impl ServerError {
    const fn category(self) -> &'static str {
        match self {
            Self::DatabaseConfiguration => "database_configuration_invalid",
            Self::ListenerBind => "listener_bind_failed",
            Self::HttpServer => "http_server_failed",
            Self::AnalyticsWorkerStart => "analytics_worker_start_failed",
        }
    }
}

async fn serve(config: Config, telemetry: &mut Telemetry) -> Result<(), ServerError> {
    let state = match AppState::from_config(&config) {
        Ok(state) => state,
        Err(PoolBuildError::InvalidConnectionOptions) => {
            telemetry
                .shutdown(Instant::now() + config.shutdown.total_timeout)
                .await;
            return Err(ServerError::DatabaseConfiguration);
        }
    };
    let pool = state.pool.clone();
    let analytics_workers = match state.start_analytics_workers() {
        Ok(workers) => workers,
        Err(_) => {
            telemetry
                .shutdown(Instant::now() + config.shutdown.total_timeout)
                .await;
            return Err(ServerError::AnalyticsWorkerStart);
        }
    };
    let mut analytics_workers = Some(analytics_workers);
    let listener = match TcpListener::bind(config.listen_addr).await {
        Ok(listener) => listener,
        Err(_) => {
            let deadline = Instant::now() + config.shutdown.total_timeout;
            let _ =
                close_pool_bounded(&pool, deadline.saturating_duration_since(Instant::now())).await;
            telemetry.shutdown(deadline).await;
            return Err(ServerError::ListenerBind);
        }
    };

    tracing::info!(
        service_name = %config.telemetry.service_name,
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
        axum::serve(listener, app::transport_service(app::router(state.clone())))
            .with_graceful_shutdown(cancellation.clone().cancelled_owned())
            .into_future(),
    );

    let mut server_exit = tokio::select! {
        result = &mut server => Some(result),
        _ = shutdown::wait_for_shutdown_signal() => None,
    };
    let shutdown_started = Instant::now();
    let total_deadline = shutdown_started + config.shutdown.total_timeout;
    state.readiness.begin_draining();
    tracing::info!(
        category = "server_draining",
        "readiness withdrawn; listener remains available during the bounded withdrawal phase"
    );

    if server_exit.is_none() {
        let withdrawal = config
            .shutdown
            .readiness_withdrawal_timeout
            .min(total_deadline.saturating_duration_since(Instant::now()));
        server_exit =
            shutdown::withdraw_readiness(&state.readiness, server.as_mut(), withdrawal).await;
    }
    let unexpected = server_exit.is_some();
    let drain = if let Some(result) = server_exit.as_ref() {
        cancellation.cancel();
        if result.is_err() {
            tracing::error!(
                category = "http_server_failed",
                "HTTP server exited unexpectedly"
            );
            shutdown::HttpDrainOutcome::ServerFailed
        } else {
            tracing::warn!(
                category = "http_server_stopped_unexpectedly",
                "HTTP server stopped without a shutdown signal"
            );
            shutdown::HttpDrainOutcome::ServerFailed
        }
    } else {
        let remaining = total_deadline.saturating_duration_since(Instant::now());
        let http_budget = config.shutdown.http_drain_timeout.min(remaining);
        shutdown::stop_accepting_and_drain(&cancellation, server.as_mut(), http_budget).await
    };
    cancellation.cancel();
    if matches!(drain, shutdown::HttpDrainOutcome::DeadlineExceeded) {
        tracing::warn!(
            category = "http_drain_deadline_reached",
            "HTTP drain deadline reached; remaining application handler futures are cancelled before resource closure"
        );
    }
    let remaining = total_deadline.saturating_duration_since(Instant::now());
    if !state.cancel_admitted_http(remaining).await {
        tracing::warn!(
            category = "http_handler_cancel_deadline",
            "one or more HTTP handler permits remained until Tokio runtime teardown"
        );
    }
    drop(server);

    let remaining = total_deadline.saturating_duration_since(Instant::now());
    finish_schema_monitor(&mut schema_monitor, remaining).await;
    if let Some(workers) = analytics_workers.take() {
        let analytics_deadline = config
            .analytics
            .drain_timeout
            .min(total_deadline.saturating_duration_since(Instant::now()));
        report_analytics_shutdown(shutdown::drain_analytics(workers, analytics_deadline).await);
    }

    let remaining = total_deadline.saturating_duration_since(Instant::now());
    let pool_closed = if remaining.is_zero() {
        false
    } else {
        close_pool_bounded(&pool, remaining).await
    };
    let (outcome, metric_outcome) = match (drain, pool_closed) {
        (shutdown::HttpDrainOutcome::ServerFailed, _) => ("http_server_failed", Outcome::Failure),
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
        elapsed_ms = shutdown_started.elapsed().as_millis() as u64,
        "server shutdown completed"
    );
    telemetry.shutdown(total_deadline).await;

    if unexpected || drain == shutdown::HttpDrainOutcome::ServerFailed {
        return Err(ServerError::HttpServer);
    }
    Ok(())
}

fn report_analytics_shutdown(outcome: WorkerStopOutcome) {
    match outcome {
        WorkerStopOutcome::Drained => tracing::info!(
            category = "analytics_drain_complete",
            outcome = "success",
            "analytics workers drained the bounded in-memory queue"
        ),
        WorkerStopOutcome::DeadlineExceeded => tracing::warn!(
            category = "analytics_drain_deadline",
            outcome = "unavailable",
            "analytics shutdown deadline reached"
        ),
        WorkerStopOutcome::WorkerFailed => tracing::warn!(
            category = "analytics_worker_failed",
            outcome = "failure",
            "an analytics worker failed during shutdown"
        ),
    }
}

async fn finish_schema_monitor(
    monitor: &mut tokio::task::JoinHandle<()>,
    deadline: std::time::Duration,
) {
    match timeout(
        deadline.min(std::time::Duration::from_secs(2)),
        &mut *monitor,
    )
    .await
    {
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
