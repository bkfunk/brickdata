//! Project per-part usage figures into `part_frequency.ron`.
//!
//! A sidecar to `catalog.sqlite`: for every part that appears in a **build**
//! (a set classified `build` in `rb_sets.set_type`), two measures — the number
//! of **distinct builds** containing it (`builds`) and the **total quantity**
//! of the part across those builds (`build_qty`) — each as an all-time total
//! plus a **per-calendar-year** breakdown over a recent window. Consumers pick
//! their own "recent" window by summing a prefix of the year list, and choose
//! whichever measure suits their purpose.
//!
//! Emitted as a lightweight RON so consumers that only need per-part usage
//! figures need not open the full catalog. The window is anchored on the newest
//! build `year` in the data, not wall-clock, so the projection is a pure
//! function of the pinned input.
//!
//! These are catalog-structural counts (how the set catalog is built), not
//! sales-weighted demand. Non-build sets — parts packs, baseplates,
//! merchandise, and sets with no catalogued inventory — are excluded via
//! `set_classify` (brickdata#19); weighted popularity metrics remain future
//! work (brickdata#17).

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::util::ron_quote;

/// Calendar years to cover, counting back from the anchor: `anchor - N ..=
/// anchor`. Five years back → six buckets.
const WINDOW_YEARS_BACK: i64 = 5;

/// One measure across time: an all-time total and a per-year breakdown,
/// positionally aligned to [`Aggregation::years`].
pub(crate) struct Series {
    pub all_time: u32,
    pub by_year: Vec<u32>,
}

/// Per-part usage figures over **builds only** (sets classified `build`): how
/// many distinct builds contain the part (`builds`) and the total quantity of
/// the part across those builds (`build_qty`).
pub(crate) struct PartFreq {
    pub builds: Series,
    pub build_qty: Series,
}

/// The whole projection: the covered years (most-recent first) and one
/// [`PartFreq`] per part that appears in any set, keyed by `design_id`.
pub(crate) struct Aggregation {
    pub years: Vec<i64>,
    pub parts: BTreeMap<String, PartFreq>,
}

/// Aggregate `rb_part_color_set` (builds only) into per-part, per-year build
/// counts and quantities.
pub(crate) fn aggregate(conn: &Connection) -> Result<Aggregation> {
    // All-time figures per part, independent of year — so a part appearing only
    // in undated or out-of-window builds is still represented. `build_qty` sums
    // across colors and builds; `builds` counts distinct builds regardless of
    // color.
    let mut parts: BTreeMap<String, PartFreq> = BTreeMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT pcs.design_id, COUNT(DISTINCT pcs.set_id), SUM(pcs.qty) \
             FROM rb_part_color_set pcs \
             JOIN rb_sets s ON s.set_id = pcs.set_id \
             WHERE s.set_type = 'build' \
             GROUP BY pcs.design_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (design, n_sets, n_qty) = row?;
            parts.insert(
                design,
                PartFreq {
                    builds: Series {
                        all_time: n_sets as u32,
                        by_year: Vec::new(),
                    },
                    build_qty: Series {
                        all_time: n_qty as u32,
                        by_year: Vec::new(),
                    },
                },
            );
        }
    }

    // Window anchored on the newest build year present, not wall-clock, so the
    // projection is reproducible from the pinned input.
    let anchor: Option<i64> = conn.query_row(
        "SELECT MAX(pcs.year) FROM rb_part_color_set pcs \
         JOIN rb_sets s ON s.set_id = pcs.set_id WHERE s.set_type = 'build'",
        [],
        |r| r.get(0),
    )?;
    let years: Vec<i64> = match anchor {
        Some(a) => (0..=WINDOW_YEARS_BACK).map(|back| a - back).collect(),
        None => Vec::new(),
    };
    let year_index: BTreeMap<i64, usize> = years.iter().enumerate().map(|(i, &y)| (y, i)).collect();
    for part in parts.values_mut() {
        part.builds.by_year = vec![0; years.len()];
        part.build_qty.by_year = vec![0; years.len()];
    }

    // Fill the in-window buckets. `BETWEEN` excludes NULL years, so undated
    // rows contribute to the all-time totals above but never to a year bucket.
    if let (Some(&newest), Some(&oldest)) = (years.first(), years.last()) {
        let mut stmt = conn.prepare(
            "SELECT pcs.design_id, pcs.year, COUNT(DISTINCT pcs.set_id), SUM(pcs.qty) \
             FROM rb_part_color_set pcs \
             JOIN rb_sets s ON s.set_id = pcs.set_id \
             WHERE s.set_type = 'build' AND pcs.year BETWEEN ?1 AND ?2 \
             GROUP BY pcs.design_id, pcs.year",
        )?;
        let rows = stmt.query_map((oldest, newest), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (design, year, n_sets, n_qty) = row?;
            if let (Some(part), Some(&idx)) = (parts.get_mut(&design), year_index.get(&year)) {
                part.builds.by_year[idx] = n_sets as u32;
                part.build_qty.by_year[idx] = n_qty as u32;
            }
        }
    }

    Ok(Aggregation { years, parts })
}

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

