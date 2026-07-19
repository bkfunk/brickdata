//! `rb_part_categories` table: Rebrickable part-category names (a tiny lookup
//! companion to `rb_parts`). Source CSV columns: `id,name`.
//!
//! - `category_id_rb` (CSV `id`) — Rebrickable's part-category id, the target
//!   of `rb_parts.category_id_rb`.

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

use super::rb_ingest::{self, Field, TableSpec};

pub(crate) fn build(conn: &Connection, metadata_cache: &Path) -> Result<usize> {
    rb_ingest::ingest(
        conn,
        metadata_cache,
        &TableSpec {
            table_stub: "part_categories",
            fields: &[
                Field::int("id").rename("category_id_rb").pk(),
                Field::text("name"),
            ],
        },
    )
}
