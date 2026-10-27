use tokio::signal;

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
