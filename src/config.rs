use std::{
    collections::BTreeMap,
    env,
    net::{IpAddr, SocketAddr},
    str::FromStr,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use http::{HeaderName, HeaderValue};
use ipnet::IpNet;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

const MAX_TIMEOUT_SECS: u64 = 300;

pub struct DatabaseUrl(String);

impl DatabaseUrl {
    pub(crate) fn expose_to_driver(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for DatabaseUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[REDACTED DATABASE URL]")
    }
}

pub struct CursorKey([u8; 32]);

impl CursorKey {
    pub fn bytes_for_crypto(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for CursorKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[REDACTED CURSOR KEY]")
    }
}

pub struct TelemetryHeaders(Vec<(HeaderName, HeaderValue)>);

impl TelemetryHeaders {
    pub fn entries_for_exporter(&self) -> &[(HeaderName, HeaderValue)] {
        &self.0
    }
}

impl std::fmt::Debug for TelemetryHeaders {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[REDACTED OTLP HEADERS]")
    }
}

pub struct PublicOrigin(String);

impl PublicOrigin {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PublicOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[REDACTED PUBLIC ORIGIN]")
    }
}

pub struct DatabaseConfig {
    pub url: DatabaseUrl,
    pub pool_min_connections: u32,
    pub pool_max_connections: u32,
    pub connect_timeout: Duration,
    pub acquire_timeout: Duration,
    pub statement_timeout: Duration,
    pub lock_timeout: Duration,
}

pub struct AnalyticsConfig {
    pub queue_capacity: usize,
    pub worker_concurrency: usize,
    pub retry_budget: u32,
    pub drain_timeout: Duration,
}

pub struct ShutdownConfig {
    pub http_drain_timeout: Duration,
    pub total_timeout: Duration,
}

pub struct TelemetryConfig {
    pub endpoint: Option<Url>,
    pub headers: TelemetryHeaders,
    pub export_timeout: Duration,
    pub trace_sample_ratio: f64,
    pub resource_attributes: BTreeMap<String, String>,
    pub environment: String,
    pub instance_id: Uuid,
}

