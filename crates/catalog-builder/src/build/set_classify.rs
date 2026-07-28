//! Classify each Rebrickable "set" as a genuine build or a non-build (parts
//! pack / baseplate / merchandise), so build-based popularity metrics can keep
//! only builds (brickdata#19).
//!
//! Signals: a set's *true* distinct-mold count and its pieces-per-mold
//! concentration (from the raw pre-mapping inventory) plus curated,
//! high-precision set-name keywords. **No LDraw part categories** (a follow-up
//! will investigate whether they help).
//!
//! ## The ceiling gate
//!
//! Genuine builds with pack-like names ("… Battle Pack") have *high* distinct
//! mold counts — 47–93 in the calibration data — while real packs live in the
//! low-distinct region. So every non-build rule is gated behind
//! [`DISTINCT_CEILING`]: above it a set is always a `build`, whatever its name.
//!
//! Thresholds are documented constants (no config file); the values used are
//! stamped into `meta` per build for reproducibility.

use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::inventory::SetInventoryCounts;

/// Max distinct molds for a set to be eligible for any non-build label; above
/// this it is always a `build`. Calibrated on `catalog-v3.sqlite`: ~37% of sets
/// have <15 distinct molds and essentially all packs fall in that band, whereas
/// real builds with pack-like names (Battle Packs) sit at 47–93 distinct molds.
///
/// Calibration query (per-set distinct raw part_num over the latest inventory,
/// joined to rb_sets): the sub-15 band held the packs; "%battle pack%" /
/// "%booster%" sets clustered at 47–93 distinct.
pub(crate) const DISTINCT_CEILING: u32 = 14;

/// Pieces-per-mold at or above which a low-distinct set is a bulk `parts_pack`.
/// Calibrated: bulk tubs/buckets/mosaics run 100+ pieces/mold, while real small
/// builds in the transition zone top out ~11 pieces/mold ("Safari Hippo"), so
/// 15 separates them without catching genuine small builds.
pub(crate) const CONCENTRATION: i64 = 15;

/// A set's classification, stored as text in `rb_sets.set_type`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SetType {
    /// A buildable model — the kept type for build-based metrics.
    Build,
    /// A bulk / monotype / assortment pack, or a single-element "set".
    PartsPack,
    /// A named baseplate / building-plate.
    Baseplate,
    /// A named non-build product (watch, magnet, …).
    Merchandise,
    /// No catalogued inventory and no name-keyword hit — unclassifiable.
    Unknown,
}

impl SetType {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SetType::Build => "build",
            SetType::PartsPack => "parts_pack",
            SetType::Baseplate => "baseplate",
            SetType::Merchandise => "merchandise",
            SetType::Unknown => "unknown",
        }
    }
}

/// Baseplate / building-plate name markers.
const BASEPLATE_KEYWORDS: &[&str] = &["baseplate", "base plate", "building plate", "brickplate"];
/// Non-build merchandise markers. Collisions (e.g. "Watchtower") are guarded by
/// the distinct ceiling — real builds with these words have >14 molds.
const MERCHANDISE_KEYWORDS: &[&str] = &[
    "watch",
    "magnet",
    "keychain",
    "key chain",
    "ornament",
    "clock",
];
/// Bulk / assortment markers. Deliberately specific: bare "pack" is absent (it
/// matches "Battle Pack" / "Booster Pack" / "Backpack", all builds). "tub" is
/// omitted (it collides with "tube"); tubs are caught by [`CONCENTRATION`].
const PACK_PHRASES: &[&str] = &[
    "pack of",
    "parts pack",
    "spare",
    "assorted",
    "assortment",
    "bulk",
    "bucket",
    "supplementary",
];

fn name_matches(name_lc: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|k| name_lc.contains(k))
}

