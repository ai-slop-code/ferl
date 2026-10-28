use crate::{client_ip::CanonicalIp, domain::ids::AccountId};

/// Identity only, not evidence of authentication or a successful charge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateSubject {
    Account(AccountId),
    Anonymous(CanonicalIp),
}
