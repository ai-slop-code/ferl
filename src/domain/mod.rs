//! Validated domain primitives. Input strings remain separate from these types;
//! constructors are the only way to create values carrying domain invariants.

pub mod count;
pub mod ids;
pub mod secret;
pub mod short_code;
pub mod text;
pub mod time;
pub mod url;

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ValidationError(&'static str);

impl ValidationError {
    pub const fn reason(self) -> &'static str {
        self.0
    }
}

impl fmt::Debug for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ValidationError").field(&self.0).finish()
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ValidationError {}

pub(crate) const fn invalid(reason: &'static str) -> ValidationError {
    ValidationError(reason)
}
