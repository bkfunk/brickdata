//! `rb_parts` table: the raw Rebrickable parts catalog. One row per
//! Rebrickable part. Source CSV columns: `part_num,name,part_cat_id,part_material`.
//!
//! Columns are named for the ID system they hold, not Rebrickable's CSV field
//! names (we rarely touch the source CSVs, and being explicit avoids confusion
//! among the many ID types):
//!
//! - `part_id_rb` (CSV `part_num`) — Rebrickable's part identifier. This is
//!   **not** the LDraw design id — most Rebrickable parts (decorated
//!   minifig/sticker/Duplo variants) have no LDraw geometry. The LDraw mapping
//!   is API-sourced (`part_crossrefs.ron`, #75).
//! - `category_id_rb` (CSV `part_cat_id`) — FK into `rb_part_categories`,
//!   indexed for category → parts lookups.
//! - `material` (CSV `part_material`) — e.g. "Plastic", "Rubber".

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

use super::rb_ingest::{self, Field, TableSpec};

pub(crate) fn build(conn: &Connection, metadata_cache: &Path) -> Result<usize> {
    rb_ingest::ingest(
        conn,
        metadata_cache,
        &TableSpec {
            table_stub: "parts",
            fields: &[
                Field::text("part_num").rename("part_id_rb").pk(),
                Field::text("name"),
                Field::opt_int("part_cat_id")
                    .rename("category_id_rb")
                    .indexed(),
                Field::opt_text("part_material").rename("material"),
            ],
        },
    )
}
