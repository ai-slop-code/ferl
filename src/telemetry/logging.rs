use tracing_subscriber::{
    EnvFilter,
    filter::filter_fn,
    fmt,
    layer::{Layer, SubscriberExt},
    util::SubscriberInitExt,
};

pub fn init_stdout() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let layer = fmt::layer()
        .json()
        .with_current_span(false)
        .with_span_list(false)
        .with_target(false)
        .with_filter(filter)
        .with_filter(filter_fn(|metadata| {
            metadata.target() == "ferl_server" || metadata.target().starts_with("ferl_server::")
        }));
    tracing_subscriber::registry().with(layer).init();
}
