//! Vendored subset of blockstar-core needed by the catalog builder
//! (bkfunk/brickdata#3). Kept item-for-item compatible with the originals
//! so the pipeline code ports with import renames only.

pub mod blob;
pub mod catalog;
pub mod categories;
pub mod colors;

pub use catalog::{CoreError, PartCatalog, PartEntry, canonical_design_id};
pub use categories::{Category, Subcategory};
