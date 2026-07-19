//! `rb_elements` table: the LEGO element layer — a part in a specific color,
//! with its boxed/marketing element ID. Source CSV columns:
//! `element_id,part_num,color_id,design_id`.
//!
//! This is the **only** source for the `element_id -> (part, color)` mapping
//! (it's not in any other CSV and is otherwise only reachable via per-part+color
//! API calls), so it powers element-ID search: a user types an element number
//! from an instruction booklet and we surface that part in that color. Indexed
//! by `part_id_rb` for the reverse lookup; `element_id` is the PK.
//!
//! Column naming (explicit about which ID system each holds):
//! - `element_id` (CSV `element_id`) — the real 6–7 digit **LEGO** element
//!   number, so it keeps the bare LEGO name (no `_rb` suffix).
//! - `part_id_rb` (CSV `part_num`) — Rebrickable's part id (→ `rb_parts`).
//! - `color_id_rb` (CSV `color_id`) — Rebrickable's color id.
//! - `design_id_rb` (CSV `design_id`) — ⚠️ Rebrickable's *LEGO design number*
//!   field (often empty). It is **not** the LDraw id and must not be joined to
//!   `ldraw_part`; the `_rb` suffix keeps it from being mistaken for one.

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;

use super::rb_ingest::{self, Field, TableSpec};

pub(crate) fn build(conn: &Connection, metadata_cache: &Path) -> Result<usize> {
    rb_ingest::ingest(
        conn,
        metadata_cache,
        &TableSpec {
            table_stub: "elements",
            fields: &[
                Field::text("element_id").pk(),
                Field::text("part_num").rename("part_id_rb").indexed(),
                Field::opt_int("color_id").rename("color_id_rb"),
                Field::opt_text("design_id").rename("design_id_rb"),
            ],
        },
    )
}
