//! Aggregate `inventory_parts` into the per-part fact + summary tables (#72).
//!
//! `inventory_parts.csv` is ~1M rows; materializing it raw would balloon
//! `catalog.sqlite`. The bundled DB only needs per-part facts — popularity,
//! year span, and which sets contain a part in what color and quantity — so we
//! stream the CSV once and write two derived tables, never the raw rows:
//!
//! - **`rb_part_color_set`** — sparse `(design_id, color_id, set_id)` facts
//!   with `qty`, `qty_spare`, and the set's denormalized `year`.
//! - **`rb_part_summary`** — one row per part: `set_count`/`qty_sum`
//!   (popularity: the `qty` values summed, spares excluded) and
//!   `year_min`/`year_max` (over all appearances).
//!
//! Both tables' `CREATE TABLE` comes from a [`TableSpec`](super::rb_ingest) so
//! the schema is declared the same way as the raw-ingest tables; only the
//! two secondary indexes on `rb_part_color_set` (its key columns need their own
//! index, which the `Role` model can't express on a PK column) are added
//! explicitly.
//!
//! ## Translations
//!
//! `inventory_parts` carries Rebrickable ids; the catalog is LDraw-namespaced:
//!
//! - `part_num` → LDraw `design_id` via the shared [`PartResolver`]
//!   (`super::resolve`): the committed `part_crossrefs.ron` pin first, then
//!   the literal fallback for in-library ids the pin omits (#112). Unmapped
//!   parts (no LDraw geometry) are counted and skipped.
//! - `color_id` → LDraw color code via the compiled-in color reference.
//!   Unmapped colors ("any color" / exotic ids) are counted and skipped.
//!
//! ## Version dedup (upstream)
//!
//! A set can have several inventory versions; counting all of them double-counts
//! its parts. That dedup happens once, at the source: `rb_inventories` holds
//! only the latest version per set (see [`rb_inventories`](super::rb_inventories)),
//! so [`inventory_to_set`] is a plain join and rows whose inventory isn't a
//! catalogued set's latest are simply not in the map.
//!
//! ## Spare-only → main
//!
//! A `(part, color, set)` that appears **only** as a spare (`qty == 0`,
//! `qty_spare > 0`) is treated as a Rebrickable mislabel and promoted to main
//! (`qty = qty_spare`). Verified against official LEGO instructions: such parts
//! are usually *used* in the build but recorded under spares (RB's `is_spare`
//! conflates true spares, unused extras, and alternate-config parts). Genuine
//! spares — a part already in main, with extra copies — keep their `qty_spare`.
//! Targeted exceptions to this blanket rule belong to the manual-corrections
//! layer (#103).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use crate::core::colors::color_reference;
use anyhow::{Context, Result};
use csv::StringRecord;
use rusqlite::{Connection, params};

use super::resolve::PartResolver;

use super::rb_ingest::{self, Field, TableSpec};

/// Observability counts stamped into `meta` after aggregation. The row counters
/// form a partition: `rows_read == rows_skipped_no_set + rows_skipped_unmapped_part
/// + rows_skipped_unmapped_color + rows_aggregated`.
#[derive(Default)]
pub(crate) struct InventoryStats {
    /// Rows written to `rb_part_color_set` (distinct `(design, color, set)`).
    pub color_set_rows: usize,
    /// Rows written to `rb_part_summary` (distinct parts that appear in a set).
    pub summary_count: usize,
    /// Total `inventory_parts` data rows read from the CSV.
    pub rows_read: usize,
    /// Rows dropped because the inventory isn't a catalogued set's latest.
    pub rows_skipped_no_set: usize,
    /// Rows dropped because `part_num` has no LDraw mapping.
    pub rows_skipped_unmapped_part: usize,
    /// Rows dropped because `color_id` has no LDraw color.
    pub rows_skipped_unmapped_color: usize,
    /// Rows folded into a fact (survived all three skips above).
    pub rows_aggregated: usize,
    /// Of the aggregated rows, those whose part mapped via the literal
    /// fallback rather than the pin (an informational subset, not part of
    /// the partition).
    pub rows_mapped_literal: usize,
    /// Of the aggregated rows, those whose translation followed an LDraw
    /// redirect — a `~Moved to` rename or a hard alias (#112; informational,
    /// orthogonal to `rows_mapped_literal` — a chase can start from a pin or
    /// literal hit).
    pub rows_redirected: usize,
    /// Of the aggregated rows, those flagged as spares (`qty_spare`).
    pub rows_spare: usize,
    /// Facts promoted spare-only → main (see the module "Spare-only → main").
    pub facts_promoted: usize,
}

