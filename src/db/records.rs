//! Database row records. These are deliberately separate from public DTOs and
//! are not serializable; credential digests and visit payloads must not acquire
//! accidental JSON or `Debug` representations.

use chrono::{DateTime, FixedOffset, Utc};
use ipnet::IpNet;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(FromRow)]
pub struct SystemStateRecord {
    pub singleton: bool,
    pub bootstrap_completed: bool,
}

#[derive(FromRow)]
pub struct AccountRecord {
    pub id: Uuid,
    pub username: String,
    pub role: String,
    pub enabled: bool,
    pub api_key_digest: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(FromRow)]
pub struct CodeReservationRecord {
    pub short_code: String,
}

#[derive(FromRow)]
pub struct LinkRecord {
    pub id: Uuid,
    pub short_code: String,
    pub owner_id: Option<Uuid>,
    pub destination_url: String,
    pub destination_search: String,
    pub title: Option<String>,
    pub title_search: Option<String>,
    pub enabled: bool,
    pub visit_count: i64,
    pub management_token_digest: Option<Vec<u8>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(FromRow)]
pub struct LinkTagRecord {
    pub link_id: Uuid,
    pub name: String,
}

#[derive(FromRow)]
pub struct VisitRecord {
    pub id: Uuid,
    pub link_id: Uuid,
    pub redirect_url: String,
    pub occurred_at: DateTime<Utc>,
    pub method: String,
    pub client_ip: IpNet,
    pub referrer: Option<String>,
    pub user_agent: Option<String>,
    pub device: String,
    pub browser: String,
    pub os: String,
}

#[derive(FromRow)]
pub struct RateBucketRecord {
    pub id: Uuid,
    pub bucket_kind: String,
    pub account_id: Option<Uuid>,
    pub client_ip: Option<IpNet>,
    pub created_at: DateTime<Utc>,
}

#[derive(FromRow)]
pub struct RateChargeRecord {
    pub id: Uuid,
    pub bucket_id: Uuid,
    pub charged_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisitCount(i64);

impl VisitCount {
    pub fn from_database(value: i64) -> Result<Self, CounterConversionError> {
        (value >= 0)
            .then_some(Self(value))
            .ok_or(CounterConversionError)
    }

    pub fn from_unsigned(value: u64) -> Result<Self, CounterConversionError> {
        i64::try_from(value)
            .map(Self)
            .map_err(|_| CounterConversionError)
    }

    pub const fn database_value(self) -> i64 {
        self.0
    }

    pub fn increment(self) -> Result<Self, CounterConversionError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(CounterConversionError)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CounterConversionError;

pub fn timestamp_as_utc(value: DateTime<FixedOffset>) -> DateTime<Utc> {
    value.with_timezone(&Utc)
}

#[cfg(test)]
mod tests {
    use super::{CounterConversionError, VisitCount, timestamp_as_utc};
    use chrono::{DateTime, TimeZone, Utc};

    #[test]
    fn visit_counter_conversions_reject_negative_and_overflow_values() {
        assert!(VisitCount::from_database(0).is_ok());
        assert_eq!(VisitCount::from_database(-1), Err(CounterConversionError));
        assert_eq!(
            VisitCount::from_unsigned(u64::MAX),
            Err(CounterConversionError)
        );
        assert_eq!(
            VisitCount::from_database(i64::MAX)
                .expect("maximum database count is valid")
                .increment(),
            Err(CounterConversionError)
        );
    }

    #[test]
    fn offset_timestamps_are_explicitly_normalized_to_utc() {
        let timestamp = DateTime::parse_from_rfc3339("2026-10-27T12:00:00+02:00")
            .expect("fixture has a valid RFC 3339 offset");
        let utc = timestamp_as_utc(timestamp);
        assert_eq!(utc, Utc.with_ymd_and_hms(2026, 10, 27, 10, 0, 0).unwrap());
    }
}
