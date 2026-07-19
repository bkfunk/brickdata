//! The `ldraw_part` table: one row per user-pickable LDraw design,
//! scanned from the LDraw library by `PartCatalog::build` (which collapses
//! `-fN` flexion variants onto the canonical design and skips
//! subparts/primitives).
//!
//! ## Columns
//!
//! - `design_id` (PK) — canonical LDraw design id (the `.dat` stem).
//! - `name`, `subcategory_id`, `category_id`, `dimensions`, `is_decorated` —
//!   the original #70 column set.
//! - `flexion_variants` (BLOB) — the `-fN` positions that collapsed onto this
//!   design, so the UI can swap between them. Added in PR #79 review.
//! - `has_base_file` — whether a plain `<design_id>.dat` exists (vs. a
//!   flexion-only design). Distinguishes the two shapes and, with
//!   `flexion_variants`, lets `PartEntry::source_dat_ids` enumerate the real
//!   `.dat` files a geometry baker must read. Added in PR #79 review.
//!
//! The last two columns extend #70's originally-specified schema; they were
//! added during review to keep the canonical→variant grouping (and the
//! files backing it) rather than discard it on collapse.
//!
//! The classified `Subcategory`/`Category` are stored as their enum
//! discriminants (`as u16`); category display names stay client-side in
//! `blockstar_core` rather than in a lookup table. The discriminant
//! encoding is pinned by the tests below (`Category` fully, `Subcategory`
//! by sentinel).
//!
//! Two companion tables record the redirect hops the scan sees (#112):
//!
//! - `ldraw_moved_to` — `~Moved to` tombstones: retired design id →
//!   renamed-to id.
//! - `ldraw_alias` — hard aliases (`!LDRAW_ORG … Alias`): a *current, valid*
//!   design id → the id whose geometry it duplicates.
//!
//! One hop per row, with both ids canonicalized by the scan (`-fN` flexion
//! suffixes collapsed) like every id the catalog keys on; a target that was
//! itself renamed appears as its own row, so chains stay representable.
//! Neither kind has an `ldraw_part` row, so these tables are the only record
//! of the hops — the build's part resolver chases both, and a runtime search
//! for a retired or alias id can too.

use crate::core::PartCatalog;
use crate::core::blob::pack_u32_le;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// Row counts from the library scan, for the `meta` stamps.
pub(crate) struct ScanCounts {
    pub parts: usize,
    pub moved_to: usize,
    pub aliases: usize,
}

/// Create and populate `ldraw_part` (and the companion `ldraw_moved_to` /
/// `ldraw_alias` hop tables) from the LDraw library at `ldraw_dir`.
pub(crate) fn build(conn: &Connection, ldraw_dir: &Path) -> Result<ScanCounts> {
    create_table(conn)?;
    let catalog = PartCatalog::build(ldraw_dir).context("scan LDraw library")?;
    populate(conn, &catalog)?;
    populate_hops(conn, "ldraw_moved_to", catalog.moved_to())?;
    populate_hops(conn, "ldraw_alias", catalog.aliases())?;
    Ok(ScanCounts {
        parts: catalog.len(),
        moved_to: catalog.moved_to().len(),
        aliases: catalog.aliases().len(),
    })
}

/// Create and fill one `design_id → target_design_id` hop table. `BTreeMap`
/// iteration keeps the inserts in key order (deterministic file layout, like
/// every other table).
fn populate_hops(
    conn: &Connection,
    table: &str,
    hops: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE {table} (
            design_id        TEXT PRIMARY KEY,
            target_design_id TEXT NOT NULL
        ) WITHOUT ROWID;
        CREATE INDEX idx_{table}_target ON {table}(target_design_id);"
    ))
    .with_context(|| format!("create {table} table"))?;
    let tx = conn
        .unchecked_transaction()
        .with_context(|| format!("begin {table} transaction"))?;
    {
        let mut stmt = tx.prepare(&format!(
            "INSERT INTO {table} (design_id, target_design_id) VALUES (?1, ?2)"
        ))?;
        for (design_id, target) in hops {
            stmt.execute(rusqlite::params![design_id, target])
                .with_context(|| format!("insert {table} {design_id}"))?;
        }
    }
    tx.commit().with_context(|| format!("commit {table}"))?;
    Ok(())
}

