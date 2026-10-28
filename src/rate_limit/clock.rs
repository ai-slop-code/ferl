use sqlx::{
    Decode, Encode, Postgres, Type,
    encode::IsNull,
    error::BoxDynError,
    postgres::{PgArgumentBuffer, PgTypeInfo, PgValueFormat, PgValueRef},
};

/// Exact PostgreSQL binary microseconds since 2000-01-01. Private, finite, within
/// PostgreSQL's timestamp range. No Chrono conversion, rounding, or float math.
#[derive(Clone, Copy)]
pub(super) struct DatabaseInstant(i64);

impl Type<Postgres> for DatabaseInstant {
    fn type_info() -> PgTypeInfo {
        <chrono::DateTime<chrono::Utc> as Type<Postgres>>::type_info()
    }
}

impl Encode<'_, Postgres> for DatabaseInstant {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        <i64 as Encode<Postgres>>::encode_by_ref(&self.0, buf)
    }
    fn size_hint(&self) -> usize {
        8
    }
}

impl<'r> Decode<'r, Postgres> for DatabaseInstant {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        if value.format() != PgValueFormat::Binary {
            return Err("accounting requires binary timestamp decoding".into());
        }
        let micros = <i64 as Decode<Postgres>>::decode(value)?;
        // PostgreSQL datetime.h MIN_TIMESTAMP <= t < END_TIMESTAMP; excludes
        // infinity sentinels and prevents unsupported-range driver panics.
        if !(-211_813_488_000_000_000..9_223_371_331_200_000_000).contains(&micros) {
            return Err("invalid accounting timestamp".into());
        }
        Ok(Self(micros))
    }
}
