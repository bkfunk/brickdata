//! `rb_part_relationships` table: Rebrickable part↔part relationships,
//! filtered to our LDraw catalog (#82).
//!
//! `part_relationships.csv` (~36k rows) relates two Rebrickable part numbers
//! with one of six type codes:
//!
//! | code | meaning                                    |
//! |------|--------------------------------------------|
//! | P    | Print (decorated → plain base)             |
//! | T    | Pattern (marbled/embossed; like Print)     |
//! | M    | Mold (functional drop-in replacement)      |
//! | A    | Alternate (loose substitute)               |
//! | R    | Pair (left/right, inner/outer, base+insert)|
//! | B    | Sub-Part (component → `cNN` assembly)      |
//!
//! R and B power editor features ("place the mirrored counterpart", "expand a
//! coupling into its molds") and exist **only** in this CSV — the per-part API
//! response covers P/T/M/A but not R/B. All six types are kept for
//! single-source simplicity.
//!
//! ## Catalog filter (one endpoint)
//!
//! ~57k of ~62k Rebrickable parts have no LDraw geometry, so most rows are
//! noise to us. Each endpoint is resolved `part_num → LDraw design_id` via
//! the shared [`PartResolver`](super::resolve) (pin first, then the literal
//! fallback of #112), then checked against `ldraw_part`; a row is kept
//! when **at least one** endpoint resolves into the catalog. One-endpoint
//! (rather than both) preserves rows whose counterpart isn't in the library,
//! so the editor can still say "a left/right pair exists, but it's not here".
//!
//! ## Columns
//!
//! The raw Rebrickable part numbers are kept verbatim (they are the row's
//! identity, and the un-resolved endpoint may have no other name), alongside
//! `child_design_id` / `parent_design_id` — the resolved LDraw ids, NULL when
//! that endpoint isn't in the catalog. The design-id columns are what runtime
//! queries key on; the DB carries no other Rebrickable→LDraw mapping.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use csv::StringRecord;
use rusqlite::{Connection, params};

use super::resolve::PartResolver;

use super::rb_ingest::{self, Field, TableSpec};

/// Observability counts stamped into `meta`. The row counters partition the
/// CSV: `rows_read == rows_skipped_out_of_catalog + rows_deduped + rows_written`.
#[derive(Default)]
pub(crate) struct RelationshipStats {
    /// Total `part_relationships` data rows read from the CSV.
    pub rows_read: usize,
    /// Rows dropped because neither endpoint resolves into `ldraw_part`.
    pub rows_skipped_out_of_catalog: usize,
    /// Exact-duplicate rows collapsed by the primary key.
    pub rows_deduped: usize,
    /// Rows written to `rb_part_relationships`.
    pub rows_written: usize,
}