pub struct Config {
    pub database: DatabaseConfig,
    pub public_origin: PublicOrigin,
    pub listen_addr: SocketAddr,
    pub http_max_in_flight: usize,
    pub anonymous_creation: bool,
    pub anonymous_create_limit: u32,
    pub account_create_limit: u32,
    pub trusted_proxies: Vec<IpNet>,
    pub cursor_key: CursorKey,
    pub analytics: AnalyticsConfig,
    pub shutdown: ShutdownConfig,
    pub telemetry: TelemetryConfig,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Config")
            .field("database", &"[REDACTED]")
            .field("cursor_key", &"[REDACTED]")
            .field("telemetry_headers", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid configuration setting {setting}: {reason}")]
pub struct ConfigError {
    pub setting: &'static str,
    pub reason: &'static str,
}

impl ConfigError {
    const fn new(setting: &'static str, reason: &'static str) -> Self {
        Self { setting, reason }
    }
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let mut values = BTreeMap::new();
        for (name, value) in env::vars_os() {
            let Ok(name) = name.into_string() else {
                continue;
            };
            if !name.starts_with("FERL_") {
                continue;
            }
            let value = value.into_string().map_err(|_| {
                ConfigError::new("environment", "FERL settings must be valid UTF-8")
            })?;
            values.insert(name, value);
        }
        Self::from_map(values)
    }

    pub fn from_map(values: BTreeMap<String, String>) -> Result<Self, ConfigError> {
        let database_url = required(&values, "FERL_DATABASE_URL")?;
        let is_postgres_url = Url::parse(database_url)
            .is_ok_and(|parsed| matches!(parsed.scheme(), "postgres" | "postgresql"));
        if !is_postgres_url {
            return Err(ConfigError::new(
                "FERL_DATABASE_URL",
                "must be a PostgreSQL connection URL",
            ));
        }

        let public_origin = parse_public_origin(required(&values, "FERL_PUBLIC_BASE_URL")?)?;
        let listen_addr = parse_value(&values, "FERL_LISTEN_ADDR", "0.0.0.0:8080")?;
        let http_max_in_flight =
            parse_bounded(&values, "FERL_HTTP_MAX_IN_FLIGHT", 256_usize, 1, 4096)?;
        let anonymous_creation = parse_bool(&values, "FERL_ANONYMOUS_CREATION", false)?;
        let anonymous_create_limit =
            parse_bounded(&values, "FERL_ANONYMOUS_CREATE_LIMIT", 5_u32, 1, 1_000_000)?;
        let account_create_limit =
            parse_bounded(&values, "FERL_ACCOUNT_CREATE_LIMIT", 60_u32, 1, 1_000_000)?;
        let cursor_key = parse_cursor_key(required(&values, "FERL_CURSOR_KEY")?)?;
        let trusted_proxies = parse_trusted_proxies(values.get("FERL_TRUSTED_PROXIES"))?;

        let pool_max = parse_bounded(&values, "FERL_DB_POOL_MAX", 10_u32, 1, 128)?;
        let pool_min = parse_bounded(&values, "FERL_DB_POOL_MIN", 0_u32, 0, 128)?;
        if pool_min > pool_max {
            return Err(ConfigError::new(
                "FERL_DB_POOL_MIN",
                "must not exceed FERL_DB_POOL_MAX",
            ));
        }

        let database = DatabaseConfig {
            url: DatabaseUrl(database_url.to_owned()),
            pool_min_connections: pool_min,
            pool_max_connections: pool_max,
            connect_timeout: seconds(&values, "FERL_DB_CONNECT_TIMEOUT_SECS", 5)?,
            acquire_timeout: seconds(&values, "FERL_DB_ACQUIRE_TIMEOUT_SECS", 2)?,
            statement_timeout: seconds(&values, "FERL_DB_STATEMENT_TIMEOUT_SECS", 5)?,
            lock_timeout: seconds(&values, "FERL_DB_LOCK_TIMEOUT_SECS", 2)?,
        };

        let analytics = AnalyticsConfig {
            queue_capacity: parse_bounded(
                &values,
                "FERL_ANALYTICS_QUEUE_CAPACITY",
                2048_usize,
                1,
                65_536,
            )?,
            worker_concurrency: parse_bounded(&values, "FERL_ANALYTICS_WORKERS", 4_usize, 1, 64)?,
            retry_budget: parse_bounded(&values, "FERL_ANALYTICS_RETRY_BUDGET", 3_u32, 1, 10)?,
            drain_timeout: seconds(&values, "FERL_ANALYTICS_DRAIN_TIMEOUT_SECS", 5)?,
        };

        let http_drain_timeout = seconds(&values, "FERL_HTTP_DRAIN_TIMEOUT_SECS", 30)?;
        let total_timeout = seconds(&values, "FERL_SHUTDOWN_TIMEOUT_SECS", 45)?;
        if total_timeout < http_drain_timeout {
            return Err(ConfigError::new(
                "FERL_SHUTDOWN_TIMEOUT_SECS",
                "must be at least FERL_HTTP_DRAIN_TIMEOUT_SECS",
            ));
        }

        let telemetry = parse_telemetry(&values)?;

        Ok(Self {
            database,
            public_origin,
            listen_addr,
            http_max_in_flight,
            anonymous_creation,
            anonymous_create_limit,
            account_create_limit,
            trusted_proxies,
            cursor_key,
            analytics,
            shutdown: ShutdownConfig {
                http_drain_timeout,
                total_timeout,
            },
            telemetry,
        })
    }
}

fn required<'a>(
    values: &'a BTreeMap<String, String>,
    name: &'static str,
) -> Result<&'a str, ConfigError> {
    values
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(ConfigError::new(name, "is required"))
}

fn parse_value<T: FromStr>(
    values: &BTreeMap<String, String>,
    name: &'static str,
    default: &str,
) -> Result<T, ConfigError> {
    let value = values.get(name).map(String::as_str).unwrap_or(default);
    value
        .parse()
        .map_err(|_| ConfigError::new(name, "has an invalid value"))
}

fn parse_bounded<T>(
    values: &BTreeMap<String, String>,
    name: &'static str,
    default: T,
    min: T,
    max: T,
) -> Result<T, ConfigError>
where
    T: FromStr + PartialOrd,
{
    let Some(value) = values.get(name) else {
        return Ok(default);
    };
    let parsed = value
        .parse()
        .map_err(|_| ConfigError::new(name, "must be an integer within its documented bounds"))?;
    if parsed < min || parsed > max {
        return Err(ConfigError::new(
            name,
            "must be an integer within its documented bounds",
        ));
    }
    Ok(parsed)
}

