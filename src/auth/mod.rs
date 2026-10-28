//! Fresh PostgreSQL-backed credential authentication and transaction guards.

mod bearer;
mod guard;
mod principal;

pub use bearer::{AuthFailure, authenticate};
pub use guard::{
    AccountMutationGuard, GuardFailure, LinkAuthorityFailure, LinkMutationGuard, OwnerState,
    authorize_link_access, begin_account_mutation, begin_account_write,
    begin_anonymous_link_mutation,
};
pub use principal::{AccountPrincipal, AccountRole, ManagementPrincipal, Principal};
