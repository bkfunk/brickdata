//! Final build slice (#73): the `part` view, the `part_fts` full-text index,
//! and the closing `ANALYZE`/`VACUUM` pass.
//!
//! ## The `part` view
//!
//! `ldraw_part` LEFT JOINed to `rb_part_summary` — the one relation the
//! runtime browses. A part in our LDraw library that appears in no
//! Rebrickable inventory gets NULL summary columns; that is a valid state
//! (an LDraw-only part), not an error.
//!
//! ## `part_fts`
//!
//! An FTS5 table over four columns:
//!
//! - `design_id` — so part-number searches hit.
//! - `ldraw_name` — the LDraw geometry-based name.
//! - `rb_name` — the Rebrickable appearance-based name(s), sourced at build
//!   time by translating `rb_parts` part numbers through the shared
//!   [`PartResolver`](super::resolve) (pin + literal fallback, #112) to
//!   `rb_parts.name`; NULL when the design has no Rebrickable mapping. When
//!   several part_nums map to one design their names are space-joined (in
//!   part_num order) so every token is searchable.
//! - `alt_ids` — other part numbers that resolve to this design: retired
//!   (`~Moved to`) ids and hard-alias ids, space-joined. A search for an old
//!   or alias number (`4073`, `30071`) finds the current design.
//!
//! No LEGO marketing name is indexed — Rebrickable doesn't expose it (see
//! `docs/lego-reference/ldraw-part-numbering.md`). The tokenizer is
//! `unicode61` with `tokenchars '-'` so ids like `3040b` and `92693c01`
//! (and `-fN`-ish tails) tokenize as single searchable terms.
//!
//! ## Determinism
//!
//! Everything here is derived from already-deterministic tables in a fixed
//! order (`ORDER BY` on the FTS fill, `BTreeMap` for the name aggregation),
//! so re-running the whole build on the same pins yields a byte-identical
//! file — the M2 acceptance. That is also why there is **no** wall-clock
//! `build_completed_at` stamp: it would contradict reproducibility, and the
//! pinned `snapshot_date` / `part_mappings_date` already identify the build.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::BTreeMap;

use super::resolve::PartResolver;

/// Row counts stamped into `meta` by the build driver.
pub(crate) struct FinalizeStats {
    pub part_view_rows: i64,
    pub fts_rows: i64,
}

impl FinalizeStats {
    pub(crate) fn meta_rows(&self) -> [(&'static str, String); 2] {
        [
            ("total_part_view_rows", self.part_view_rows.to_string()),
            ("fts_row_count", self.fts_rows.to_string()),
        ]
    }
}

/// Create the `part` view and build `part_fts`. Runs after every table
/// slice; returns the counts for `meta`.
pub(crate) fn run(conn: &Connection, resolver: &PartResolver<'_>) -> Result<FinalizeStats> {
    create_part_view(conn)?;
    build_fts(conn, resolver)?;

    let count = |sql: &str| -> Result<i64> {
        conn.query_row(sql, [], |r| r.get(0))
            .with_context(|| format!("count via {sql:?}"))
    };
    Ok(FinalizeStats {
        part_view_rows: count("SELECT COUNT(*) FROM part")?,
        fts_rows: count("SELECT COUNT(*) FROM part_fts")?,
    })
}

/// `ANALYZE` then `VACUUM`, in that order — VACUUM last because it rewrites
/// the whole file. Split from [`run`] so the driver can stamp the row counts
/// between the two steps and `build_status` after both (the very last write,
/// making a half-built DB detectable by its missing/non-`complete` status).
pub(crate) fn optimize(conn: &Connection) -> Result<()> {
    conn.execute_batch("ANALYZE;").context("ANALYZE")?;
    conn.execute_batch("VACUUM;").context("VACUUM")?;
    Ok(())
}

fn create_part_view(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE VIEW part AS
         SELECT
             lp.design_id,
             lp.name AS ldraw_name,
             lp.subcategory_id,
             lp.category_id,
             lp.dimensions,
             lp.is_decorated,
             lp.flexion_variants,
             lp.has_base_file,
             s.set_count,
             s.qty_sum,
             s.year_min,
             s.year_max
         FROM ldraw_part lp
         LEFT JOIN rb_part_summary s ON s.design_id = lp.design_id;",
    )
    .context("create part view")
}

