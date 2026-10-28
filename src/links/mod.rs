//! Production link creation and single-object management.

mod allocation;
mod repository;
pub mod service;

pub mod handlers;
pub mod representation;

pub use service::{ActiveLinkSnapshot, LinkFailure, LinkService, resolve_active_link};