impl InventoryStats {
    /// The `(meta key, value)` rows for this run, so `build` can stamp them all
    /// via `stamp_all` — adding a counter here can't drift from its meta key.
    pub(crate) fn meta_rows(&self) -> [(&'static str, String); 11] {
        [
            ("rb_part_color_set_rows", self.color_set_rows.to_string()),
            ("rb_part_summary_count", self.summary_count.to_string()),
            ("inventory_parts_rows_read", self.rows_read.to_string()),
            (
                "inventory_parts_rows_skipped_no_set",
                self.rows_skipped_no_set.to_string(),
            ),
            (
                "inventory_parts_rows_skipped_unmapped_part",
                self.rows_skipped_unmapped_part.to_string(),
            ),
            (
                "inventory_parts_rows_skipped_unmapped_color",
                self.rows_skipped_unmapped_color.to_string(),
            ),
            (
                "inventory_parts_rows_aggregated",
                self.rows_aggregated.to_string(),
            ),
            (
                "inventory_parts_rows_mapped_literal",
                self.rows_mapped_literal.to_string(),
            ),
            (
                "inventory_parts_rows_redirected",
                self.rows_redirected.to_string(),
            ),
            ("inventory_parts_rows_spare", self.rows_spare.to_string()),
            (
                "inventory_parts_facts_promoted",
                self.facts_promoted.to_string(),
            ),
        ]
    }
}

/// Expected `inventory_parts.csv` header. Pinned so a re-pinned snapshot that
/// reorders or inserts a column fails loudly instead of being mis-read by the
/// fixed column indices below (shared with [`rb_ingest::validate_header`]).
const EXPECTED_HEADER: &[&str] = &[
    "inventory_id",
    "part_num",
    "color_id",
    "quantity",
    "is_spare",
    "img_url",
];

// Column indices into an `inventory_parts` record.
const COL_INVENTORY_ID: usize = 0;
const COL_PART_NUM: usize = 1;
const COL_COLOR_ID: usize = 2;
const COL_QUANTITY: usize = 3;
const COL_IS_SPARE: usize = 4;

/// One typed `inventory_parts` row. Extraction lives in [`InventoryRow::parse`]
/// so the streaming loop reads whole rows, not scattered per-cell parses — and
/// it reuses [`rb_ingest`]'s shared cell/int helpers for consistent errors.
#[derive(Debug)]
struct InventoryRow<'a> {
    inventory_id: i64,
    /// The Rebrickable part number (translated to an LDraw design via the pin).
    part_num: &'a str,
    /// The Rebrickable color id (translated to an LDraw color code). Signed
    /// because Rebrickable uses `-1` for "[No Color/Any]"; such rows have no
    /// LDraw color and are counted as unmapped.
    color_id: i64,
    quantity: i64,
    is_spare: bool,
}

impl<'a> InventoryRow<'a> {
    fn parse(record: &'a StringRecord) -> Result<Self> {
        Ok(Self {
            inventory_id: rb_ingest::req_int(
                rb_ingest::cell(record, COL_INVENTORY_ID, "inventory_id")?,
                "inventory_id",
            )?,
            part_num: rb_ingest::cell(record, COL_PART_NUM, "part_num")?,
            color_id: rb_ingest::req_int(
                rb_ingest::cell(record, COL_COLOR_ID, "color_id")?,
                "color_id",
            )?,
            quantity: rb_ingest::req_int(
                rb_ingest::cell(record, COL_QUANTITY, "quantity")?,
                "quantity",
            )?,
            is_spare: parse_is_spare(rb_ingest::cell(record, COL_IS_SPARE, "is_spare")?)?,
        })
    }
}