/// Classify a set from its raw inventory shape and name. Pure — the calibration
/// lives in the constants and this ordering. `counts` is `None` for a set with
/// no catalogued inventory; such a set can only match a name keyword, else it is
/// [`SetType::Unknown`].
pub(crate) fn classify(counts: Option<&SetInventoryCounts>, name: &str) -> SetType {
    // Above the ceiling → always a build, no matter the name.
    if let Some(c) = counts
        && c.distinct_part_count > DISTINCT_CEILING
    {
        return SetType::Build;
    }

    let name_lc = name.to_lowercase();
    if name_matches(&name_lc, BASEPLATE_KEYWORDS) {
        return SetType::Baseplate;
    }
    if name_matches(&name_lc, MERCHANDISE_KEYWORDS) {
        return SetType::Merchandise;
    }
    if name_matches(&name_lc, PACK_PHRASES) {
        return SetType::PartsPack;
    }

    match counts {
        Some(c) if c.distinct_part_count == 1 => SetType::PartsPack,
        Some(c) if c.pieces_main >= CONCENTRATION * i64::from(c.distinct_part_count) => {
            SetType::PartsPack
        }
        Some(_) => SetType::Build,
        None => SetType::Unknown,
    }
}

/// Per-type set counts, stamped into `meta` (same pattern as `InventoryStats`).
#[derive(Default)]
pub(crate) struct ClassifyStats {
    pub build: usize,
    pub parts_pack: usize,
    pub baseplate: usize,
    pub merchandise: usize,
    pub unknown: usize,
}

impl ClassifyStats {
    fn tally(&mut self, t: SetType) {
        match t {
            SetType::Build => self.build += 1,
            SetType::PartsPack => self.parts_pack += 1,
            SetType::Baseplate => self.baseplate += 1,
            SetType::Merchandise => self.merchandise += 1,
            SetType::Unknown => self.unknown += 1,
        }
    }