/// Render the aggregation as `part_frequency.ron`: a header, provenance, the
/// covered `years`, then one key-sorted
/// `"design_id": (builds: (…), build_qty: (…))` line per part. Hand-rolled for
/// a tight, deterministic, reviewable diff —
/// same rationale as `part_crossrefs.ron` / `color_names.ron`.
pub(crate) fn render(snapshot: &str, snapshot_date: &str, agg: &Aggregation) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    out.push_str("PartFrequency(\n");
    out.push_str(&format!("    generated_from: {},\n", ron_quote(snapshot)));
    out.push_str(&format!(
        "    snapshot_date: {},\n",
        ron_quote(snapshot_date)
    ));
    out.push_str(&format!("    years: {},\n", render_ints(&agg.years)));
    out.push_str("    parts: {\n");
    for (design, freq) in &agg.parts {
        out.push_str(&format!(
            "        {}: (builds: {}, build_qty: {}),\n",
            ron_quote(design),
            render_series(&freq.builds),
            render_series(&freq.build_qty),
        ));
    }
    out.push_str("    },\n");
    out.push_str(")\n");
    out
}

/// A [`Series`] as `(all_time: N, by_year: [...])`.
fn render_series(s: &Series) -> String {
    format!(
        "(all_time: {}, by_year: {})",
        s.all_time,
        render_ints(&s.by_year)
    )
}

/// A RON int array — `[a, b, c]` — for any integer type.
fn render_ints<T: std::fmt::Display>(xs: &[T]) -> String {
    let items: Vec<String> = xs.iter().map(|x| x.to_string()).collect();
    format!("[{}]", items.join(", "))
}

/// Aggregate, render, and write `part_frequency.ron` to `out_path`, drawing
/// provenance from the DB's own `meta` table. Written via [`util::atomic_write`]
/// (temp sibling + fsync + rename) so a crash never leaves a torn file. Returns
/// the part count.
pub(crate) fn generate(conn: &Connection, out_path: &std::path::Path) -> Result<usize> {
    let snapshot = meta_value(conn, "rebrickable_snapshot")?;
    let snapshot_date = meta_value(conn, "snapshot_date")?;
    let agg = aggregate(conn)?;
    let count = agg.parts.len();
    let text = render(&snapshot, &snapshot_date, &agg);

    // Same durability the DB build takes (see `run_with`): fsync the staged
    // temp before the rename, so a power loss never leaves a torn sidecar.
    crate::util::atomic_write(out_path, text.as_bytes())?;
    Ok(count)
}