pub(crate) fn build(
    conn: &Connection,
    metadata_cache: &Path,
    resolver: &PartResolver<'_>,
) -> Result<InventoryStats> {
    create_tables(conn)?;

    let colors = color_reference();

    // Each catalogued set's (latest) inventory → its set_id, and set_id → year.
    let inv_to_set = inventory_to_set(conn)?;
    let set_to_year = set_to_year(conn)?;

    // (design_id, color_id, set_id) → (qty, qty_spare). BTreeMap so the writes
    // below land in primary-key order — in-order B-tree inserts, and a
    // deterministic file for the reproducible-build check (#73).
    let mut quantities: BTreeMap<(String, u32, u32), (i64, i64)> = BTreeMap::new();
    // design_id → (year_min, year_max) over all appearances with a known year.
    let mut years: BTreeMap<String, (i64, i64)> = BTreeMap::new();

    let mut stats = InventoryStats::default();

    let mut rdr = rb_ingest::open(metadata_cache, "inventory_parts")?;
    rb_ingest::validate_header(&mut rdr, "inventory_parts", EXPECTED_HEADER)?;
    let mut record = StringRecord::new();
    while rdr
        .read_record(&mut record)
        .context("read inventory_parts.csv row")?
    {
        stats.rows_read += 1;
        let row = InventoryRow::parse(&record)?;

        // Drop rows whose inventory isn't a catalogued set's latest before any
        // translation work (version dedup already applied in rb_inventories).
        let Some(&set_id) = inv_to_set.get(&row.inventory_id) else {
            stats.rows_skipped_no_set += 1;
            continue;
        };
        let Some(resolved) = resolver.translate(row.part_num) else {
            stats.rows_skipped_unmapped_part += 1;
            continue;
        };
        if resolved.via_literal {
            stats.rows_mapped_literal += 1;
        }
        if resolved.via_redirect {
            stats.rows_redirected += 1;
        }
        let design_id = resolved.design_id;
        // A negative rb color ("[No Color/Any]") or one with no LDraw code is
        // unmapped and skipped.
        let Some(color_id) = u32::try_from(row.color_id)
            .ok()
            .and_then(|c| colors.ldraw_code_for_rb(c))
        else {
            stats.rows_skipped_unmapped_color += 1;
            continue;
        };
        stats.rows_aggregated += 1;

        let entry = quantities
            .entry((design_id.to_owned(), color_id, set_id))
            .or_insert((0, 0));
        if row.is_spare {
            entry.1 += row.quantity;
            stats.rows_spare += 1;
        } else {
            entry.0 += row.quantity;
        }

        // Year derives from the set regardless of spare status.
        if let Some(Some(year)) = set_to_year.get(&set_id) {
            years
                .entry(design_id.to_owned())
                .and_modify(|(lo, hi)| {
                    *lo = (*lo).min(*year);
                    *hi = (*hi).max(*year);
                })
                .or_insert((*year, *year));
        }
    }

    // Assume a spare-only fact is a mislabeled used part; promote it to main.
    stats.facts_promoted = promote_spare_only(&mut quantities);

    stats.color_set_rows = write_color_set(conn, &quantities, &set_to_year)?;
    stats.summary_count = write_summary(conn, &quantities, &years)?;
    Ok(stats)
}

/// Create the two derived tables from their [`TableSpec`]s (same schema-from-a-
/// declaration path as the raw-ingest tables), then add the two secondary
/// indexes `rb_part_color_set` needs. Those indexes are on key columns, so they
/// can't be expressed via `Field::indexed()` (a column is either a PK or
/// indexed, not both) and are declared here.
fn create_tables(conn: &Connection) -> Result<()> {
    let color_set = TableSpec {
        table_stub: "part_color_set",
        fields: &[
            Field::text("design_id").pk(),
            Field::int("color_id").pk(),
            Field::int("set_id").pk(),
            Field::int("qty"),
            Field::int("qty_spare"),
            Field::opt_int("year"),
        ],
    }
    .create_sql();
    let summary = TableSpec {
        table_stub: "part_summary",
        fields: &[
            Field::text("design_id").pk(),
            Field::int("set_count"),
            Field::int("qty_sum"),
            Field::opt_int("year_min"),
            Field::opt_int("year_max"),
        ],
    }
    .create_sql();
    conn.execute_batch(&format!(
        "{color_set}
         {summary}
         CREATE INDEX idx_rb_part_color_set_color ON rb_part_color_set(color_id);
         CREATE INDEX idx_rb_part_color_set_set ON rb_part_color_set(set_id);"
    ))
    .context("create rb_part_color_set / rb_part_summary")
}

