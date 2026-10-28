use crate::pagination::CursorBoundary;

pub(crate) struct BreakdownBoundary {
    pub(crate) value: Option<String>,
}

impl BreakdownBoundary {
    pub(crate) fn from_cursor(boundary: CursorBoundary) -> Result<Self, ()> {
        match boundary {
            CursorBoundary::BreakdownValue { value } => Ok(Self { value }),
            _ => Err(()),
        }
    }

    pub(crate) fn into_cursor(self) -> CursorBoundary {
        CursorBoundary::BreakdownValue { value: self.value }
    }
}
