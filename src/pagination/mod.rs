//! Stateless authenticated keyset cursor primitives. Decoding validates a
//! boundary and request binding; it does not authenticate or authorize access.

mod codec;
mod context;

pub use codec::{
    CursorBoundary, CursorCodec, CursorError, MAX_CURSOR_ENCODED_BYTES, MAX_CURSOR_ENVELOPE_BYTES,
    MAX_CURSOR_GROUPING_VALUE_BYTES,
};
pub use context::{CursorActor, CursorContext, CursorResource};