/// Promote every spare-only fact (`qty == 0`, `qty_spare > 0`) to main, in
/// place. Returns how many were promoted. See the module "Spare-only → main".
fn promote_spare_only(quantities: &mut BTreeMap<(String, u32, u32), (i64, i64)>) -> usize {
    let mut promoted = 0;
    for (qty, qty_spare) in quantities.values_mut() {
        if *qty == 0 && *qty_spare > 0 {
            *qty = *qty_spare;
            *qty_spare = 0;
            promoted += 1;
        }
    }
    promoted
}

/// `inventory_id → set_id` for each catalogued set's inventory. Since
/// `rb_inventories` is already deduped to one (latest) version per set, this is
/// a plain join; inventories with no matching `rb_sets` row (e.g. `fig-*`
/// minifig inventories) fall out and their `inventory_parts` rows are skipped.
fn inventory_to_set(conn: &Connection) -> Result<HashMap<i64, u32>> {
    let mut stmt = conn
        .prepare(
            "SELECT i.inventory_id_rb, s.set_id
             FROM rb_inventories i
             JOIN rb_sets s ON s.set_num_rb = i.set_num_rb",
        )
        .context("prepare inventory→set query")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .context("query inventory→set")?;
    let mut map = HashMap::new();
    for row in rows {
        let (inv, set_id) = row.context("read inventory→set row")?;
        map.insert(inv, u32::try_from(set_id).expect("set_id fits in u32"));
    }
    Ok(map)
}

/// `set_id → year` (year nullable). Loaded once so the streaming pass needs no
/// per-row query.
fn set_to_year(conn: &Connection) -> Result<HashMap<u32, Option<i64>>> {
    let mut stmt = conn
        .prepare("SELECT set_id, year FROM rb_sets")
        .context("prepare set→year query")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
        })
        .context("query set years")?;
    let mut map = HashMap::new();
    for row in rows {
        let (set_id, year) = row.context("read set→year row")?;
        map.insert(u32::try_from(set_id).expect("set_id fits in u32"), year);
    }
    Ok(map)
}

/// Write the sparse `(design, color, set)` fact rows, denormalizing each set's
/// `year`. `quantities` is already in primary-key order (a `BTreeMap`), so the
/// inserts land in key order.
fn write_color_set(
    conn: &Connection,
    quantities: &BTreeMap<(String, u32, u32), (i64, i64)>,
    set_to_year: &HashMap<u32, Option<i64>>,
) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO rb_part_color_set
                 (design_id, color_id, set_id, qty, qty_spare, year)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for ((design_id, color_id, set_id), (qty, qty_spare)) in quantities {
            let year = set_to_year.get(set_id).copied().flatten();
            stmt.execute(params![design_id, color_id, set_id, qty, qty_spare, year])
                .with_context(|| {
                    format!("insert rb_part_color_set {design_id}/{color_id}/{set_id}")
                })?;
        }
    }
    tx.commit().context("commit rb_part_color_set")?;
    Ok(quantities.len())
}