impl RelationshipStats {
    /// The `(meta key, value)` rows for this run, so `build` can stamp them
    /// all via `stamp_all` — adding a counter here can't drift from its key.
    pub(crate) fn meta_rows(&self) -> [(&'static str, String); 4] {
        [
            ("rb_part_relationships_count", self.rows_written.to_string()),
            ("part_relationships_rows_read", self.rows_read.to_string()),
            (
                "part_relationships_rows_skipped_out_of_catalog",
                self.rows_skipped_out_of_catalog.to_string(),
            ),
            (
                "part_relationships_rows_deduped",
                self.rows_deduped.to_string(),
            ),
        ]
    }
}

/// Expected `part_relationships.csv` header, pinned like every other ingest so
/// a re-pinned snapshot that reorders or inserts a column fails loudly.
const EXPECTED_HEADER: &[&str] = &["rel_type", "child_part_num", "parent_part_num"];

const COL_REL_TYPE: usize = 0;
const COL_CHILD: usize = 1;
const COL_PARENT: usize = 2;

/// The six documented relationship type codes. An unknown code is a hard
/// error, not a pass-through — a new upstream type should be noticed and
/// classified, not silently carried.
const REL_TYPES: &[&str] = &["P", "T", "M", "A", "R", "B"];

pub(crate) fn build(
    conn: &Connection,
    metadata_cache: &Path,
    resolver: &PartResolver<'_>,
) -> Result<RelationshipStats> {
    create_table(conn)?;

    // part_num → its design id, only if that design is in the catalog.
    let resolve = |part_num: &str| resolver.resolve_in_catalog(part_num).map(|r| r.design_id);

    let mut stats = RelationshipStats::default();
    // Kept rows, keyed by the full raw tuple: BTreeMap so the inserts below
    // land in primary-key order (deterministic file layout for the
    // reproducible-build check, #73) and exact duplicates collapse.
    let mut kept: BTreeMap<(String, String, String), ()> = BTreeMap::new();

    let mut rdr = rb_ingest::open(metadata_cache, "part_relationships")?;
    rb_ingest::validate_header(&mut rdr, "part_relationships", EXPECTED_HEADER)?;
    let mut record = StringRecord::new();
    while rdr
        .read_record(&mut record)
        .context("read part_relationships.csv row")?
    {
        stats.rows_read += 1;
        let rel_type = parse_rel_type(rb_ingest::cell(&record, COL_REL_TYPE, "rel_type")?)?;
        let child = rb_ingest::cell(&record, COL_CHILD, "child_part_num")?;
        let parent = rb_ingest::cell(&record, COL_PARENT, "parent_part_num")?;

        if resolve(child).is_none() && resolve(parent).is_none() {
            stats.rows_skipped_out_of_catalog += 1;
            continue;
        }
        if kept
            .insert(
                (rel_type.to_owned(), child.to_owned(), parent.to_owned()),
                (),
            )
            .is_some()
        {
            stats.rows_deduped += 1;
        }
    }

    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO rb_part_relationships
                 (rel_type, child_part_num, parent_part_num,
                  child_design_id, parent_design_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (rel_type, child, parent) in kept.keys() {
            stmt.execute(params![
                rel_type,
                child,
                parent,
                resolve(child),
                resolve(parent)
            ])
            .with_context(|| format!("insert rb_part_relationships {rel_type}/{child}/{parent}"))?;
        }
    }
    tx.commit().context("commit rb_part_relationships")?;

    stats.rows_written = kept.len();
    Ok(stats)
}

/// Create the table from its [`TableSpec`] (same schema-from-a-declaration
/// path as the other tables), then add the two raw-part-num indexes — those
/// columns are part of the composite key, which `Field::indexed()` can't
/// express (a column is either a PK or indexed, not both).
fn create_table(conn: &Connection) -> Result<()> {
    let spec = TableSpec {
        table_stub: "part_relationships",
        fields: &[
            Field::text("rel_type").pk(),
            Field::text("child_part_num").pk(),
            Field::text("parent_part_num").pk(),
            Field::opt_text("child_design_id").indexed(),
            Field::opt_text("parent_design_id").indexed(),
        ],
    }
    .create_sql();
    conn.execute_batch(&format!(
        "{spec}
         CREATE INDEX idx_rb_part_relationships_child
             ON rb_part_relationships(child_part_num);
         CREATE INDEX idx_rb_part_relationships_parent
             ON rb_part_relationships(parent_part_num);"
    ))
    .context("create rb_part_relationships")
}

/// Validate a `rel_type` code against the six documented values, borrowing it
/// through. Anything else fails the build loudly (see [`REL_TYPES`]).
fn parse_rel_type(raw: &str) -> Result<&str> {
    if REL_TYPES.contains(&raw) {
        Ok(raw)
    } else {
        anyhow::bail!(
            "unexpected rel_type {raw:?} (expected one of {REL_TYPES:?}); \
             a new Rebrickable relationship type needs classifying"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rel_type_accepts_the_six_codes_and_rejects_others() {
        for code in REL_TYPES {
            assert_eq!(parse_rel_type(code).unwrap(), *code);
        }
        for bad in ["X", "p", "", "PR"] {
            let err = parse_rel_type(bad).expect_err("unknown code must error");
            assert!(
                format!("{err:#}").contains("rel_type"),
                "names the column: {err:#}"
            );
        }
    }

    #[test]
    fn validate_header_rejects_reordered_columns() {
        let mut ok =
            csv::Reader::from_reader("rel_type,child_part_num,parent_part_num\n".as_bytes());
        rb_ingest::validate_header(&mut ok, "part_relationships", EXPECTED_HEADER)
            .expect("matching header passes");
        let mut bad =
            csv::Reader::from_reader("rel_type,parent_part_num,child_part_num\n".as_bytes());
        assert!(
            rb_ingest::validate_header(&mut bad, "part_relationships", EXPECTED_HEADER).is_err(),
            "reordered must fail"
        );
    }
}
