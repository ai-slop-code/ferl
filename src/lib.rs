pub mod app;
pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod http;
pub mod pagination;
pub mod shutdown;
pub mod telemetry;

pub const BUILD_VERSION: &str = env!("FERL_BUILD_VERSION");