/// Create and fill `part_fts` in one `INSERT … SELECT` over `ldraw_part`
/// LEFT JOINed to a temp `design_id → rb_name` table (aggregated in Rust
/// from `rb_parts` via the shared resolver). The temp table lives in
/// SQLite's temp database, never in the output file.
fn build_fts(conn: &Connection, resolver: &PartResolver<'_>) -> Result<()> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE part_fts USING fts5(
             design_id,
             ldraw_name,
             rb_name,
             alt_ids,
             tokenize = \"unicode61 tokenchars '-'\"
         );
         CREATE TEMP TABLE rb_names (
             design_id TEXT PRIMARY KEY,
             rb_name   TEXT NOT NULL
         );
         CREATE TEMP TABLE alt_ids (
             design_id TEXT PRIMARY KEY,
             ids       TEXT NOT NULL
         );",
    )
    .context("create part_fts / temp tables")?;

    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt =
            tx.prepare("INSERT INTO temp.rb_names (design_id, rb_name) VALUES (?1, ?2)")?;
        for (design_id, rb_name) in rb_names_by_design(&tx, resolver)? {
            stmt.execute(rusqlite::params![design_id, rb_name])
                .with_context(|| format!("insert temp rb_names {design_id}"))?;
        }
        let mut stmt = tx.prepare("INSERT INTO temp.alt_ids (design_id, ids) VALUES (?1, ?2)")?;
        for (design_id, sources) in resolver.redirect_sources_by_target() {
            stmt.execute(rusqlite::params![design_id, sources.join(" ")])
                .with_context(|| format!("insert temp alt_ids {design_id}"))?;
        }
    }
    tx.commit().context("commit temp FTS sources")?;

    // ORDER BY pins the FTS rowid assignment (hence the file bytes) to the
    // design-id order, independent of the scan plan.
    conn.execute_batch(
        "INSERT INTO part_fts (design_id, ldraw_name, rb_name, alt_ids)
         SELECT lp.design_id, lp.name, rn.rb_name, ai.ids
         FROM ldraw_part lp
         LEFT JOIN temp.rb_names rn ON rn.design_id = lp.design_id
         LEFT JOIN temp.alt_ids ai ON ai.design_id = lp.design_id
         ORDER BY lp.design_id;
         DROP TABLE temp.rb_names;
         DROP TABLE temp.alt_ids;",
    )
    .context("populate part_fts")
}

/// Aggregate `design_id → space-joined Rebrickable name(s)`: every `rb_parts`
/// row whose part_num the resolver maps (pin or literal fallback, #112) to an
/// **in-catalog** design contributes its name, in part_num order
/// (deterministic, and `BTreeMap` keeps the design order stable too). The
/// in-catalog gate matters: the FTS fill LEFT JOINs *from* `ldraw_part`, so a
/// name keyed on an absent design could never join — it would only bloat the
/// temp table. Designs with no mapped Rebrickable part are simply absent.
fn rb_names_by_design(
    conn: &Connection,
    resolver: &PartResolver<'_>,
) -> Result<BTreeMap<String, String>> {
    let mut stmt = conn
        .prepare("SELECT part_id_rb, name FROM rb_parts ORDER BY part_id_rb")
        .context("prepare rb_parts name query")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .context("query rb_parts names")?;

    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for row in rows {
        let (part_num, name) = row.context("read rb_parts name row")?;
        let Some(resolved) = resolver.resolve_in_catalog(&part_num) else {
            continue;
        };
        let design_id = resolved.design_id;
        names
            .entry(design_id.to_owned())
            .and_modify(|joined| {
                joined.push(' ');
                joined.push_str(&name);
            })
            .or_insert(name);
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refresh_parts::{RbCrossRefPin, RbPartCrossRefs};

    /// A pin mapping each `(part_num, design_id)` pair, with empty cross-refs.
    fn pin_of(pairs: &[(&str, &str)]) -> RbCrossRefPin {
        RbCrossRefPin {
            generated: "2026-01-01".into(),
            parts: pairs
                .iter()
                .map(|(part_num, design)| {
                    (
                        (*part_num).to_owned(),
                        RbPartCrossRefs {
                            ldraw: (*design).to_owned(),
                            external_ids: Default::default(),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn rb_names_space_join_in_part_num_order_and_skip_unmapped() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE ldraw_part (design_id TEXT PRIMARY KEY);
             INSERT INTO ldraw_part VALUES ('92693c01'), ('7777');
             CREATE TABLE ldraw_moved_to (
                 design_id TEXT PRIMARY KEY,
                 target_design_id TEXT NOT NULL
             );
             CREATE TABLE ldraw_alias (
                 design_id TEXT PRIMARY KEY,
                 target_design_id TEXT NOT NULL
             );
             CREATE TABLE rb_parts (part_id_rb TEXT PRIMARY KEY, name TEXT NOT NULL);
             INSERT INTO rb_parts VALUES
                 ('9999', 'Actuator Long'),
                 ('1111', 'Actuator Short'),
                 ('7777', 'Literal Name'),
                 ('2222', 'Ghost Part'),
                 ('no-map', 'Invisible Part');",
        )
        .unwrap();
        // Two part_nums pin onto one in-catalog design; one maps via the
        // literal fallback (in ldraw_part, not in the pin); one pins onto a
        // design absent from ldraw_part (could never join the FTS fill); one
        // has no mapping at all.
        let pin = pin_of(&[
            ("1111", "92693c01"),
            ("9999", "92693c01"),
            ("2222", "ghost"),
        ]);
        let resolver = PartResolver::new(&conn, &pin).unwrap();

        let names = rb_names_by_design(&conn, &resolver).unwrap();
        assert_eq!(
            names.len(),
            2,
            "unmapped and out-of-catalog rb_parts contribute nothing"
        );
        // Joined in part_num order ('1111' < '9999'), space-separated.
        assert_eq!(names["92693c01"], "Actuator Short Actuator Long");
        // The literal-fallback part's Rebrickable name is indexed too (#112).
        assert_eq!(names["7777"], "Literal Name");
        // The pin-mapped-but-absent design is gated out, not keyed on "ghost".
        assert!(!names.contains_key("ghost"));
    }
}
