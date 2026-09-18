# Set-type classification & build-based frequency filter — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Classify every Rebrickable set on `rb_sets.set_type`, and make the `part_frequency.ron` sidecar count only *builds* (not packs/baseplates/merchandise).

**Architecture:** During the existing `inventory_parts` streaming pass, accumulate each set's true (pre-mapping) distinct-mold count and non-spare piece total. A new pure `set_classify` module turns those numbers plus the set name into a `SetType`, gating every non-build rule behind a distinct-mold ceiling (so pack-named real builds like "Battle Pack" stay builds). The classification is stored on `rb_sets`; the frequency sidecar joins it and keeps only `set_type = 'build'`, renaming its fields `builds`/`build_qty`.

**Tech Stack:** Rust (edition 2024), `rusqlite` (bundled SQLite), `csv`, `anyhow`, RON output. Build tool crate: `crates/catalog-builder`.

**Spec:** `docs/superpowers/specs/2026-07-23-set-type-classification-design.md`

## Global Constraints

Every task's requirements implicitly include these:

- **MSRV 1.88**, edition 2024 (let-chains are allowed — already used in `build.rs`).
- **`-D warnings`** everywhere (`cargo test`, `cargo check`, `cargo clippy --all-targets`). No unused items in any committed state.
- **`cargo fmt --all --check` must pass.** Write formatted code; format only files you changed (`rustfmt <path>`), never a blanket sweep.
- **No config file** — thresholds are named `pub(crate) const`s with calibration doc-comments; the values used are stamped into `meta`.
- **No LDraw part categories** in classification (name + numeric signals only).
- **Determinism preserved** (#73): the build must stay byte-identical on the same pins. `classify` is pure; the classification UPDATE runs `ORDER BY set_id`.
- Verify each task with: `cargo test -p brickdata-catalog-builder`, then `cargo clippy --workspace --all-targets --all-features -- -D warnings`, then `cargo fmt --all --check`.

## File Structure

- **Modify** `crates/catalog-builder/src/build/inventory.rs` — add `SetInventoryCounts` and a `record_set_shape` helper; accumulate per-set raw shape in `build`; return `(InventoryStats, HashMap<u32, SetInventoryCounts>)`.
- **Create** `crates/catalog-builder/src/build/set_classify.rs` — `SetType` enum, threshold constants, keyword lists, the pure `classify` function, `ClassifyStats`, and the `build` step that adds the two `rb_sets` columns and fills them.
- **Modify** `crates/catalog-builder/src/build.rs` — declare `mod set_classify;`, bump `SCHEMA_VERSION` to 4, destructure the inventory return, call `set_classify::build`, stamp classifier meta.
- **Modify** `crates/catalog-builder/tests/build.rs` — `schema_version` → `"4"`; assert set-type meta rows exist.
- **Modify** `crates/catalog-builder/src/build/part_frequency.rs` — build-only aggregation (join `rb_sets`, filter `set_type = 'build'`), rename fields `sets`→`builds` / `qty`→`build_qty`, rewrite the header, update tests.

---

### Task 1: Classify sets and store on `rb_sets`

Adds the raw per-set counts, the classifier, and the DB wiring in one coherent, fully-wired change (required by `-D warnings` — the classifier's outputs are read by the driver in the same commit).

**Files:**
- Modify: `crates/catalog-builder/src/build/inventory.rs`
- Create: `crates/catalog-builder/src/build/set_classify.rs`
- Modify: `crates/catalog-builder/src/build.rs`
- Modify: `crates/catalog-builder/tests/build.rs`

**Interfaces:**
- Produces (`inventory`): `pub(crate) struct SetInventoryCounts { pub distinct_part_count: u32, pub pieces_main: i64 }`; `inventory::build(...) -> Result<(InventoryStats, HashMap<u32, SetInventoryCounts>)>`.
- Produces (`set_classify`): `pub(crate) enum SetType { Build, PartsPack, Baseplate, Merchandise, Unknown }` with `fn as_str(self) -> &'static str`; `pub(crate) const DISTINCT_CEILING: u32 = 14`; `pub(crate) const CONCENTRATION: i64 = 15`; `pub(crate) fn classify(counts: Option<&SetInventoryCounts>, name: &str) -> SetType`; `pub(crate) fn build(conn, &HashMap<u32, SetInventoryCounts>) -> Result<ClassifyStats>` where `ClassifyStats::meta_rows(&self) -> [(&'static str, String); 5]`.

- [ ] **Step 1: Write the failing test for the inventory shape helper**

Add to the `tests` module at the bottom of `crates/catalog-builder/src/build/inventory.rs`:

```rust
    #[test]
    fn record_set_shape_counts_distinct_molds_and_excludes_spares() {
        let mut shape: HashMap<u32, (HashSet<String>, i64)> = HashMap::new();
        // Two distinct molds in set 7; a repeated mold doesn't double-count.
        record_set_shape(&mut shape, 7, "3001", 4, false);
        record_set_shape(&mut shape, 7, "3002", 2, false);
        record_set_shape(&mut shape, 7, "3001", 9, false);
        // A spare adds a distinct mold but no main pieces.
        record_set_shape(&mut shape, 7, "3003", 5, true);
        let (molds, pieces_main) = &shape[&7];
        assert_eq!(molds.len(), 3, "3001/3002/3003 are three distinct molds");
        assert_eq!(*pieces_main, 4 + 2 + 9, "spare quantity excluded from pieces_main");
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p brickdata-catalog-builder record_set_shape -- --nocapture`
Expected: FAIL to compile — `cannot find function record_set_shape` / `HashSet` unresolved.

- [ ] **Step 3: Add the struct, the helper, and the `HashSet` import**

In `crates/catalog-builder/src/build/inventory.rs`, change the collections import (line ~50) to include `HashSet`:

```rust
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
```

Add, near `InventoryStats` (after its `impl` block, before `EXPECTED_HEADER`):

```rust
/// A set's true inventory shape, from the raw `inventory_parts` rows **before**
/// any part_num → LDraw mapping drops anything — the honest signal
/// [`set_classify`](super::set_classify) needs. Computing it pre-mapping keeps a
/// small licensed set full of un-mapped printed/minifig parts from looking like
/// a pack (its distinct molds would otherwise be undercounted).
pub(crate) struct SetInventoryCounts {
    /// Distinct raw `part_num` molds in the set (spare or not).
    pub distinct_part_count: u32,
    /// Σ non-spare `quantity` across the set's raw inventory.
    pub pieces_main: i64,
}

/// Fold one raw inventory row into the per-set shape accumulator: record its
/// mold, and add its quantity to the main total unless it is a spare.
fn record_set_shape(
    shape: &mut HashMap<u32, (HashSet<String>, i64)>,
    set_id: u32,
    part_num: &str,
    quantity: i64,
    is_spare: bool,
) {
    let entry = shape.entry(set_id).or_default();
    entry.0.insert(part_num.to_owned());
    if !is_spare {
        entry.1 += quantity;
    }
}
```

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test -p brickdata-catalog-builder record_set_shape`
Expected: PASS.

- [ ] **Step 5: Accumulate the shape in `build` and return it**

In `inventory::build`, declare the accumulator next to the existing `quantities`/`years` maps (after line ~211):

```rust
    // Per-set raw shape for classification (#19): distinct molds (pre-mapping)
    // and non-spare piece total, keyed by the dense set_id.
    let mut set_shape: HashMap<u32, (HashSet<String>, i64)> = HashMap::new();
```

Inside the streaming loop, immediately after the `inv_to_set` lookup resolves `set_id` (the `let Some(&set_id) = inv_to_set.get(&row.inventory_id) else { … };` block) and **before** the `resolver.translate` drop, add:

```rust
        // Record the raw (pre-mapping) shape before any unmapped-part/color drop
        // below, so distinct_part_count counts every mold Rebrickable lists.
        record_set_shape(&mut set_shape, set_id, row.part_num, row.quantity, row.is_spare);
```

After the write calls near the end of `build` (after `stats.summary_count = write_summary(...)?;`), convert and return a tuple. Replace the final `Ok(stats)` with:

```rust
    let set_counts: HashMap<u32, SetInventoryCounts> = set_shape
        .into_iter()
        .map(|(set_id, (molds, pieces_main))| {
            (
                set_id,
                SetInventoryCounts {
                    distinct_part_count: u32::try_from(molds.len()).expect("mold count fits u32"),
                    pieces_main,
                },
            )
        })
        .collect();
    Ok((stats, set_counts))
```

And change the signature (line ~193):

```rust
pub(crate) fn build(
    conn: &Connection,
    metadata_cache: &Path,
    resolver: &PartResolver<'_>,
) -> Result<(InventoryStats, HashMap<u32, SetInventoryCounts>)> {
```

- [ ] **Step 6: Write the failing `classify` tests (new module)**

Create `crates/catalog-builder/src/build/set_classify.rs` containing **only** its `tests` module for now, so the failing test drives the API:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::inventory::SetInventoryCounts;

    fn counts(distinct: u32, pieces: i64) -> SetInventoryCounts {
        SetInventoryCounts { distinct_part_count: distinct, pieces_main: pieces }
    }

    #[test]
    fn high_distinct_pack_named_set_is_still_a_build() {
        // A "Battle Pack" is a real build with many molds — the ceiling protects it.
        let c = counts(60, 180);
        assert_eq!(classify(Some(&c), "Clone Troopers Battle Pack"), SetType::Build);
    }

    #[test]
    fn single_mold_set_is_a_parts_pack() {
        let c = counts(1, 100);
        assert_eq!(classify(Some(&c), "2 x 4 Black Bricks"), SetType::PartsPack);
    }

    #[test]
    fn baseplate_and_merchandise_names_win_below_ceiling() {
        assert_eq!(classify(Some(&counts(1, 1)), "Green Baseplate"), SetType::Baseplate);
        assert_eq!(classify(Some(&counts(3, 6)), "LEGO Star Wars Watch"), SetType::Merchandise);
    }

    #[test]
    fn high_concentration_low_distinct_is_a_parts_pack() {
        // 2 molds, 40 pieces → 20 pieces/mold ≥ 15.
        assert_eq!(classify(Some(&counts(2, 40)), "Bricks and Half Bricks"), SetType::PartsPack);
    }

    #[test]
    fn small_diverse_set_stays_a_build() {
        // 8 molds, 45 pieces → ~5.6 pieces/mold, no keyword → build.
        assert_eq!(classify(Some(&counts(8, 45)), "Safari Basic Set"), SetType::Build);
    }

    #[test]
    fn no_inventory_is_unknown_unless_a_keyword_hits() {
        assert_eq!(classify(None, "Some Promotional Thing"), SetType::Unknown);
        assert_eq!(classify(None, "32 x 32 Baseplate"), SetType::Baseplate);
    }
}
```

- [ ] **Step 7: Run to confirm it fails**

Run: `cargo test -p brickdata-catalog-builder set_classify`
Expected: FAIL to compile — `classify`, `SetType` not found.

- [ ] **Step 8: Implement the classifier (above the tests module)**

Prepend to `crates/catalog-builder/src/build/set_classify.rs`:

```rust
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
const MERCHANDISE_KEYWORDS: &[&str] = &["watch", "magnet", "keychain", "key chain", "ornament", "clock"];
/// Bulk / assortment markers. Deliberately specific: bare "pack" is absent (it
/// matches "Battle Pack" / "Booster Pack" / "Backpack", all builds). "tub" is
/// omitted (it collides with "tube"); tubs are caught by [`CONCENTRATION`].
const PACK_PHRASES: &[&str] =
    &["pack of", "parts pack", "spare", "assorted", "assortment", "bulk", "bucket", "supplementary"];

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
```

- [ ] **Step 9: Run the classifier tests to confirm they pass**

Run: `cargo test -p brickdata-catalog-builder set_classify`
Expected: PASS (all six `classify` tests).

- [ ] **Step 10: Write the failing test for `set_classify::build`**

Add to the `tests` module in `set_classify.rs`:

```rust
    #[test]
    fn build_adds_columns_classifies_every_set_and_tallies() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE rb_sets (
                 set_num_rb TEXT PRIMARY KEY, name TEXT NOT NULL, set_id INTEGER NOT NULL
             ) WITHOUT ROWID;
             INSERT INTO rb_sets VALUES
                 ('10-1', 'Fire Station',        1),  -- build (diverse, in counts)
                 ('20-1', '2 x 4 Black Bricks',  2),  -- parts_pack (1 mold)
                 ('30-1', 'Green Baseplate',     3),  -- baseplate (name)
                 ('40-1', 'Promo Keyring Thing', 4);  -- unknown (no inventory, no kw)",
        )
        .unwrap();

        let mut counts: HashMap<u32, SetInventoryCounts> = HashMap::new();
        counts.insert(1, SetInventoryCounts { distinct_part_count: 40, pieces_main: 300 });
        counts.insert(2, SetInventoryCounts { distinct_part_count: 1, pieces_main: 100 });
        counts.insert(3, SetInventoryCounts { distinct_part_count: 1, pieces_main: 1 });
        // set 4 absent → no catalogued inventory.

        let stats = build(&conn, &counts).unwrap();

        let set_type = |sid: i64| -> String {
            conn.query_row("SELECT set_type FROM rb_sets WHERE set_id = ?1", [sid], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(set_type(1), "build");
        assert_eq!(set_type(2), "parts_pack");
        assert_eq!(set_type(3), "baseplate");
        assert_eq!(set_type(4), "unknown");

        // distinct_part_count is filled for sets with inventory, NULL otherwise.
        let distinct: Option<i64> = conn
            .query_row("SELECT distinct_part_count FROM rb_sets WHERE set_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(distinct, Some(40));
        let none: Option<i64> = conn
            .query_row("SELECT distinct_part_count FROM rb_sets WHERE set_id = 4", [], |r| r.get(0))
            .unwrap();
        assert_eq!(none, None);

        assert_eq!(stats.build, 1);
        assert_eq!(stats.parts_pack, 1);
        assert_eq!(stats.baseplate, 1);
        assert_eq!(stats.unknown, 1);
    }
```

- [ ] **Step 11: Run to confirm it fails**

Run: `cargo test -p brickdata-catalog-builder build_adds_columns`
Expected: FAIL to compile — `build`, `ClassifyStats` not found.

- [ ] **Step 12: Implement `ClassifyStats` and `build` (between `classify` and the tests module)**

```rust
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
/// set. `counts` is the per-`set_id` raw shape from [`inventory::build`]; a set
/// absent from it has no catalogued inventory (NULL distinct, name-only class).
/// The scan/UPDATE runs `ORDER BY set_id` and `classify` is pure, so the result
/// is deterministic (reproducible-build guarantee, #73).
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
        mapped.collect::<rusqlite::Result<Vec<_>>>().context("read rb_sets rows")?
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
```

- [ ] **Step 13: Run the `build` test to confirm it passes**

Run: `cargo test -p brickdata-catalog-builder build_adds_columns`
Expected: PASS.

- [ ] **Step 14: Wire the driver — module, schema bump, call, meta stamps**

In `crates/catalog-builder/src/build.rs`:

Add the module declaration (alongside the other `mod` lines, after `mod resolve;`):

```rust
mod set_classify;
```

Bump the schema constant and its comment (line ~48):

```rust
/// Bumped whenever the on-disk schema changes in a way the runtime must
/// notice. Stamped into `meta` so a mismatched DB is detectable at load.
/// v2 (#12): `colors` + `rb_part_external_id` tables. v3: resolved
/// `design_id` column on `rb_parts`. v4 (#19): `distinct_part_count` +
/// `set_type` columns on `rb_sets`.
const SCHEMA_VERSION: u32 = 4;
```

Destructure the inventory return and classify right after (replace line ~225–226 `let inv = inventory::build(...)?;` / `stamp_all(&conn, &inv.meta_rows())?;`):

```rust
    let (inv, set_counts) = inventory::build(&conn, csv_dir, &resolver)?;
    stamp_all(&conn, &inv.meta_rows())?;

    // Classify every set (#19): build vs parts_pack / baseplate / merchandise /
    // unknown, stored on rb_sets. Runs after inventory (needs the true per-set
    // molds) and after rb_sets + set_ids exist. Only builds feed the frequency
    // sidecar; the thresholds/policy are stamped for reproducibility.
    let cls = set_classify::build(&conn, &set_counts)?;
    stamp_all(&conn, &cls.meta_rows())?;
    stamp(&conn, "set_class_distinct_ceiling", &set_classify::DISTINCT_CEILING.to_string())?;
    stamp(&conn, "set_class_concentration", &set_classify::CONCENTRATION.to_string())?;
    stamp(&conn, "part_frequency_included_set_types", "build")?;
```

- [ ] **Step 15: Update the integration test — schema 4 + set-type meta**

In `crates/catalog-builder/tests/build.rs`, `build_creates_db_with_meta_rows`, change line ~225:

```rust
    assert_eq!(meta.get("schema_version").map(String::as_str), Some("4"));
```

Add, before the closing `let _ = std::fs::remove_dir_all(&root);` of that test:

```rust
    // #19: classifier ran, thresholds + per-type counts recorded.
    assert_eq!(
        meta.get("set_class_distinct_ceiling").map(String::as_str),
        Some("14")
    );
    assert!(
        meta.contains_key("set_type_build_count"),
        "per-type set counts should be stamped"
    );
    // Every catalogued set is a build or a specific non-build type; the fixture
    // has real inventories, so at least one build must exist.
    let builds: i64 = meta
        .get("set_type_build_count")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    assert!(builds > 0, "fixture should yield at least one build set");
```

- [ ] **Step 16: Run the crate's whole test suite**

Run: `cargo test -p brickdata-catalog-builder`
Expected: PASS (new unit tests + updated integration test + all existing tests).

- [ ] **Step 17: Lint and format gates**

Run:
```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
rustfmt crates/catalog-builder/src/build/set_classify.rs crates/catalog-builder/src/build/inventory.rs crates/catalog-builder/src/build.rs crates/catalog-builder/tests/build.rs
cargo fmt --all --check
```
Expected: clippy clean; `cargo fmt --all --check` exits 0.

- [ ] **Step 18: Commit**

```bash
git add crates/catalog-builder/src/build/inventory.rs \
        crates/catalog-builder/src/build/set_classify.rs \
        crates/catalog-builder/src/build.rs \
        crates/catalog-builder/tests/build.rs
git commit -m "feat(catalog): classify sets on rb_sets.set_type (schema v4) (#19)

Compute each set's true distinct-mold count + non-spare pieces from raw
inventory_parts, classify build/parts_pack/baseplate/merchandise/unknown via a
distinct-mold-ceiling-gated pure classifier (no LDraw categories), and store it
on rb_sets. Thresholds + per-type counts stamped into meta.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Qs6JKeTvZkJ7kY17bWWM7e"
```

---

### Task 2: Make the frequency sidecar count builds only

**Files:**
- Modify: `crates/catalog-builder/src/build/part_frequency.rs`
- Modify: `crates/catalog-builder/tests/build.rs`

**Interfaces:**
- Consumes: `rb_sets.set_type` (from Task 1).
- Produces: `part_frequency.ron` per-part fields `builds` / `build_qty`; `PartFreq { builds: Series, build_qty: Series }`.

- [ ] **Step 1: Update the test seed to add `rb_sets`, and write the exclusion test**

In `part_frequency.rs`'s `tests` module, replace the `seed` function so it also creates a classified `rb_sets` the aggregation can join (all four seed sets are builds, preserving the existing assertions):

```rust
    fn seed(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE rb_part_color_set (
                 design_id TEXT NOT NULL, color_id INTEGER NOT NULL, set_id INTEGER NOT NULL,
                 qty INTEGER NOT NULL, qty_spare INTEGER NOT NULL, year INTEGER,
                 PRIMARY KEY (design_id, color_id, set_id)
             ) WITHOUT ROWID;
             CREATE TABLE rb_sets (set_id INTEGER PRIMARY KEY, set_type TEXT NOT NULL);
             INSERT INTO rb_sets VALUES (1,'build'),(2,'build'),(3,'build'),(4,'build');
             INSERT INTO rb_part_color_set VALUES
                 ('A', 0, 1, 4, 0, 2026),
                 ('A', 1, 1, 2, 0, 2026),
                 ('A', 0, 2, 1, 0, 2025),
                 ('B', 0, 3, 1, 0, 2020),
                 ('B', 0, 4, 1, 0, NULL);",
        )
        .unwrap();
    }
```

Add a new test that flips a set to non-build and asserts it drops out:

```rust
    #[test]
    fn non_build_sets_are_excluded_from_the_aggregation() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        // Reclassify set 2 (A's 2025 appearance) as a parts pack.
        conn.execute("UPDATE rb_sets SET set_type = 'parts_pack' WHERE set_id = 2", [])
            .unwrap();
        let agg = aggregate(&conn).unwrap();
        // A now appears only in build set 1 (2026); its 2025 build count is gone.
        let a = &agg.parts["A"];
        assert_eq!(a.builds.all_time, 1);
        assert_eq!(a.builds.by_year, vec![1, 0, 0, 0, 0, 0]);
        assert_eq!(a.build_qty.all_time, 6); // set 1 only: 4 + 2
    }
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p brickdata-catalog-builder part_frequency`
Expected: FAIL to compile — no field `builds`/`build_qty`; and/or the existing `seed`-based tests fail because `aggregate` doesn't yet join `rb_sets`.

- [ ] **Step 3: Rename the `PartFreq` fields**

In `part_frequency.rs`, update the struct and its doc comment:

```rust
/// Per-part usage figures over **builds only** (sets classified `build`): how
/// many distinct builds contain the part (`builds`) and the total quantity of
/// the part across those builds (`build_qty`).
pub(crate) struct PartFreq {
    pub builds: Series,
    pub build_qty: Series,
}
```

- [ ] **Step 4: Filter the three aggregation queries to builds and use the new fields**

In `aggregate`, the all-time query and its row handling become:

```rust
        let mut stmt = conn.prepare(
            "SELECT pcs.design_id, COUNT(DISTINCT pcs.set_id), SUM(pcs.qty) \
             FROM rb_part_color_set pcs \
             JOIN rb_sets s ON s.set_id = pcs.set_id \
             WHERE s.set_type = 'build' \
             GROUP BY pcs.design_id",
        )?;
```

with the `parts.insert` using the renamed fields:

```rust
            parts.insert(
                design,
                PartFreq {
                    builds: Series { all_time: n_sets as u32, by_year: Vec::new() },
                    build_qty: Series { all_time: n_qty as u32, by_year: Vec::new() },
                },
            );
```

The anchor query:

```rust
    let anchor: Option<i64> = conn.query_row(
        "SELECT MAX(pcs.year) FROM rb_part_color_set pcs \
         JOIN rb_sets s ON s.set_id = pcs.set_id WHERE s.set_type = 'build'",
        [],
        |r| r.get(0),
    )?;
```

The per-year loop init and query:

```rust
    for part in parts.values_mut() {
        part.builds.by_year = vec![0; years.len()];
        part.build_qty.by_year = vec![0; years.len()];
    }
```

```rust
        let mut stmt = conn.prepare(
            "SELECT pcs.design_id, pcs.year, COUNT(DISTINCT pcs.set_id), SUM(pcs.qty) \
             FROM rb_part_color_set pcs \
             JOIN rb_sets s ON s.set_id = pcs.set_id \
             WHERE s.set_type = 'build' AND pcs.year BETWEEN ?1 AND ?2 \
             GROUP BY pcs.design_id, pcs.year",
        )?;
```

and its row fill:

```rust
            if let (Some(part), Some(&idx)) = (parts.get_mut(&design), year_index.get(&year)) {
                part.builds.by_year[idx] = n_sets as u32;
                part.build_qty.by_year[idx] = n_qty as u32;
            }
```

- [ ] **Step 5: Update `render` and the header**

In `render`, the per-part line:

```rust
        out.push_str(&format!(
            "        {}: (builds: {}, build_qty: {}),\n",
            ron_quote(design),
            render_series(&freq.builds),
            render_series(&freq.build_qty),
        ));
```

Replace the `HEADER` constant:

```rust
const HEADER: &str = "\
// Per-part usage figures over builds.
//
// GENERATED by the catalog `build` from rb_part_color_set — do not edit by hand.
// For each part: `builds` is the number of distinct BUILDS that contain it;
// `build_qty` is the total quantity of the part across those builds. Each has an
// `all_time` total and a `by_year` breakdown, positionally aligned to `years`
// (most-recent first). Pick a window by summing a prefix of by_year; older-than-
// window usage is `all_time - sum(by_year)`.
//
// A build is a set classified `build` in rb_sets.set_type (brickdata#19):
// parts packs, baseplates, merchandise, and sets with no catalogued inventory
// are excluded. Classifier: distinct-mold ceiling 14, concentration 15. The
// window is anchored on the newest build year in the data, so this file is a
// pure function of the pinned catalog. See bkfunk/brickdata#17, #19.
";
```

- [ ] **Step 6: Update the remaining existing tests to the new field names**

In `part_frequency.rs` tests, rename every `.sets`→`.builds` and `.qty`→`.build_qty` on `PartFreq`, and in `render_is_key_sorted_and_valid_ron` rename the `Aggregation`/`PartFreq` literal fields and the expected RON substrings (`sets: (all_time` → `builds: (all_time`, `qty: (all_time` → `build_qty: (all_time`) and the `ParsedRow` fields:

```rust
        #[derive(serde::Deserialize)]
        struct ParsedRow {
            builds: ParsedSeries,
            build_qty: ParsedSeries,
        }
```
```rust
        assert_eq!(doc.parts["A"].builds.all_time, 2);
        assert_eq!(doc.parts["A"].builds.by_year, vec![1, 1, 0, 0, 0, 0]);
        assert_eq!(doc.parts["A"].build_qty.all_time, 7);
        assert_eq!(doc.parts["A"].build_qty.by_year, vec![6, 1, 0, 0, 0, 0]);
```

Apply the same `.sets`→`.builds`, `.qty`→`.build_qty` renames in `counts_sets_and_quantity_per_year_with_all_time_totals` and `generate_writes_sidecar_drawing_provenance_from_meta` (whose seed now provides `rb_sets`), and update that test's expected line to `"A": (builds: (all_time: 2, by_year: [1, 1, 0, 0, 0, 0]), build_qty: (all_time: 7, by_year: [6, 1, 0, 0, 0, 0])),`.

- [ ] **Step 7: Run the sidecar unit tests**

Run: `cargo test -p brickdata-catalog-builder part_frequency`
Expected: PASS (exclusion test + all renamed tests).

- [ ] **Step 8: Add an integration assertion for the renamed field**

In `crates/catalog-builder/tests/build.rs`, `build_emits_part_frequency_sidecar_next_to_the_db`, after the `PartFrequency(` assertion:

```rust
    // #19: the sidecar now reports builds, not raw sets.
    assert!(text.contains("builds:"), "sidecar should carry build counts:\n{text}");
    assert!(text.contains("build_qty:"), "sidecar should carry build quantities");
```

- [ ] **Step 9: Full crate tests + lint + format**

Run:
```bash
cargo test -p brickdata-catalog-builder
cargo clippy --workspace --all-targets --all-features -- -D warnings
rustfmt crates/catalog-builder/src/build/part_frequency.rs crates/catalog-builder/tests/build.rs
cargo fmt --all --check
```
Expected: all PASS / clean.

- [ ] **Step 10: Commit**

```bash
git add crates/catalog-builder/src/build/part_frequency.rs crates/catalog-builder/tests/build.rs
git commit -m "feat(catalog): part_frequency counts builds only (builds/build_qty) (#19)

Join rb_sets and keep only set_type='build' in every aggregation query; rename
the sidecar's per-part fields sets->builds, qty->build_qty; record the filter
policy + thresholds in the RON header.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Qs6JKeTvZkJ7kY17bWWM7e"
```

---

### Task 3: Full-build verification & acceptance spot-check

Validates the acceptance spot-check against a real catalog. No code changes / no commit — a gate before opening the PR. Uses the CSVs already cached under `work/cache` (offline).

**Files:** none (runs the builder, inspects `work/`).

- [ ] **Step 1: Run the full workspace test matrix (mirrors CI)**

Run:
```bash
cargo test --workspace --all-features
cargo test --workspace --no-default-features
cargo check --workspace --all-features   # MSRV-shaped check
```
Expected: all PASS.

- [ ] **Step 2: Build the real catalog from the committed pins**

Run: `just build-catalog`
(Equivalent to `cargo run --release -p brickdata-catalog-builder -- build pins/rebrickable-2026-06-01.ron pins/ldraw-2026-06-01.ron work/catalog.sqlite`.)
Expected: writes `work/catalog.sqlite` and `work/part_frequency.ron` with no error.

- [ ] **Step 3: Spot-check classifications (acceptance)**

Run:
```bash
sqlite3 work/catalog.sqlite "SELECT set_type, COUNT(*) FROM rb_sets GROUP BY set_type ORDER BY 2 DESC;"
sqlite3 work/catalog.sqlite "SELECT name, distinct_part_count, set_type FROM rb_sets
  WHERE name IN ('Technic Chainlinks','2 x 4 Black Bricks','Green Baseplate')
     OR name LIKE '%Battle Pack%' ORDER BY set_type LIMIT 20;"
```
Expected: known bulk packs / baseplates classify non-`build`; sets named "…Battle Pack" (high distinct) classify `build`. Confirm the per-type counts are plausible (`build` is the large majority; `unknown` ≈ the no-inventory sets).

- [ ] **Step 4: Confirm the sidecar is build-based**

Run:
```bash
head -20 work/part_frequency.ron
grep -c "builds:" work/part_frequency.ron
sqlite3 work/catalog.sqlite "SELECT key, value FROM meta
  WHERE key IN ('schema_version','set_class_distinct_ceiling','set_class_concentration',
                'part_frequency_included_set_types','set_type_build_count');"
```
Expected: header states builds-only + policy; every part line uses `builds:` / `build_qty:`; `schema_version = 4`; thresholds and `part_frequency_included_set_types = build` present.

## Self-Review

- **Spec coverage:** distinct_part_count from raw inventory → Task 1 Steps 3,5. `set_type` stored for every set → Task 1 Steps 12,14. Sidecar builds-only + `builds`/`build_qty` + header policy → Task 2. `meta` policy/thresholds → Task 1 Step 14. Schema v4 → Task 1 Step 14. Spot-check acceptance → Task 3. `unknown` = no-inventory → Task 1 classifier + test Step 10. All spec sections covered.
- **Placeholder scan:** none — every step has concrete code/commands.
- **Type consistency:** `SetInventoryCounts { distinct_part_count: u32, pieces_main: i64 }` consumed identically in `set_classify`; `classify(Option<&SetInventoryCounts>, &str) -> SetType` used by `build`; `PartFreq { builds, build_qty }` used consistently in aggregate/render/tests; `inventory::build` tuple return destructured in `build.rs` (Step 14) matching the signature (Step 5).

## Known follow-ups (not in this plan)

- File a brickdata issue: investigate whether the dominant part's LDraw category improves baseplate/merchandise classification.
- The `part_frequency.ron` release-asset publishing (#17's remaining task) and the `rb_part_summary` build columns (#130) remain out of scope.
