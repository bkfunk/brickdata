//! `rb_themes` table: Rebrickable set themes (a tiny lookup companion to
//! `rb_sets`). Source CSV columns: `id,name,parent_id`.
//!
//! - `theme_id_rb` (CSV `id`) — Rebrickable's theme id (→ `rb_sets.theme_id_rb`).
//! - `parent_theme_id_rb` (CSV `parent_id`) — parent theme; empty for a
//!   top-level theme -> NULL.

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

use super::rb_ingest::{self, Field, TableSpec};

pub(crate) fn build(conn: &Connection, metadata_cache: &Path) -> Result<usize> {
    rb_ingest::ingest(
        conn,
        metadata_cache,
        &TableSpec {
            table_stub: "themes",
            fields: &[
                Field::int("id").rename("theme_id_rb").pk(),
                Field::text("name"),
                Field::opt_int("parent_id").rename("parent_theme_id_rb"),
            ],
        },
    )
}