/// Read a `meta` string value, erroring if the key is absent.
fn meta_value(conn: &Connection, key: &str) -> Result<String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .with_context(|| format!("meta key {key:?} missing"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `rb_part_color_set` with rows exercising the three behaviors
    /// that matter: distinct-set counting across colors, the year window's
    /// bounds, and NULL-year rows.
    fn seed(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE rb_part_color_set (
                 design_id TEXT NOT NULL, color_id INTEGER NOT NULL, set_id INTEGER NOT NULL,
                 qty INTEGER NOT NULL, qty_spare INTEGER NOT NULL, year INTEGER,
                 PRIMARY KEY (design_id, color_id, set_id)
             ) WITHOUT ROWID;
             CREATE TABLE rb_sets (set_id INTEGER PRIMARY KEY, set_type TEXT NOT NULL);
             INSERT INTO rb_sets VALUES (1, 'build'), (2, 'build'), (3, 'build'), (4, 'build');
             INSERT INTO rb_part_color_set VALUES
                 -- A: set 1 in two colors (one distinct set) + set 2, both in-window
                 ('A', 0, 1, 4, 0, 2026),
                 ('A', 1, 1, 2, 0, 2026),
                 ('A', 0, 2, 1, 0, 2025),
                 -- B: one set older than the window, one with an unknown year
                 ('B', 0, 3, 1, 0, 2020),
                 ('B', 0, 4, 1, 0, NULL);",
        )
        .unwrap();
    }

    #[test]
    fn window_is_anchored_on_max_year_most_recent_first() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        let agg = aggregate(&conn).unwrap();
        // MAX(year) is 2026, so the six buckets run 2026 down to 2021.
        assert_eq!(agg.years, vec![2026, 2025, 2024, 2023, 2022, 2021]);
    }

    #[test]
    fn counts_sets_and_quantity_per_year_with_all_time_totals() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        let agg = aggregate(&conn).unwrap();

        // A: set 1 counts once despite two color rows; set 2 in 2025.
        // Quantity sums across colors: set 1 = 4 + 2 = 6 (2026), set 2 = 1 (2025).
        let a = &agg.parts["A"];
        assert_eq!(a.builds.all_time, 2);
        assert_eq!(a.builds.by_year, vec![1, 1, 0, 0, 0, 0]); // 2026, 2025, then zeros
        assert_eq!(a.build_qty.all_time, 7);
        assert_eq!(a.build_qty.by_year, vec![6, 1, 0, 0, 0, 0]);

        // B: both sets and their quantities are all-time, but neither lands in a
        // window bucket — 2020 is older than the window, NULL has no year.
        let b = &agg.parts["B"];
        assert_eq!(b.builds.all_time, 2);
        assert_eq!(b.builds.by_year, vec![0, 0, 0, 0, 0, 0]);
        assert_eq!(b.build_qty.all_time, 2);
        assert_eq!(b.build_qty.by_year, vec![0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn non_build_sets_are_excluded_from_the_aggregation() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn);
        // Reclassify set 2 (A's 2025 appearance) as a parts pack.
        conn.execute(
            "UPDATE rb_sets SET set_type = 'parts_pack' WHERE set_id = 2",
            [],
        )
        .unwrap();
        let agg = aggregate(&conn).unwrap();
        // A now appears only in build set 1 (2026); its 2025 build count is gone.
        let a = &agg.parts["A"];
        assert_eq!(a.builds.all_time, 1);
        assert_eq!(a.builds.by_year, vec![1, 0, 0, 0, 0, 0]);
        assert_eq!(a.build_qty.all_time, 6); // set 1 only: 4 + 2
    }

    #[test]
    fn render_is_key_sorted_and_valid_ron() {
        let agg = Aggregation {
            years: vec![2026, 2025, 2024, 2023, 2022, 2021],
            parts: BTreeMap::from([
                (
                    "B".to_string(),
                    PartFreq {
                        builds: Series {
                            all_time: 2,
                            by_year: vec![0, 0, 0, 0, 0, 0],
                        },
                        build_qty: Series {
                            all_time: 2,
                            by_year: vec![0, 0, 0, 0, 0, 0],
                        },
                    },
                ),
                (
                    "A".to_string(),
                    PartFreq {
                        builds: Series {
                            all_time: 2,
                            by_year: vec![1, 1, 0, 0, 0, 0],
                        },
                        build_qty: Series {
                            all_time: 7,
                            by_year: vec![6, 1, 0, 0, 0, 0],
                        },
                    },
                ),
            ]),
        };
        let out = render("rebrickable-2026-06-01", "2026-06-01", &agg);

        // Provenance + covered years in the header.
        assert!(
            out.contains("generated_from: \"rebrickable-2026-06-01\","),
            "{out}"
        );
        assert!(
            out.contains("years: [2026, 2025, 2024, 2023, 2022, 2021],"),
            "{out}"
        );
        // One deterministic line per part, key-sorted (A before B), carrying
        // both the set-count and quantity series.
        assert!(
            out.contains(
                "\"A\": (builds: (all_time: 2, by_year: [1, 1, 0, 0, 0, 0]), \
                 build_qty: (all_time: 7, by_year: [6, 1, 0, 0, 0, 0])),"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "\"B\": (builds: (all_time: 2, by_year: [0, 0, 0, 0, 0, 0]), \
                 build_qty: (all_time: 2, by_year: [0, 0, 0, 0, 0, 0])),"
            ),
            "{out}"
        );
        assert!(out.find("\"A\":").unwrap() < out.find("\"B\":").unwrap());

        // Parses back as valid RON with the same data (a consumer will).
        // Distinct names from the module's own types so they don't shadow them.
        #[derive(serde::Deserialize)]
        struct ParsedSeries {
            all_time: u32,
            by_year: Vec<u32>,
        }
        #[derive(serde::Deserialize)]
        struct ParsedRow {
            builds: ParsedSeries,
            build_qty: ParsedSeries,
        }
        // Named to match the `PartFrequency(...)` wrapper — RON checks the
        // struct name, so a consumer names its deserialize target the same.
        #[derive(serde::Deserialize)]
        struct PartFrequency {
            generated_from: String,
            snapshot_date: String,
            years: Vec<i64>,
            parts: BTreeMap<String, ParsedRow>,
        }
        let doc: PartFrequency = ron::from_str(&out).expect("valid RON");
        assert_eq!(doc.generated_from, "rebrickable-2026-06-01");
        assert_eq!(doc.snapshot_date, "2026-06-01");
        assert_eq!(doc.years, vec![2026, 2025, 2024, 2023, 2022, 2021]);
        assert_eq!(doc.parts["A"].builds.all_time, 2);
        assert_eq!(doc.parts["A"].builds.by_year, vec![1, 1, 0, 0, 0, 0]);
        assert_eq!(doc.parts["A"].build_qty.all_time, 7);
        assert_eq!(doc.parts["A"].build_qty.by_year, vec![6, 1, 0, 0, 0, 0]);
    }

    #[test]
    fn generate_writes_sidecar_drawing_provenance_from_meta() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta VALUES
                 ('rebrickable_snapshot', 'rebrickable-2026-06-01'),
                 ('snapshot_date', '2026-06-01');",
        )
        .unwrap();
        seed(&conn);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("part_frequency.ron");
        let count = generate(&conn, &path).unwrap();

        assert_eq!(count, 2); // parts A and B
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("generated_from: \"rebrickable-2026-06-01\","),
            "{text}"
        );
        assert!(text.contains(
            "\"A\": (builds: (all_time: 2, by_year: [1, 1, 0, 0, 0, 0]), \
             build_qty: (all_time: 7, by_year: [6, 1, 0, 0, 0, 0])),"
        ));
    }
}
