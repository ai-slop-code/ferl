pub mod accounts;
pub mod analytics;
pub mod app;
pub mod auth;
pub mod client_ip;
pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod http;
pub mod links;
pub mod pagination;
pub mod rate_limit;
pub mod redirects;
pub mod shutdown;
pub mod tags;
pub mod telemetry;

pub const BUILD_VERSION: &str = env!("FERL_BUILD_VERSION");

#[cfg(test)]
extern crate self as ferl_server;

#[cfg(test)]
#[path = "../tests/support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[cfg(test)]
#[path = "../tests/support/database.rs"]
#[allow(dead_code)]
mod database_support;
