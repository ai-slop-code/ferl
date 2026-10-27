use std::fmt;

use serde::Serialize;
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub const fn new(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(stringify!($name))
            }
        }
    };
}

id_type!(AccountId);
id_type!(LinkId);
id_type!(VisitId);

#[cfg(test)]
mod tests {
    use super::{AccountId, LinkId};
    use uuid::Uuid;

    #[test]
    fn distinct_ids_do_not_coerce_implicitly() {
        let value = Uuid::nil();
        let account = AccountId::new(value);
        let link = LinkId::new(value);
        assert_eq!(account.as_uuid(), link.as_uuid());
        assert_eq!(format!("{account:?}"), "AccountId");
        assert_eq!(format!("{link:?}"), "LinkId");
    }
}
