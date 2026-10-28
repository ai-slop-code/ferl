//! Production link creation and single-object management.

mod allocation;
pub(crate) mod list_repository;
mod repository;
pub mod service;

pub mod handlers;
mod list_handler;
pub mod representation;

pub use service::{ActiveLinkSnapshot, LinkFailure, LinkService, resolve_active_link};