fn create_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE ldraw_part (
            design_id        TEXT PRIMARY KEY,
            name             TEXT NOT NULL,
            subcategory_id   INTEGER NOT NULL,
            category_id      INTEGER NOT NULL,
            dimensions       BLOB,
            is_decorated     INTEGER NOT NULL,
            flexion_variants BLOB,
            has_base_file    INTEGER NOT NULL
        ) WITHOUT ROWID;
        CREATE INDEX idx_ldraw_part_category ON ldraw_part(category_id);
        CREATE INDEX idx_ldraw_part_subcategory ON ldraw_part(subcategory_id);",
    )
    .context("create ldraw_part table")
}

fn populate(conn: &Connection, catalog: &PartCatalog) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .context("begin ldraw_part transaction")?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO ldraw_part
                (design_id, name, subcategory_id, category_id, dimensions,
                 is_decorated, flexion_variants, has_base_file)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for entry in catalog.entries() {
            // Stud dimensions packed as little-endian u32s; NULL when the
            // name carried no `N x N` run.
            let dimensions = pack_u32_le(&entry.dimensions);
            // Flexion-position numbers (e.g. f1/f2) packed the same way;
            // NULL for an ordinary part with no flexion variants.
            let flexion_variants = pack_u32_le(&entry.flexion_variants);
            stmt.execute(rusqlite::params![
                entry.design_id,
                entry.name,
                entry.subcategory as u16,
                entry.subcategory.category() as u16,
                dimensions,
                entry.is_decorated,
                flexion_variants,
                entry.has_base_file,
            ])
            .with_context(|| format!("insert ldraw_part {}", entry.design_id))?;
        }
    }
    tx.commit().context("commit ldraw_part transaction")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::core::{Category, Subcategory};

    // `build` stores the taxonomy as `enum as u16` discriminants, so the
    // discriminant values are an on-disk contract: reordering the enums in
    // blockstar-core would silently repoint every stored id.

    // `Category` is small, so pin every current variant to its discriminant,
    // in order. This fails on any reorder or mid-list insertion — the changes
    // that repoint already-stored ids. Appending a new *trailing* variant
    // leaves these discriminants unchanged and is intentionally allowed (old
    // stored ids stay valid), so it is deliberately not caught here.
    #[test]
    fn category_discriminants_are_fully_pinned() {
        let expected = [
            (Category::Bricks, 0),
            (Category::Plates, 1),
            (Category::Tiles, 2),
            (Category::Slopes, 3),
            (Category::Technic, 4),
            (Category::Electronics, 5),
            (Category::Minifigs, 6),
            (Category::ThemeElements, 7),
            (Category::Nature, 8),
            (Category::Buildings, 9),
            (Category::Vehicles, 10),
            (Category::Other, 11),
        ];
        for (cat, disc) in expected {
            assert_eq!(cat as u16, disc, "{cat:?} discriminant drifted");
        }
    }

    // `Subcategory` has ~90 variants, so a full pin would be its own
    // maintenance burden. These sentinels catch the common drift (a variant
    // added/removed before them shifts their values) but do NOT prove the
    // entire mapping is stable — a reorder among unchecked variants would
    // pass. Treat them as a tripwire, not a complete contract.
    #[test]
    fn subcategory_sentinel_discriminants() {
        assert_eq!(Subcategory::Bricks as u16, 0);
        assert_eq!(Subcategory::Plates as u16, 4);
        assert_eq!(Subcategory::TechnicBricks as u16, 19);
    }
}
