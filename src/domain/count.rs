use std::fmt;

use serde::Serialize;

use super::{ValidationError, invalid};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Count(i64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct PositiveCount(Count);

impl Count {
    pub fn from_database(value: i64) -> Result<Self, ValidationError> {
        (value >= 0)
            .then_some(Self(value))
            .ok_or(invalid("count is negative"))
    }

    pub fn from_unsigned(value: u64) -> Result<Self, ValidationError> {
        i64::try_from(value)
            .map(Self)
            .map_err(|_| invalid("count exceeds signed 64-bit range"))
    }

    pub const fn database_value(self) -> i64 {
        self.0
    }

    pub fn increment(self) -> Result<Self, ValidationError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(invalid("count exceeds signed 64-bit range"))
    }
}

impl PositiveCount {
    pub fn from_database(value: i64) -> Result<Self, ValidationError> {
        if value <= 0 {
            return Err(invalid("positive count must be greater than zero"));
        }
        Ok(Self(Count::from_database(value)?))
    }

    pub fn as_count(self) -> Count {
        self.0
    }
}

impl fmt::Debug for Count {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Count")
    }
}

impl fmt::Debug for PositiveCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PositiveCount")
    }
}

#[cfg(test)]
mod tests {
    use super::{Count, PositiveCount};

    #[test]
    fn signed_database_limits_and_positive_tag_count_are_checked() {
        assert!(Count::from_database(0).is_ok());
        assert!(Count::from_database(i64::MAX).is_ok());
        assert!(Count::from_database(-1).is_err());
        assert!(Count::from_unsigned(u64::MAX).is_err());
        assert!(Count::from_database(i64::MAX).unwrap().increment().is_err());
        assert!(PositiveCount::from_database(1).is_ok());
        assert!(PositiveCount::from_database(0).is_err());
        assert!(PositiveCount::from_database(-1).is_err());
    }
}