    pub(crate) fn meta_rows(&self) -> [(&'static str, String); 5] {
        [
            ("set_type_build_count", self.build.to_string()),
            ("set_type_parts_pack_count", self.parts_pack.to_string()),
            ("set_type_baseplate_count", self.baseplate.to_string()),
            ("set_type_merchandise_count", self.merchandise.to_string()),
            ("set_type_unknown_count", self.unknown.to_string()),
        ]
    }
}

/// Add `distinct_part_count` + `set_type` to `rb_sets` and fill them for every
/// set. `counts` is the per-`set_id` raw shape from [`inventory::build`](super::inventory);
/// a set absent from it has no catalogued inventory (NULL distinct, name-only
/// class). The scan/UPDATE runs `ORDER BY set_id` and `classify` is pure, so the
/// result is deterministic (reproducible-build guarantee, #73).
pub(crate) fn build(
    conn: &Connection,
    counts: &HashMap<u32, SetInventoryCounts>,
) -> Result<ClassifyStats> {
    conn.execute_batch(
        "ALTER TABLE rb_sets ADD COLUMN distinct_part_count INTEGER;
         ALTER TABLE rb_sets ADD COLUMN set_type TEXT NOT NULL DEFAULT 'unknown';",
    )
    .context("add rb_sets.distinct_part_count / set_type")?;

    let rows: Vec<(i64, String)> = {
        let mut stmt = conn
            .prepare("SELECT set_id, name FROM rb_sets ORDER BY set_id")
            .context("prepare rb_sets scan")?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .context("query rb_sets")?;
        mapped
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read rb_sets rows")?
    };

    let mut stats = ClassifyStats::default();
    let tx = conn.unchecked_transaction()?;
    {
        let mut upd = tx.prepare(
            "UPDATE rb_sets SET distinct_part_count = ?1, set_type = ?2 WHERE set_id = ?3",
        )?;
        for (set_id, name) in &rows {
            let sid = u32::try_from(*set_id).expect("set_id fits u32");
            let shape = counts.get(&sid);
            let set_type = classify(shape, name);
            stats.tally(set_type);
            let distinct: Option<i64> = shape.map(|c| i64::from(c.distinct_part_count));
            upd.execute(rusqlite::params![distinct, set_type.as_str(), set_id])
                .with_context(|| format!("update rb_sets set_type for set_id {set_id}"))?;
        }
    }
    tx.commit().context("commit rb_sets classification")?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::inventory::SetInventoryCounts;

    fn counts(distinct: u32, pieces: i64) -> SetInventoryCounts {
        SetInventoryCounts {
            distinct_part_count: distinct,
            pieces_main: pieces,
        }
    }

    #[test]
    fn high_distinct_pack_named_set_is_still_a_build() {
        // A "Battle Pack" is a real build with many molds — the ceiling protects it.
        let c = counts(60, 180);
        assert_eq!(
            classify(Some(&c), "Clone Troopers Battle Pack"),
            SetType::Build
        );
    }

    #[test]
    fn single_mold_set_is_a_parts_pack() {
        let c = counts(1, 100);
        assert_eq!(classify(Some(&c), "2 x 4 Black Bricks"), SetType::PartsPack);
    }

    #[test]
    fn baseplate_and_merchandise_names_win_below_ceiling() {
        assert_eq!(
            classify(Some(&counts(1, 1)), "Green Baseplate"),
            SetType::Baseplate
        );
        assert_eq!(
            classify(Some(&counts(3, 6)), "LEGO Star Wars Watch"),
            SetType::Merchandise
        );
    }

    #[test]
    fn high_concentration_low_distinct_is_a_parts_pack() {
        // 2 molds, 40 pieces → 20 pieces/mold ≥ 15.
        assert_eq!(
            classify(Some(&counts(2, 40)), "Bricks and Half Bricks"),
            SetType::PartsPack
        );
    }

    #[test]
    fn small_diverse_set_stays_a_build() {
        // 8 molds, 45 pieces → ~5.6 pieces/mold, no keyword → build.
        assert_eq!(
            classify(Some(&counts(8, 45)), "Safari Basic Set"),
            SetType::Build
        );
    }

    #[test]
    fn no_inventory_is_unknown_unless_a_keyword_hits() {
        assert_eq!(classify(None, "Some Promotional Thing"), SetType::Unknown);
        assert_eq!(classify(None, "32 x 32 Baseplate"), SetType::Baseplate);
    }

    #[test]
    fn build_adds_columns_classifies_every_set_and_tallies() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE rb_sets (
                 set_num_rb TEXT PRIMARY KEY, name TEXT NOT NULL, set_id INTEGER NOT NULL
             ) WITHOUT ROWID;
             INSERT INTO rb_sets VALUES
                 ('10-1', 'Fire Station',        1),
                 ('20-1', '2 x 4 Black Bricks',  2),
                 ('30-1', 'Green Baseplate',     3),
                 ('40-1', 'Promo Keyring Thing', 4);",
        )
        .unwrap();

        let mut counts: HashMap<u32, SetInventoryCounts> = HashMap::new();
        counts.insert(
            1,
            SetInventoryCounts {
                distinct_part_count: 40,
                pieces_main: 300,
            },
        );
        counts.insert(
            2,
            SetInventoryCounts {
                distinct_part_count: 1,
                pieces_main: 100,
            },
        );
        counts.insert(
            3,
            SetInventoryCounts {
                distinct_part_count: 1,
                pieces_main: 1,
            },
        );
        // set 4 absent → no catalogued inventory.

        let stats = build(&conn, &counts).unwrap();

        let set_type = |sid: i64| -> String {
            conn.query_row(
                "SELECT set_type FROM rb_sets WHERE set_id = ?1",
                [sid],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(set_type(1), "build");
        assert_eq!(set_type(2), "parts_pack");
        assert_eq!(set_type(3), "baseplate");
        assert_eq!(set_type(4), "unknown");

        // distinct_part_count is filled for sets with inventory, NULL otherwise.
        let distinct: Option<i64> = conn
            .query_row(
                "SELECT distinct_part_count FROM rb_sets WHERE set_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(distinct, Some(40));
        let none: Option<i64> = conn
            .query_row(
                "SELECT distinct_part_count FROM rb_sets WHERE set_id = 4",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(none, None);

        assert_eq!(stats.build, 1);
        assert_eq!(stats.parts_pack, 1);
        assert_eq!(stats.baseplate, 1);
        assert_eq!(stats.unknown, 1);
    }
}
