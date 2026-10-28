use std::{future::Future, io, pin::Pin, time::Duration};

use tokio::{
    signal,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

use crate::{
    analytics::{AnalyticsWorkers, WorkerStopOutcome},
    app::ReadinessState,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpDrainOutcome {
    Drained,
    DeadlineExceeded,
    ServerFailed,
}

/// Mark readiness unavailable while continuing to poll the server future so
/// health probes remain observable before acceptance is stopped.
pub async fn withdraw_readiness(
    readiness: &ReadinessState,
    server: Pin<&mut (dyn Future<Output = Result<(), io::Error>> + Send)>,
    withdrawal: Duration,
) -> Option<Result<(), io::Error>> {
    readiness.begin_draining();
    tokio::select! {
        result = server => Some(result),
        _ = sleep(withdrawal) => None,
    }
}

pub async fn stop_accepting_and_drain(
    cancellation: &CancellationToken,
    server: Pin<&mut (dyn Future<Output = Result<(), io::Error>> + Send)>,
    drain_timeout: Duration,
) -> HttpDrainOutcome {
    cancellation.cancel();
    match timeout(drain_timeout, server).await {
        Ok(Ok(())) => HttpDrainOutcome::Drained,
        Ok(Err(_)) => HttpDrainOutcome::ServerFailed,
        Err(_) => HttpDrainOutcome::DeadlineExceeded,
    }
}

/// Drain the instance-local bounded analytics queue only after HTTP admission
/// has stopped and in-flight redirects have completed. The worker guard
/// cancels any remaining work when this independent deadline expires.
pub async fn drain_analytics(workers: AnalyticsWorkers, deadline: Duration) -> WorkerStopOutcome {
    workers.shutdown(deadline).await
}

pub async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use signal::unix::{SignalKind, signal as unix_signal};

        let terminate = unix_signal(SignalKind::terminate());
        if let Ok(mut terminate) = terminate {
            tokio::select! {
                result = signal::ctrl_c() => {
                    if result.is_err() {
                        tracing::warn!(category = "shutdown_signal_error", "could not listen for Ctrl-C");
                    }
                }
                _ = terminate.recv() => {}
            }
            return;
        }
    }

    if signal::ctrl_c().await.is_err() {
        tracing::warn!(
            category = "shutdown_signal_error",
            "could not listen for Ctrl-C"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{future::IntoFuture, sync::Arc, time::Duration};

    use axum::{extract::State, routing::get};
    use tokio::{
        io::AsyncWriteExt,
        net::{TcpListener, TcpStream},
        sync::{Semaphore, mpsc},
        time::timeout,
    };
    use tokio_util::sync::CancellationToken;

    use crate::app::ReadinessState;

    use super::{HttpDrainOutcome, stop_accepting_and_drain};

    struct HeldRequest {
        started: mpsc::UnboundedSender<()>,
        release: Arc<Semaphore>,
    }

    async fn hold_request(State(state): State<Arc<HeldRequest>>) -> &'static str {
        state.started.send(()).expect("test receiver remains open");
        state
            .release
            .acquire()
            .await
            .expect("test release gate remains open")
            .forget();
        "finished"
    }

    #[tokio::test]
    async fn active_request_drain_obeys_its_deadline_and_marks_instance_unready() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test HTTP listener binds");
        let address = listener.local_addr().expect("listener has an address");
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let app = axum::Router::new().route(
            "/held",
            get(hold_request).with_state(Arc::new(HeldRequest {
                started: started_tx,
                release: Arc::clone(&release),
            })),
        );
        let cancellation = CancellationToken::new();
        let mut server = Box::pin(
            axum::serve(listener, app)
                .with_graceful_shutdown(cancellation.clone().cancelled_owned())
                .into_future(),
        );
        let mut request_setup = Box::pin(async move {
            let mut stream = TcpStream::connect(address).await?;
            stream
                .write_all(b"GET /held HTTP/1.1\r\nHost: local.test\r\nConnection: close\r\n\r\n")
                .await?;
            started_rx
                .recv()
                .await
                .expect("handler reports that request is active");
            Ok::<TcpStream, std::io::Error>(stream)
        });
        let mut stream = timeout(Duration::from_secs(2), async {
            tokio::select! {
                result = &mut server => panic!("server exited before request start: {result:?}"),
                stream = &mut request_setup => stream.expect("request reaches handler"),
            }
        })
        .await
        .expect("active request is established before the deadline");

        let readiness = ReadinessState::new_accepting();
        assert!(readiness.is_accepting_work());
        readiness.begin_draining();
        let result =
            stop_accepting_and_drain(&cancellation, server.as_mut(), Duration::from_millis(50))
                .await;
        assert_eq!(result, HttpDrainOutcome::DeadlineExceeded);
        assert!(!readiness.is_accepting_work());

        drop(server);
        release.add_permits(1);
        let mut response = Vec::new();
        timeout(
            Duration::from_secs(2),
            tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut response),
        )
        .await
        .expect("connection cleanup completes before the deadline")
        .expect("connection closes cleanly");
    }
}
