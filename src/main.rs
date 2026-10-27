use std::{future::IntoFuture, process::ExitCode, time::Instant};

use clap::{Parser, Subcommand};
use tokio::{net::TcpListener, time::timeout};
use tokio_util::sync::CancellationToken;

use ferl_server::{
    BUILD_VERSION,
    app::{self, AppState},
    config::{Config, ConfigError},
    db::pool::{self, PoolBuildError},
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
        Command::Migrate => unavailable_operator_command("migrate"),
        Command::BootstrapAdmin { username: _ } => unavailable_operator_command("bootstrap-admin"),
    }
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
    let pool = pool::create_pool(&config.database).map_err(|error| match error {
        PoolBuildError::InvalidConnectionOptions => ServerError::DatabaseConfiguration,
    })?;
    let state = AppState::new(
        pool.clone(),
        config.database.acquire_timeout,
        config.http_max_in_flight,
    );
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
    let mut server = Box::pin(
        axum::serve(listener, app::router(state.clone()))
            .with_graceful_shutdown(cancellation.clone().cancelled_owned())
            .into_future(),
    );

    tokio::select! {
        result = &mut server => {
            result.map_err(|_| ServerError::HttpServer)?;
            close_pool_bounded(&pool, config.shutdown.total_timeout).await;
        }
        _ = shutdown::wait_for_shutdown_signal() => {
            let shutdown_started = Instant::now();
            state.readiness.begin_draining();
            cancellation.cancel();
            metrics::record_operation(Operation::Shutdown, Outcome::Success);
            let mut server_failed = false;
            match timeout(config.shutdown.http_drain_timeout, &mut server).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => server_failed = true,
                Err(_) => tracing::warn!(
                    category = "http_drain_deadline_reached",
                    "HTTP drain deadline reached; active connections will be closed"
                ),
            }
            drop(server);

            let remaining = config
                .shutdown
                .total_timeout
                .saturating_sub(shutdown_started.elapsed());
            if remaining.is_zero() {
                tracing::warn!(
                    category = "shutdown_deadline_reached",
                    "total shutdown deadline reached before database pool closure"
                );
            } else {
                close_pool_bounded(&pool, remaining).await;
            }
            if server_failed {
                return Err(ServerError::HttpServer);
            }
        }
    }

    Ok(())
}

async fn close_pool_bounded(pool: &sqlx::PgPool, deadline: std::time::Duration) {
    if timeout(deadline, pool.close()).await.is_err() {
        tracing::warn!(
            category = "database_pool_close_deadline_reached",
            "database pool did not close before the shutdown deadline"
        );
    }
}