/// Roll the facts up per design: `set_count` = distinct sets the part is used in
/// (`qty > 0`), `qty_sum` = the `qty` values summed across those facts (spares
/// excluded — it is the same measure as the fact-level `qty`, not `qty +
/// qty_spare`), and the year span over all appearances. After
/// [`promote_spare_only`] every fact has `qty > 0`, so the guard is
/// belt-and-suspenders — it keeps genuine spare-only quantities out of
/// popularity if promotion is ever disabled.
fn write_summary(
    conn: &Connection,
    quantities: &BTreeMap<(String, u32, u32), (i64, i64)>,
    years: &BTreeMap<String, (i64, i64)>,
) -> Result<usize> {
    let mut rollup: BTreeMap<&str, (BTreeSet<u32>, i64)> = BTreeMap::new();
    for ((design_id, _color, set_id), (qty, _spare)) in quantities {
        let entry = rollup.entry(design_id.as_str()).or_default();
        if *qty > 0 {
            entry.0.insert(*set_id);
            entry.1 += *qty;
        }
    }

    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO rb_part_summary
                 (design_id, set_count, qty_sum, year_min, year_max)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (design_id, (sets, qty_sum)) in &rollup {
            let (year_min, year_max) = match years.get(*design_id) {
                Some((lo, hi)) => (Some(*lo), Some(*hi)),
                None => (None, None),
            };
            let set_count = i64::try_from(sets.len()).expect("set_count fits in i64");
            stmt.execute(params![design_id, set_count, qty_sum, year_min, year_max])
                .with_context(|| format!("insert rb_part_summary {design_id}"))?;
        }
    }
    tx.commit().context("commit rb_part_summary")?;
    Ok(rollup.len())
}

// ─── CSV cell helpers specific to inventory_parts ───────────────────────────

/// Rebrickable writes `is_spare` as `True`/`False`. Anything else is a hard
/// error rather than a silent "not spare" — a changed encoding should fail
/// loudly.
fn parse_is_spare(raw: &str) -> Result<bool> {
    match raw {
        "True" => Ok(true),
        "False" => Ok(false),
        other => anyhow::bail!("unexpected is_spare value {other:?} (expected True/False)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(fields: &[&str]) -> StringRecord {
        StringRecord::from(fields.to_vec())
    }

    #[test]
    fn inventory_row_parses_all_fields() {
        let r = rec(&["102", "3001", "4", "5", "False", ""]);
        let row = InventoryRow::parse(&r).expect("parse");
        assert_eq!(row.inventory_id, 102);
        assert_eq!(row.part_num, "3001");
        assert_eq!(row.color_id, 4);
        assert_eq!(row.quantity, 5);
        assert!(!row.is_spare);

        let spare = rec(&["103", "3023", "4", "1", "True", ""]);
        assert!(InventoryRow::parse(&spare).unwrap().is_spare);
    }

    #[test]
    fn inventory_row_rejects_bad_is_spare_and_names_bad_column() {
        // Lowercase / unexpected is_spare fails loudly rather than defaulting.
        let bad_spare = rec(&["1", "3001", "4", "2", "true", ""]);
        assert!(InventoryRow::parse(&bad_spare).is_err());
        // A non-integer column is named in the error.
        let bad_int = rec(&["x", "3001", "4", "2", "False", ""]);
        let err = InventoryRow::parse(&bad_int).expect_err("non-int inventory_id");
        assert!(format!("{err:#}").contains("inventory_id"), "{err:#}");
    }

    #[test]
    fn validate_header_rejects_reordered_columns() {
        let mut ok = csv::Reader::from_reader(
            "inventory_id,part_num,color_id,quantity,is_spare,img_url\n".as_bytes(),
        );
        rb_ingest::validate_header(&mut ok, "inventory_parts", EXPECTED_HEADER)
            .expect("matching header passes");
        let mut bad = csv::Reader::from_reader(
            "inventory_id,color_id,part_num,quantity,is_spare,img_url\n".as_bytes(),
        );
        assert!(
            rb_ingest::validate_header(&mut bad, "inventory_parts", EXPECTED_HEADER).is_err(),
            "reordered must fail"
        );
    }

    #[test]
    fn promote_spare_only_moves_spare_to_main() {
        let mut q: BTreeMap<(String, u32, u32), (i64, i64)> = BTreeMap::new();
        q.insert(("3023".into(), 4, 2), (0, 1)); // spare-only → promoted
        q.insert(("3001".into(), 4, 1), (2, 1)); // genuine spare → untouched
        q.insert(("3626".into(), 15, 1), (1, 0)); // plain main → untouched
        assert_eq!(promote_spare_only(&mut q), 1);
        assert_eq!(q[&("3023".into(), 4, 2)], (1, 0));
        assert_eq!(q[&("3001".into(), 4, 1)], (2, 1));
        assert_eq!(q[&("3626".into(), 15, 1)], (1, 0));
    }
}
