//! `rb_inventories` table: Rebrickable set inventories. Source CSV columns:
//! `id,version,set_num`. A set can have multiple inventory versions in the
//! source, but only the **latest (max `version`) per set** is kept — see
//! [`keep_latest_version`]. Downstream (`inventory_parts` aggregation, #72) can
//! then join straight through this table with no per-query version dedup.
//! Indexed by `set_num_rb`.
//!
//! - `inventory_id_rb` (CSV `id`) — Rebrickable's inventory id.
//! - `version` — inventory version within a set (a plain integer, not an id).
//! - `set_num_rb` (CSV `set_num`) — FK into `rb_sets.set_num_rb`. Same caveat:
//!   this is Rebrickable's set id (LEGO set number + `-<version>` suffix), not
//!   the bare LEGO set number.
//!
//! `id` and `version` are required integers, parsed strictly so an unexpectedly
//! empty field is a clear parse error rather than a NOT NULL failure at insert.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

use super::rb_ingest::{self, Field, TableSpec};

/// Ingest the CSV, then prune to the latest inventory version per set. Returns
/// the surviving row count (so the stamped `rb_inventories_count` reflects what
/// the table actually holds after dedup).
pub(crate) fn build(conn: &Connection, metadata_cache: &Path) -> Result<usize> {
    rb_ingest::ingest(
        conn,
        metadata_cache,
        &TableSpec {
            table_stub: "inventories",
            fields: &[
                Field::int("id").rename("inventory_id_rb").pk(),
                Field::int("version"),
                Field::text("set_num").rename("set_num_rb").indexed(),
            ],
        },
    )?;
    keep_latest_version(conn)
}

/// Delete every inventory that isn't its set's max-`version` row. A set ships
/// several inventory revisions over time; counting them all would multiply a
/// set's parts. Deduping here (once, at the source table) keeps that concern out
/// of every downstream query. Returns the surviving row count.
fn keep_latest_version(conn: &Connection) -> Result<usize> {
    conn.execute_batch(
        "DELETE FROM rb_inventories
         WHERE version < (
             SELECT MAX(i2.version) FROM rb_inventories i2
             WHERE i2.set_num_rb = rb_inventories.set_num_rb
         );",
    )
    .context("prune rb_inventories to latest version per set")?;
    let count = conn
        .query_row("SELECT COUNT(*) FROM rb_inventories", [], |r| {
            r.get::<_, i64>(0)
        })
        .context("count surviving rb_inventories")?;
    Ok(count as usize)
}
