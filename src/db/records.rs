//! Database row records. These are deliberately separate from public DTOs and
//! are not serializable; credential digests and visit payloads must not acquire
//! accidental JSON or `Debug` representations.

use chrono::{DateTime, FixedOffset, Utc};
use ipnet::IpNet;
use sqlx::{
    Decode, FromRow, Postgres, Type,
    error::BoxDynError,
    postgres::{PgTypeInfo, PgValueFormat, PgValueRef},
};
use uuid::Uuid;

pub use crate::domain::count::Count as VisitCount;

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
    pub created_at: DatabaseTimestamp,
    pub updated_at: DatabaseTimestamp,
}

/// A checked finite PostgreSQL timestamp decoded without SQLx's Chrono date
/// addition. DTO conversion performs the narrower wire-range check later.
#[derive(Clone, Copy)]
pub struct DatabaseTimestamp(i64);

impl DatabaseTimestamp {
    pub fn from_utc(value: DateTime<Utc>) -> Option<Self> {
        let micros = value.timestamp_micros().checked_sub(946_684_800_000_000)?;
        (-211_813_488_000_000_000..9_223_371_331_200_000_000)
            .contains(&micros)
            .then_some(Self(micros))
    }

    pub const fn postgres_micros(self) -> i64 {
        self.0
    }
}

impl Type<Postgres> for DatabaseTimestamp {
    fn type_info() -> PgTypeInfo {
        <DateTime<Utc> as Type<Postgres>>::type_info()
    }
}

impl<'r> Decode<'r, Postgres> for DatabaseTimestamp {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        if value.format() != PgValueFormat::Binary {
            return Err("link timestamp requires binary decoding".into());
        }
        let micros = <i64 as Decode<Postgres>>::decode(value)?;
        // PostgreSQL datetime.h finite timestamp range (in microseconds since
        // 2000-01-01); the two extreme int64 values encode infinities.
        if !(-211_813_488_000_000_000..9_223_371_331_200_000_000).contains(&micros) {
            return Err("link timestamp is outside PostgreSQL's finite range".into());
        }
        Ok(Self(micros))
    }
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

pub fn timestamp_as_utc(value: DateTime<FixedOffset>) -> DateTime<Utc> {
    value.with_timezone(&Utc)
}

#[cfg(test)]
mod tests {
    use super::{VisitCount, timestamp_as_utc};
    use chrono::{DateTime, TimeZone, Utc};

    #[test]
    fn visit_counter_conversions_reject_negative_and_overflow_values() {
        assert!(VisitCount::from_database(0).is_ok());
        assert!(VisitCount::from_database(-1).is_err());
        assert!(VisitCount::from_unsigned(u64::MAX).is_err());
        assert!(
            VisitCount::from_database(i64::MAX)
                .expect("maximum database count is valid")
                .increment()
                .is_err()
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
