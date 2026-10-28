use tracing_subscriber::{
    EnvFilter,
    filter::filter_fn,
    fmt::{self, MakeWriter},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

pub fn init_stdout() {
    init_with_writer(std::io::stdout);
}

pub fn init_stderr() {
    init_with_writer(std::io::stderr);
}

/// Install the production JSON/filter composition with an explicit output sink.
pub fn init_with_writer<W>(writer: W)
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let layer = fmt::layer()
        .json()
        .with_writer(writer)
        .with_current_span(false)
        .with_span_list(false)
        .with_target(false);
    // Global filters compose before formatting. Nesting per-layer filters here
    // can leave application events suppressed after filtered dependency spans.
    tracing_subscriber::registry()
        .with(filter)
        .with(filter_fn(|metadata| {
            is_allowlisted_target(metadata.target())
        }))
        .with(layer)
        .init();
}

/// Keep application stdout/stderr diagnostics on the explicit internal target
/// allowlist; dependency events can contain SQL, parameters, URLs, or headers.
pub fn is_allowlisted_target(target: &str) -> bool {
    target == "ferl_server" || target.starts_with("ferl_server::")
}
