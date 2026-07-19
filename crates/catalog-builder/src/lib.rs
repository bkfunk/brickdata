//! Catalog builder: parses/cleans pinned upstream snapshots into
//! `catalog.sqlite` and Rust-friendly cleaned outputs (bkfunk/brickdata#3).
//! Library form exists for the integration tests.

pub mod build;
pub mod core;
pub mod fetch;
mod ldraw_part;
pub mod refresh_colors;
pub mod refresh_parts;
pub mod util;