fn seconds(
    values: &BTreeMap<String, String>,
    name: &'static str,
    default: u64,
) -> Result<Duration, ConfigError> {
    let seconds = parse_bounded(values, name, default, 1_u64, MAX_TIMEOUT_SECS)?;
    Ok(Duration::from_secs(seconds))
}

fn parse_bool(
    values: &BTreeMap<String, String>,
    name: &'static str,
    default: bool,
) -> Result<bool, ConfigError> {
    match values.get(name).map(String::as_str) {
        None => Ok(default),
        Some(value) if value.eq_ignore_ascii_case("true") => Ok(true),
        Some(value) if value.eq_ignore_ascii_case("false") => Ok(false),
        Some(_) => Err(ConfigError::new(name, "must be true or false")),
    }
}

fn parse_public_origin(value: &str) -> Result<PublicOrigin, ConfigError> {
    let parsed = Url::parse(value)
        .map_err(|_| ConfigError::new("FERL_PUBLIC_BASE_URL", "must be an HTTP(S) origin"))?;
    let valid_scheme = matches!(parsed.scheme(), "http" | "https");
    if !valid_scheme
        || parsed.host_str().is_none()
        || authority_has_user_info(value)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !matches!(parsed.path(), "" | "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ConfigError::new(
            "FERL_PUBLIC_BASE_URL",
            "must be an HTTP(S) origin without user info, path, query, or fragment",
        ));
    }
    Ok(PublicOrigin(parsed.origin().ascii_serialization()))
}

fn authority_has_user_info(value: &str) -> bool {
    value
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default())
        .is_some_and(|authority| authority.contains('@'))
}

fn parse_cursor_key(value: &str) -> Result<CursorKey, ConfigError> {
    let decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        ConfigError::new("FERL_CURSOR_KEY", "must be base64url for exactly 32 bytes")
    })?;
    let bytes: [u8; 32] = decoded.try_into().map_err(|_| {
        ConfigError::new("FERL_CURSOR_KEY", "must be base64url for exactly 32 bytes")
    })?;
    Ok(CursorKey(bytes))
}

fn parse_trusted_proxies(value: Option<&String>) -> Result<Vec<IpNet>, ConfigError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.trim().is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return Err(ConfigError::new(
                    "FERL_TRUSTED_PROXIES",
                    "must be a comma-separated list of IP addresses or CIDRs",
                ));
            }
            IpNet::from_str(entry)
                .or_else(|_| IpAddr::from_str(entry).map(IpNet::from))
                .map_err(|_| {
                    ConfigError::new(
                        "FERL_TRUSTED_PROXIES",
                        "must be a comma-separated list of IP addresses or CIDRs",
                    )
                })
        })
        .collect()
}

fn parse_telemetry(values: &BTreeMap<String, String>) -> Result<TelemetryConfig, ConfigError> {
    let endpoint = match values.get("FERL_OTEL_ENDPOINT") {
        None => None,
        Some(value) if value.is_empty() => None,
        Some(value) => {
            let parsed = Url::parse(value)
                .map_err(|_| ConfigError::new("FERL_OTEL_ENDPOINT", "must be an HTTP(S) URL"))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || authority_has_user_info(value)
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                return Err(ConfigError::new(
                    "FERL_OTEL_ENDPOINT",
                    "must be an HTTP(S) URL",
                ));
            }
            Some(parsed)
        }
    };
    let headers = parse_telemetry_headers(values.get("FERL_OTEL_HEADERS"))?;
    let export_timeout = seconds(values, "FERL_OTEL_TIMEOUT_SECS", 5)?;
    let trace_sample_ratio = match values.get("FERL_OTEL_TRACE_SAMPLE_RATIO") {
        None => 1.0,
        Some(value) => value
            .parse::<f64>()
            .ok()
            .filter(|ratio| ratio.is_finite() && (0.0..=1.0).contains(ratio))
            .ok_or(ConfigError::new(
                "FERL_OTEL_TRACE_SAMPLE_RATIO",
                "must be a finite number from 0 through 1",
            ))?,
    };
    let resource_attributes =
        parse_resource_attributes(values.get("FERL_OTEL_RESOURCE_ATTRIBUTES"))?;
    let environment = values
        .get("FERL_ENVIRONMENT")
        .cloned()
        .unwrap_or_else(|| "local".to_owned());
    if environment.is_empty()
        || environment.len() > 128
        || environment.chars().any(char::is_control)
    {
        return Err(ConfigError::new(
            "FERL_ENVIRONMENT",
            "must be non-empty, at most 128 bytes, and contain no control characters",
        ));
    }
    let instance_id = match values.get("FERL_INSTANCE_ID") {
        None => Uuid::new_v4(),
        Some(value) => Uuid::parse_str(value)
            .map_err(|_| ConfigError::new("FERL_INSTANCE_ID", "must be a UUID"))?,
    };

    Ok(TelemetryConfig {
        endpoint,
        headers,
        export_timeout,
        trace_sample_ratio,
        resource_attributes,
        environment,
        instance_id,
    })
}

