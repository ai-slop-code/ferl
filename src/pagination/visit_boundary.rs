use crate::{
    domain::{ids::VisitId, time::Timestamp},
    pagination::CursorBoundary,
};

#[derive(Clone, Copy)]
pub(crate) struct VisitBoundary {
    pub(crate) occurred_at: Timestamp,
    pub(crate) id: VisitId,
}

impl VisitBoundary {
    pub(crate) fn from_record(record: &crate::db::records::VisitRecord) -> Result<Self, ()> {
        let unix_micros = record
            .occurred_at
            .postgres_micros()
            .checked_add(946_684_800_000_000)
            .ok_or(())?;
        let occurred_at = Timestamp::from_unix_micros(unix_micros).map_err(|_| ())?;
        Ok(Self {
            occurred_at,
            id: VisitId::new(record.id),
        })
    }

    pub(crate) fn from_cursor(boundary: CursorBoundary) -> Result<Self, ()> {
        match boundary {
            CursorBoundary::VisitOccurredAt { at, id } => Ok(Self {
                occurred_at: at,
                id,
            }),
            _ => Err(()),
        }
    }

    pub(crate) const fn into_cursor(self) -> CursorBoundary {
        CursorBoundary::VisitOccurredAt {
            at: self.occurred_at,
            id: self.id,
        }
    }
}
