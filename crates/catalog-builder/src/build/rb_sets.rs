//! `rb_sets` table: Rebrickable sets. Source CSV columns:
//! `set_num,name,year,theme_id,num_parts,img_url`. Consumed by the inventory
//! aggregation (#72) for set name/year.
//!
//! - `set_num_rb` (CSV `set_num`) — Rebrickable's set identifier. It's the
//!   LEGO set number plus Rebrickable's `-<version>` suffix (e.g. `8880-1`),
//!   so it's **not** the bare LEGO set number; the `_rb` suffix marks that.
//! - `theme_id_rb` (CSV `theme_id`) — FK into `rb_themes`. Not indexed: sets
//!   are reached by their PK (from the inventory join), and nothing yet queries
//!   sets by theme — add `.indexed()` if a theme → sets lookup appears.
//! - `img_url` may be empty -> NULL.
//! - `set_id` (added by [`add_set_ids`], not from the CSV) — a dense `u32`
//!   assigned at build time, the compact join key `rb_part_color_set`
//!   references instead of the wide `set_num_rb` string (#72).

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::path::Path;

use super::rb_ingest::{self, Field, TableSpec};

pub(crate) fn build(conn: &Connection, metadata_cache: &Path) -> Result<usize> {
    rb_ingest::ingest(
        conn,
        metadata_cache,
        &TableSpec {
            table_stub: "sets",
            fields: &[
                Field::text("set_num").rename("set_num_rb").pk(),
                Field::text("name"),
                Field::opt_int("year"),
                Field::opt_int("theme_id").rename("theme_id_rb"),
                Field::opt_int("num_parts"),
                Field::opt_text("img_url"),
            ],
        },
    )
}

/// Add the dense `set_id` column and assign one per set. `set_id` is the
/// compact build-time id `rb_part_color_set` joins on (instead of the wide
/// `set_num_rb` string); see #72.
///
/// Why not just use SQLite's rowid? These tables are `WITHOUT ROWID` (keyed by
/// `set_num_rb`), so there is no implicit rowid to borrow. Even on a rowid
/// table, an *implicit* rowid isn't a stable key to reference from ~1M fact
/// rows (`VACUUM` can renumber it). `set_id` is an explicit 4-byte surrogate: it
/// shrinks every `rb_part_color_set` row versus repeating the `set_num_rb` text
/// (e.g. `"8880-1"`), and makes the fact→set join an integer compare.
///
/// Ids are assigned 1..=N ordered by `set_num_rb`, so the same snapshot always
/// yields the same ids — required for the reproducible-build guarantee (#73).
/// The column is declared `NOT NULL DEFAULT 0` (SQLite can't add a bare
/// `NOT NULL` column to a populated table); every row is then updated to a
/// distinct positive id, so the transient default never survives and the
/// `UNIQUE` index holds.
pub(crate) fn add_set_ids(conn: &Connection) -> Result<()> {
    conn.execute_batch("ALTER TABLE rb_sets ADD COLUMN set_id INTEGER NOT NULL DEFAULT 0")
        .context("add rb_sets.set_id column")?;

    let set_nums: Vec<String> = {
        let mut stmt = conn.prepare("SELECT set_num_rb FROM rb_sets ORDER BY set_num_rb")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<Vec<String>>>()
            .context("read rb_sets keys for set_id assignment")?
    };

    let tx = conn.unchecked_transaction()?;
    {
        let mut upd = tx.prepare("UPDATE rb_sets SET set_id = ?1 WHERE set_num_rb = ?2")?;
        for (i, set_num) in set_nums.iter().enumerate() {
            let set_id = i64::try_from(i + 1).expect("set count fits in i64");
            upd.execute(params![set_id, set_num])
                .with_context(|| format!("assign set_id to {set_num}"))?;
        }
    }
    tx.commit().context("commit set_id assignment")?;

    conn.execute_batch("CREATE UNIQUE INDEX idx_rb_sets_set_id ON rb_sets(set_id)")
        .context("index rb_sets.set_id")?;
    Ok(())
}
