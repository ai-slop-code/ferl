//! Reusable checked link representation construction. Part 07 must not invent
//! a second DTO mapping or infer owner state from link state.

use crate::{
    config::PublicOrigin,
    db::records::LinkRecord,
    http::dto::{LinkDto, RecordConversionError},
};

pub fn build(
    record: &LinkRecord,
    canonical_tag_names: Vec<String>,
    origin: &PublicOrigin,
    owner_enabled: Option<bool>,
) -> Result<LinkDto, RecordConversionError> {
    LinkDto::from_record(record, canonical_tag_names, origin, owner_enabled)
}
