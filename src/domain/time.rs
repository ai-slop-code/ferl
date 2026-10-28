use std::{fmt, str::FromStr};

use chrono::{DateTime, Datelike, SecondsFormat, Utc};
use serde::{Serialize, Serializer};

use super::{ValidationError, invalid};

/// PostgreSQL `timestamptz` has microsecond precision. All request filters,
/// persisted timestamps, DTOs, and cursor boundaries are normalized to the
/// same microsecond instant (truncation toward the start of the UTC second).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TimeInterval {
    from: Option<Timestamp>,
    to: Option<Timestamp>,
}

impl Timestamp {
    pub fn parse_rfc3339(input: &str) -> Result<Self, ValidationError> {
        let parsed = DateTime::parse_from_rfc3339(input)
            .map_err(|_| invalid("timestamp must be RFC 3339 with an explicit offset"))?;
        let utc = parsed.with_timezone(&Utc);
        if utc.timestamp_subsec_nanos() >= 1_000_000_000 {
            return Err(invalid("leap-second timestamps are unsupported"));
        }
        ensure_wire_year(utc.year())?;
        let micros = utc.timestamp_micros();
        DateTime::from_timestamp_micros(micros)
            .ok_or(invalid("timestamp is outside the supported range"))?;
        Ok(Self(micros))
    }

    pub fn from_database(value: DateTime<Utc>) -> Result<Self, ValidationError> {
        Self::from_unix_micros(value.timestamp_micros())
            .map_err(|_| invalid("stored timestamp is outside the supported range"))
    }

    pub fn from_unix_micros(value: i64) -> Result<Self, ValidationError> {
        let timestamp = DateTime::from_timestamp_micros(value)
            .ok_or(invalid("timestamp is outside the supported range"))?;
        ensure_wire_year(timestamp.year())?;
        Ok(Self(value))
    }

    pub const fn unix_micros(self) -> i64 {
        self.0
    }

    pub fn as_utc(self) -> DateTime<Utc> {
        DateTime::from_timestamp_micros(self.0).expect("timestamp invariant was checked")
    }

    pub fn to_wire(self) -> String {
        self.as_utc().to_rfc3339_opts(SecondsFormat::Micros, true)
    }
}

fn ensure_wire_year(year: i32) -> Result<(), ValidationError> {
    if (0..=9_999).contains(&year) {
        Ok(())
    } else {
        Err(invalid("timestamp is outside the RFC 3339 wire range"))
    }
}

impl TimeInterval {
    pub fn new(from: Option<Timestamp>, to: Option<Timestamp>) -> Result<Self, ValidationError> {
        if matches!((from, to), (Some(start), Some(end)) if start >= end) {
            return Err(invalid("time interval requires from < to"));
        }
        Ok(Self { from, to })
    }

    pub const fn from(self) -> Option<Timestamp> {
        self.from
    }

    pub const fn to(self) -> Option<Timestamp> {
        self.to
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_wire())
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Timestamp([UTC microseconds])")
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_wire())
    }
}

impl FromStr for Timestamp {
    type Err = ValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse_rfc3339(value)
    }
}

impl fmt::Debug for TimeInterval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TimeInterval")
    }
}

#[cfg(test)]
mod tests {
    use super::{TimeInterval, Timestamp};

    #[test]
    fn explicit_offsets_are_utc_normalized_and_microsecond_precise() {
        let plus = Timestamp::parse_rfc3339("2026-10-27T12:00:00.123456789+02:00").unwrap();
        let utc = Timestamp::parse_rfc3339("2026-10-27T10:00:00.123456Z").unwrap();
        assert_eq!(plus, utc);
        assert_eq!(plus.to_wire(), "2026-10-27T10:00:00.123456Z");
        assert!(serde_json::to_string(&plus).unwrap().ends_with("Z\""));
    }

    #[test]
    fn rejects_missing_offsets_bad_ranges_and_invalid_intervals() {
        for value in [
            "2026-10-27T10:00:00",
            "not-a-date",
            "999999-01-01T00:00:00Z",
            "2026-12-31T23:59:60Z",
        ] {
            assert!(Timestamp::parse_rfc3339(value).is_err(), "{value}");
        }
        let a = Timestamp::parse_rfc3339("2026-01-01T00:00:00Z").unwrap();
        let b = Timestamp::parse_rfc3339("2026-01-01T00:00:01Z").unwrap();
        assert!(TimeInterval::new(Some(a), Some(b)).is_ok());
        assert!(TimeInterval::new(Some(a), Some(a)).is_err());
        assert!(TimeInterval::new(Some(b), Some(a)).is_err());
    }
}
