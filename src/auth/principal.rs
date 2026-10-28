use std::fmt;

use crate::domain::{
    ids::{AccountId, LinkId},
    secret::SecretDigest,
};

/// A role read from the primary database for this request. It is not a session
/// claim and is never cached between requests.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AccountRole {
    User,
    Admin,
}

#[derive(Clone)]
pub struct AccountPrincipal {
    id: AccountId,
    role: AccountRole,
    digest: SecretDigest,
}

#[derive(Clone)]
pub struct ManagementPrincipal {
    link_id: LinkId,
    digest: SecretDigest,
}

#[derive(Clone)]
pub enum Principal {
    Account(AccountPrincipal),
    ManagementToken(ManagementPrincipal),
}

impl AccountPrincipal {
    pub const fn id(&self) -> AccountId {
        self.id
    }

    pub const fn role(&self) -> AccountRole {
        self.role
    }

    pub(crate) fn digest(&self) -> &SecretDigest {
        &self.digest
    }

    pub(crate) fn from_database(id: AccountId, role: AccountRole, digest: SecretDigest) -> Self {
        Self { id, role, digest }
    }
}

impl ManagementPrincipal {
    pub const fn link_id(&self) -> LinkId {
        self.link_id
    }

    pub(crate) fn digest(&self) -> &SecretDigest {
        &self.digest
    }

    pub(crate) fn from_database(link_id: LinkId, digest: SecretDigest) -> Self {
        Self { link_id, digest }
    }
}

impl Principal {
    pub fn account(&self) -> Option<&AccountPrincipal> {
        match self {
            Self::Account(account) => Some(account),
            Self::ManagementToken(_) => None,
        }
    }

    pub fn management_token(&self) -> Option<&ManagementPrincipal> {
        match self {
            Self::Account(_) => None,
            Self::ManagementToken(token) => Some(token),
        }
    }
}

impl AccountRole {
    pub const fn is_admin(self) -> bool {
        matches!(self, Self::Admin)
    }

    pub(crate) fn parse_database(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }
}

macro_rules! redacted_debug {
    ($name:ty, $text:literal) => {
        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str($text)
            }
        }
    };
}

redacted_debug!(AccountRole, "AccountRole");
redacted_debug!(AccountPrincipal, "AccountPrincipal([AUTHENTICATED])");
redacted_debug!(ManagementPrincipal, "ManagementPrincipal([AUTHENTICATED])");
redacted_debug!(Principal, "Principal([AUTHENTICATED])");