fn parse_telemetry_headers(value: Option<&String>) -> Result<TelemetryHeaders, ConfigError> {
    let Some(value) = value else {
        return Ok(TelemetryHeaders(Vec::new()));
    };
    if value.is_empty() {
        return Ok(TelemetryHeaders(Vec::new()));
    }
    let mut headers = Vec::new();
    for pair in value.split(',') {
        let Some((name, content)) = pair.split_once('=') else {
            return Err(ConfigError::new(
                "FERL_OTEL_HEADERS",
                "must contain comma-separated name=value pairs",
            ));
        };
        if name.is_empty() || content.is_empty() {
            return Err(ConfigError::new(
                "FERL_OTEL_HEADERS",
                "must contain comma-separated name=value pairs",
            ));
        }
        let name = HeaderName::from_str(name).map_err(|_| {
            ConfigError::new(
                "FERL_OTEL_HEADERS",
                "contains an invalid header name or value",
            )
        })?;
        let content = HeaderValue::from_str(content).map_err(|_| {
            ConfigError::new(
                "FERL_OTEL_HEADERS",
                "contains an invalid header name or value",
            )
        })?;
        headers.push((name, content));
    }
    Ok(TelemetryHeaders(headers))
}

fn parse_resource_attributes(
    value: Option<&String>,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let mut attributes = BTreeMap::new();
    let Some(value) = value else {
        return Ok(attributes);
    };
    if value.is_empty() {
        return Ok(attributes);
    }
    for pair in value.split(',') {
        let Some((key, content)) = pair.split_once('=') else {
            return Err(ConfigError::new(
                "FERL_OTEL_RESOURCE_ATTRIBUTES",
                "must contain comma-separated key=value pairs",
            ));
        };
        if key.is_empty()
            || content.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            || attributes
                .insert(key.to_owned(), content.to_owned())
                .is_some()
        {
            return Err(ConfigError::new(
                "FERL_OTEL_RESOURCE_ATTRIBUTES",
                "contains an invalid or duplicate key/value pair",
            ));
        }
    }
    Ok(attributes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_values() -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "FERL_DATABASE_URL".to_owned(),
                "postgres://user:secret@localhost/ferl".to_owned(),
            ),
            (
                "FERL_PUBLIC_BASE_URL".to_owned(),
                "https://links.example/".to_owned(),
            ),
            (
                "FERL_CURSOR_KEY".to_owned(),
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
            ),
        ])
    }

    #[test]
    fn defaults_are_concrete_and_origin_is_normalized() {
        let config = Config::from_map(valid_values()).expect("valid configuration");
        assert_eq!(config.public_origin.as_str(), "https://links.example");
        assert_eq!(
            config.listen_addr,
            "0.0.0.0:8080".parse().expect("socket address")
        );
        assert_eq!(config.http_max_in_flight, 256);
        assert!(!config.anonymous_creation);
        assert_eq!(config.anonymous_create_limit, 5);
        assert_eq!(config.account_create_limit, 60);
        assert!(config.trusted_proxies.is_empty());
        assert_eq!(config.database.pool_max_connections, 10);
        assert_eq!(config.telemetry.trace_sample_ratio, 1.0);
    }

    #[test]
    fn rejects_non_origin_urls_including_empty_user_info() {
        for value in [
            "https://user:pass@example.com",
            "https://@example.com",
            "https://example.com/prefix",
            "https://example.com?x=1",
            "ftp://example.com",
        ] {
            let mut values = valid_values();
            values.insert("FERL_PUBLIC_BASE_URL".to_owned(), value.to_owned());
            assert_eq!(
                Config::from_map(values)
                    .expect_err("origin must be rejected")
                    .setting,
                "FERL_PUBLIC_BASE_URL"
            );
        }
    }

    #[test]
    fn validates_secrets_and_never_echoes_them_in_errors() {
        let mut values = valid_values();
        values.insert("FERL_CURSOR_KEY".to_owned(), "not-the-key".to_owned());
        let error = Config::from_map(values).expect_err("invalid key must fail");
        assert_eq!(error.setting, "FERL_CURSOR_KEY");
        assert!(!error.to_string().contains("not-the-key"));

        let mut values = valid_values();
        values.insert(
            "FERL_DATABASE_URL".to_owned(),
            "postgres://alice:pw-secret@db/ferl".to_owned(),
        );
        values.insert("FERL_DB_POOL_MAX".to_owned(), "0".to_owned());
        let error = Config::from_map(values).expect_err("zero pool size must fail");
        assert_eq!(error.setting, "FERL_DB_POOL_MAX");
        assert!(!error.to_string().contains("pw-secret"));
    }

    #[test]
    fn validates_proxy_ranges_and_bounded_settings() {
        let mut values = valid_values();
        values.insert(
            "FERL_TRUSTED_PROXIES".to_owned(),
            "192.0.2.0/24,2001:db8::1".to_owned(),
        );
        values.insert("FERL_HTTP_MAX_IN_FLIGHT".to_owned(), "0".to_owned());
        let error = Config::from_map(values).expect_err("zero HTTP concurrency must fail");
        assert_eq!(error.setting, "FERL_HTTP_MAX_IN_FLIGHT");

        let mut values = valid_values();
        values.insert(
            "FERL_TRUSTED_PROXIES".to_owned(),
            "192.0.2.1,not-an-ip".to_owned(),
        );
        let error = Config::from_map(values).expect_err("malformed proxy must fail");
        assert_eq!(error.setting, "FERL_TRUSTED_PROXIES");
    }

    #[test]
    fn validates_telemetry_settings_and_redacts_export_headers() {
        let mut values = valid_values();
        values.insert(
            "FERL_OTEL_ENDPOINT".to_owned(),
            "https://collector.example/otlp".to_owned(),
        );
        values.insert(
            "FERL_OTEL_HEADERS".to_owned(),
            "authorization=collector-secret".to_owned(),
        );
        values.insert("FERL_OTEL_TRACE_SAMPLE_RATIO".to_owned(), "0.25".to_owned());
        values.insert(
            "FERL_OTEL_RESOURCE_ATTRIBUTES".to_owned(),
            "deployment=local,team=backend".to_owned(),
        );
        let config = Config::from_map(values.clone()).expect("valid telemetry configuration");
        assert_eq!(config.telemetry.trace_sample_ratio, 0.25);
        assert_eq!(config.telemetry.headers.entries_for_exporter().len(), 1);
        assert!(!format!("{:?}", config.telemetry.headers).contains("collector-secret"));
        assert_eq!(config.telemetry.resource_attributes["deployment"], "local");

        values.insert("FERL_OTEL_TRACE_SAMPLE_RATIO".to_owned(), "1.01".to_owned());
        let error = Config::from_map(values).expect_err("sampling ratio above one is invalid");
        assert_eq!(error.setting, "FERL_OTEL_TRACE_SAMPLE_RATIO");
        assert!(!error.to_string().contains("1.01"));

        let mut values = valid_values();
        values.insert(
            "FERL_OTEL_ENDPOINT".to_owned(),
            "https://@collector.example".to_owned(),
        );
        let error = Config::from_map(values).expect_err("endpoint user info is invalid");
        assert_eq!(error.setting, "FERL_OTEL_ENDPOINT");
    }

    #[test]
    fn configuration_debug_redacts_secret_material() {
        let database = DatabaseUrl("postgres://user:password@host/db".to_owned());
        let cursor = CursorKey([42; 32]);
        assert!(!format!("{database:?}").contains("password"));
        assert!(!format!("{cursor:?}").contains("42"));

        let config = Config::from_map(valid_values()).expect("valid configuration");
        let output = format!("{config:?}");
        assert!(!output.contains("links.example"));
        assert!(!output.contains("postgres://"));
        assert_eq!(config.cursor_key.bytes_for_crypto().len(), 32);
        assert!(config.telemetry.headers.entries_for_exporter().is_empty());
    }
}
