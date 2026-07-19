//! Integration tests for the `build` subcommand: the DB is produced with its
//! `meta` rows (#69), the snapshot-verify gate fails loudly (#69), the
//! `ldraw_part` table is populated from the fixture library (#70), the small
//! Rebrickable tables are raw-ingested with empty-field-as-NULL (#71),
//! `inventory_parts` aggregates into the fact + summary tables (#72), and
//! `part_relationships` ingests catalog-filtered (#82).

use brickdata::pin::{AssetFingerprint, RebrickablePin};
use brickdata_catalog_builder::core::blob::unpack_u32_le;
use brickdata_catalog_builder::core::{Category, PartCatalog};
use brickdata_catalog_builder::{build, util};
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Synthetic CSV content for the six tables #71 ingests. Each deliberately
/// exercises an empty field that must round-trip to SQL NULL: `elements`
/// (empty `design_id`), `themes` (empty top-level `parent_id`), and `sets`
/// (empty `img_url`).
const RB_CSVS: &[(&str, &str)] = &[
    (
        "parts",
        // 92693's Rebrickable name carries a token ("Long") absent from the
        // LDraw name of design 92693c01 — the FTS rb_name test keys on it.
        // 3005 is deliberately absent from the crossrefs pin but present in
        // the fixture library: the literal-fallback case (#112); its rb name
        // carries a token ("Classic") for the FTS literal-join test.
        "part_num,name,part_cat_id,part_material\n\
         3001,Brick 2 x 4,11,Plastic\n\
         3005,Brick 1 x 1 Classic,11,Plastic\n\
         3626,Minifig Head,13,Plastic\n\
         4073,Plate Round 1 x 1 Legacy,14,Plastic\n\
         92693,Technic Linear Actuator Long,52,Plastic\n",
    ),
    ("part_categories", "id,name\n11,Bricks\n13,Minifigs\n"),
    (
        "elements",
        "element_id,part_num,color_id,design_id\n\
         300126,3001,4,\n\
         4159553,3001,15,3001\n",
    ),
    ("themes", "id,name,parent_id\n1,Technic,\n50,Star Wars,1\n"),
    (
        "sets",
        // set_ids are assigned 1..=N by set_num order: 1000-1=1, 75000-1=2,
        // 8880-1=3.
        "set_num,name,year,theme_id,num_parts,img_url\n\
         1000-1,Test Set,1985,1,123,\n\
         8880-1,Super Car,1994,1,1343,https://example/img.png\n\
         75000-1,Spare Test,2023,1,5,\n",
    ),
    (
        "inventories",
        // Set 8880-1 has two versions; only the max (v2 = id 102) survives.
        "id,version,set_num\n\
         100,1,1000-1\n\
         101,1,8880-1\n\
         102,2,8880-1\n\
         103,1,75000-1\n",
    ),
    (
        "inventory_parts",
        // Columns: inventory_id,part_num,color_id,quantity,is_spare,img_url
        //   inv 100 → 1000-1 (1985); 102 → 8880-1 (1994, surviving); 101 →
        //   8880-1 v1 (dropped by dedup); 103 → 75000-1 (2023).
        //   3001 in 1000-1: main qty 2 + spare 1. nomap-part: no LDraw mapping.
        //   color 9999: no LDraw color. 3023: spare-only, and its pin design
        //   is an LDraw tombstone → its facts land on 3023b. 92693 →
        //   92693c01. 4073: unpinned tombstone → chased to 6141. 30071: hard
        //   alias of 3005 → merges into 3005's fact (#112).
        "inventory_id,part_num,color_id,quantity,is_spare,img_url\n\
         100,3001,4,2,False,\n\
         100,3001,4,1,True,\n\
         100,3626,15,1,False,\n\
         102,3001,4,5,False,\n\
         101,3001,4,99,False,\n\
         100,nomap-part,4,1,False,\n\
         100,3001,9999,1,False,\n\
         100,3005,4,3,False,\n\
         100,30071,4,4,False,\n\
         103,3023,4,1,True,\n\
         103,4073,4,2,False,\n\
         103,92693,4,1,False,\n",
    ),
    (
        "part_relationships",
        // One row per rel type (#82). In-catalog endpoints (pin-mapped AND in
        // the fixture library): 3001, 3024. 3626 is pin-mapped but NOT in the
        // fixture library (its design_id column must stay NULL — as would
        // 3023, whose fixture .dat is a `~Moved to` tombstone the catalog
        // scan excludes); the print/pattern/mold/sub-part counterparts aren't
        // in the pin at all. The 43722/43723 pair touches nothing in-catalog
        // → skipped. The final row is an exact duplicate → collapsed.
        "rel_type,child_part_num,parent_part_num\n\
         P,3001pr0001,3001\n\
         T,3024pat0002,3024\n\
         M,3001,3001a\n\
         A,3024,3001\n\
         R,3001,3626\n\
         B,3024,973c00\n\
         R,3005,43723\n\
         M,4073,6141\n\
         R,43722,43723\n\
         A,3024,3001\n",
    ),
];

/// A synthetic `part_crossrefs.ron` covering the part_nums the inventory
/// fixture references. `nomap-part` is deliberately absent (tests the
/// unmapped-part skip), and so is `3005` — it is in the fixture library, so
/// it must map via the literal fallback (#112); color 9999 is absent from
/// the color reference (tests the unmapped-color skip). `92693` maps to the
/// collapsed `92693c01` design.
const CROSSREFS_RON: &str = r#"RbCrossRefPin(
    generated: "2026-05-27",
    parts: {
        "3001": (ldraw: "3001", external_ids: {"BrickLink": ["3001"]}),
        "3023": (ldraw: "3023", external_ids: {}),
        "3024": (ldraw: "3024", external_ids: {}),
        "3626": (ldraw: "3626", external_ids: {}),
        "92693": (ldraw: "92693c01", external_ids: {}),
    },
)
"#;

fn write_crossrefs(root: &Path) -> PathBuf {
    let path = root.join("part_crossrefs.ron");
    std::fs::write(&path, CROSSREFS_RON).unwrap();
    path
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

/// Populate a fake per-tag CSV dir — one `.csv.gz` per pinned table — and a
/// matching in-memory pin. Every pinned table has a synthetic CSV in
/// `RB_CSVS` (the fallback arm keeps a future pinned-but-unread table from
/// breaking the setup). Returns `(pin, csv_dir)` — `run_with` takes the pin
/// by value now that fetch-time verification owns the hash gate.
fn fake_pin_and_csv_dir(root: &Path) -> (RebrickablePin, PathBuf) {
    let csv_dir = root.join("csvs");
    std::fs::create_dir_all(&csv_dir).unwrap();
    let mut file_fingerprints = BTreeMap::new();

    let mut add = |table: &str, bytes: Vec<u8>| {
        let filename = format!("{table}.csv.gz");
        std::fs::write(csv_dir.join(&filename), &bytes).unwrap();
        let mirror_url = format!(
            "https://github.com/bkfunk/brickdata/releases/download/\
             rebrickable-2026-05-27/{filename}"
        );
        file_fingerprints.insert(
            filename,
            AssetFingerprint {
                sha256: util::hash_bytes(&bytes),
                bytes: bytes.len() as u64,
                mirror_url,
            },
        );
    };

    let ingested: BTreeMap<&str, &str> = RB_CSVS.iter().copied().collect();
    for &table in build::TABLES {
        match ingested.get(table) {
            Some(csv) => add(table, gzip(csv.as_bytes())),
            None => add(table, format!("fake-{table}-content").into_bytes()),
        }
    }

    let pin = RebrickablePin {
        mirror_tag: "rebrickable-2026-05-27".into(),
        snapshot_date: "2026-05-27".into(),
        file_fingerprints,
    };
    (pin, csv_dir)
}

/// The committed fixture LDraw library in this crate.
fn fixture_ldraw_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ldraw")
}

fn temp_root(name: &str) -> PathBuf {
    // Include the pid so concurrent `cargo test` processes (e.g. two CI jobs on
    // one runner) get distinct dirs — the eager remove_dir_all below must never
    // delete another run's temp tree.
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("brickdata-build-test-{pid}-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn read_meta(db: &Path) -> HashMap<String, String> {
    let conn = Connection::open(db).unwrap();
    conn.prepare("SELECT key, value FROM meta")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn build_creates_db_with_meta_rows() {
    let root = temp_root("ok");
    let (pin, csv_dir) = fake_pin_and_csv_dir(&root);
    let crossrefs = write_crossrefs(&root);
    let out = root.join("catalog.sqlite");

    build::run_with(&pin, &csv_dir, &crossrefs, &fixture_ldraw_dir(), &out)
        .expect("build should succeed against a matching cache");

    assert!(out.exists(), "DB should exist at the output path");
    assert!(
        !root.join("catalog.sqlite.tmp").exists(),
        "the .tmp staging file should have been renamed away"
    );

    let meta = read_meta(&out);
    assert_eq!(meta.get("schema_version").map(String::as_str), Some("1"));
    assert_eq!(
        meta.get("snapshot_date").map(String::as_str),
        Some("2026-05-27")
    );
    assert_eq!(
        meta.get("rebrickable_snapshot").map(String::as_str),
        Some("rebrickable-2026-05-27"),
        "the mirror release tag is recorded for input-version traceability"
    );
    assert_eq!(
        meta.get("builder_version").map(String::as_str),
        Some(env!("CARGO_PKG_VERSION"))
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn build_fails_loudly_when_a_pinned_csv_is_missing() {
    let root = temp_root("missing");
    let (pin, csv_dir) = fake_pin_and_csv_dir(&root);
    let crossrefs = write_crossrefs(&root);
    // Simulate an un-fetched cache: a pinned file is absent.
    std::fs::remove_file(csv_dir.join("parts.csv.gz")).unwrap();
    let out = root.join("catalog.sqlite");

    let err = build::run_with(&pin, &csv_dir, &crossrefs, &fixture_ldraw_dir(), &out)
        .expect_err("build must fail when a pinned CSV is missing");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("parts.csv.gz"),
        "error names the missing file: {msg}"
    );
    assert!(
        !out.exists(),
        "no DB should be left behind on a failed build"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The hash gate moved from build-time cache verification into
/// `materialize_csv_dir`'s fetch path: a transport serving bytes that don't
/// match the pin's sha256 must fail verification, never becoming visible to
/// the build. (The `brickdata` crate owns the deeper Fetcher tests; this
/// pins the builder-side wiring.)
#[test]
fn materialize_rejects_bytes_that_do_not_match_the_pin() {
    use brickdata::fetch::{Fetcher, Transport, TransportError};

    struct Tampered;
    impl Transport for Tampered {
        fn get(&self, _url: &str, sink: &mut dyn Write) -> Result<u64, TransportError> {
            let body = b"tampered-content";
            sink.write_all(body).unwrap();
            Ok(body.len() as u64)
        }
    }

    let root = temp_root("drift");
    let (pin, _csv_dir) = fake_pin_and_csv_dir(&root);
    let cache = root.join("fetch-cache");
    let fetcher = Fetcher::with_transport(&cache, Tampered);

    let err = build::materialize_csv_dir(&fetcher, &pin, &cache)
        .expect_err("materialize must fail when fetched bytes don't match the pin");
    let msg = format!("{err:#}");
    assert!(
        msg.contains(&pin.mirror_tag),
        "error names the pin's mirror tag: {msg}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn build_populates_ldraw_part_from_fixture() {
    let root = temp_root("ldraw-part");
    let (pin, csv_dir) = fake_pin_and_csv_dir(&root);
    let crossrefs = write_crossrefs(&root);
    let out = root.join("catalog.sqlite");

    build::run_with(&pin, &csv_dir, &crossrefs, &fixture_ldraw_dir(), &out)
        .expect("build should succeed");

    let conn = Connection::open(&out).unwrap();

    // Row count matches the catalog scan exactly (after the -fN collapse).
    let expected = PartCatalog::build(fixture_ldraw_dir())
        .expect("scan fixture")
        .len();
    let rows = conn
        .query_row("SELECT COUNT(*) FROM ldraw_part", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap() as usize;
    assert_eq!(rows, expected, "ldraw_part row count != catalog len");
    assert!(rows > 0, "fixture produced no parts");

    // meta.ldraw_part_count agrees with the table.
    let meta = read_meta(&out);
    let rows_str = rows.to_string();
    assert_eq!(
        meta.get("ldraw_part_count").map(String::as_str),
        Some(rows_str.as_str()),
    );

    // A known fixture part round-trips with its classified metadata.
    let (name, category_id, dimensions, is_decorated): (String, i64, Option<Vec<u8>>, i64) = conn
        .query_row(
            "SELECT name, category_id, dimensions, is_decorated
             FROM ldraw_part WHERE design_id = '3001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("part 3001 present");
    assert!(name.starts_with("Brick"), "name was {name:?}");
    assert_eq!(category_id, Category::Bricks as i64, "3001 is a Brick");
    assert_eq!(is_decorated, 0, "3001 is not decorated");
    assert_eq!(
        unpack_u32_le(&dimensions.expect("3001 has dimensions")).expect("dimensions BLOB decodes"),
        vec![2, 4],
    );

    // An ordinary part: no flexion variants, and it has its own base .dat.
    let (flexion, has_base): (Option<Vec<u8>>, i64) = conn
        .query_row(
            "SELECT flexion_variants, has_base_file FROM ldraw_part WHERE design_id = '3001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(flexion, None, "an ordinary part has no flexion_variants");
    assert_eq!(has_base, 1, "an ordinary part has a base .dat");

    // The fixture's two `~Moved to` tombstones land in ldraw_moved_to
    // (and nowhere in ldraw_part), with the count stamped into meta.
    let hop = |table: &str, id: &str| -> Option<String> {
        conn.query_row(
            &format!("SELECT target_design_id FROM {table} WHERE design_id = ?1"),
            rusqlite::params![id],
            |r| r.get(0),
        )
        .ok()
    };
    assert_eq!(hop("ldraw_moved_to", "3023").as_deref(), Some("3023b"));
    assert_eq!(hop("ldraw_moved_to", "4073").as_deref(), Some("6141"));
    assert_eq!(
        meta.get("ldraw_moved_to_count").map(String::as_str),
        Some("2")
    );
    // The 30071 hard alias (of 3005) lands in ldraw_alias, not ldraw_part.
    assert_eq!(hop("ldraw_alias", "30071").as_deref(), Some("3005"));
    assert_eq!(meta.get("ldraw_alias_count").map(String::as_str), Some("1"));

    // Close the DB before removing the temp dir — on Windows an open SQLite
    // handle holds a file lock that would make remove_dir_all fail.
    drop(conn);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn build_stores_flexion_variants_blob() {
    let root = temp_root("flexion-blob");
    let (pin, csv_dir) = fake_pin_and_csv_dir(&root);
    let crossrefs = write_crossrefs(&root);
    let out = root.join("catalog.sqlite");

    // A throwaway LDraw library with one flexion part (base + f1 + f2),
    // so the build's flexion_variants BLOB can be asserted end-to-end.
    let ldraw = root.join("ldraw");
    let parts = ldraw.join("parts");
    std::fs::create_dir_all(&parts).unwrap();
    std::fs::write(
        parts.join("92693c01.dat"),
        b"0 Technic Linear Actuator Body Assembly\n0 !LDRAW_ORG Shortcut\n",
    )
    .unwrap();
    std::fs::write(
        parts.join("92693c01-f1.dat"),
        b"0 Technic Linear Actuator (Contracted)\n0 !LDRAW_ORG Shortcut\n",
    )
    .unwrap();
    std::fs::write(
        parts.join("92693c01-f2.dat"),
        b"0 Technic Linear Actuator (Extended)\n0 !LDRAW_ORG Shortcut\n",
    )
    .unwrap();

    build::run_with(&pin, &csv_dir, &crossrefs, &ldraw, &out).expect("build should succeed");

    let conn = Connection::open(&out).unwrap();
    // The three -fN files collapse to one row whose BLOB lists both positions.
    let flexion: Option<Vec<u8>> = conn
        .query_row(
            "SELECT flexion_variants FROM ldraw_part WHERE design_id = '92693c01'",
            [],
            |r| r.get(0),
        )
        .expect("canonical flexion part present");
    assert_eq!(
        unpack_u32_le(&flexion.expect("flexion part has variants"))
            .expect("flexion_variants BLOB decodes"),
        vec![1, 2],
    );
    // And the leaked variant ids are absent.
    let leaked: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ldraw_part WHERE design_id LIKE '92693c01-f%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leaked, 0, "flexion variant rows leaked into ldraw_part");

    // Close the DB before removing the temp dir — on Windows an open SQLite
    // handle holds a file lock that would make remove_dir_all fail.
    drop(conn);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn build_ingests_small_rebrickable_tables() {
    let root = temp_root("rb-tables");
    let (pin, csv_dir) = fake_pin_and_csv_dir(&root);
    let crossrefs = write_crossrefs(&root);
    let out = root.join("catalog.sqlite");

    build::run_with(&pin, &csv_dir, &crossrefs, &fixture_ldraw_dir(), &out)
        .expect("build should succeed");

    let conn = Connection::open(&out).unwrap();
    let count = |table: &str| {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
    };

    // Two synthetic rows each — except rb_parts (3, a 92693 row added for the
    // #73 FTS cases), rb_sets (3, a 2023 set added for the #72 cases) and
    // rb_inventories (3: four source rows, but 8880-1 v1 is pruned by the
    // latest-version dedup, leaving 100/102/103).
    for table in ["rb_part_categories", "rb_elements", "rb_themes"] {
        assert_eq!(count(table), 2, "{table} row count");
    }
    assert_eq!(count("rb_parts"), 5, "rb_parts row count");
    assert_eq!(count("rb_sets"), 3, "rb_sets row count");
    assert_eq!(
        count("rb_inventories"),
        3,
        "rb_inventories row count (8880-1 v1 pruned)"
    );

    // The row counts are stamped into meta for observability.
    let meta = read_meta(&out);
    for (key, expected) in [
        ("rb_parts_count", "5"),
        ("rb_part_categories_count", "2"),
        ("rb_elements_count", "2"),
        ("rb_themes_count", "2"),
        ("rb_sets_count", "3"),
        ("rb_inventories_count", "3"),
    ] {
        assert_eq!(meta.get(key).map(String::as_str), Some(expected), "{key}");
    }

    // A populated row round-trips, including a parsed integer column.
    let (name, cat, material): (String, i64, Option<String>) = conn
        .query_row(
            "SELECT name, category_id_rb, material FROM rb_parts WHERE part_id_rb = '3001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("rb_parts 3001 present");
    assert_eq!(name, "Brick 2 x 4");
    assert_eq!(cat, 11);
    assert_eq!(material.as_deref(), Some("Plastic"));

    // Empty fields become SQL NULL, not empty strings.
    let elem_design: Option<String> = conn
        .query_row(
            "SELECT design_id_rb FROM rb_elements WHERE element_id = '300126'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(elem_design, None, "empty elements.design_id -> NULL");

    let theme_parent: Option<i64> = conn
        .query_row(
            "SELECT parent_theme_id_rb FROM rb_themes WHERE theme_id_rb = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(theme_parent, None, "empty themes.parent_id -> NULL");

    let set_img: Option<String> = conn
        .query_row(
            "SELECT img_url FROM rb_sets WHERE set_num_rb = '1000-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(set_img, None, "empty sets.img_url -> NULL");

    // A non-empty nullable field is preserved.
    let elem_design2: Option<String> = conn
        .query_row(
            "SELECT design_id_rb FROM rb_elements WHERE element_id = '4159553'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(elem_design2.as_deref(), Some("3001"));

    // Close the DB before removing the temp dir — on Windows an open SQLite
    // handle holds a file lock that would make remove_dir_all fail.
    drop(conn);
    let _ = std::fs::remove_dir_all(&root);
}

/// #72: `inventory_parts` aggregates into `rb_part_color_set` — qty/spare
/// split, version dedup, part-num→design translation, year denorm, and the
/// skip counts.
#[test]
fn build_aggregates_inventory_fact_rows() {
    let root = temp_root("inv-facts");
    let (pin, csv_dir) = fake_pin_and_csv_dir(&root);
    let crossrefs = write_crossrefs(&root);
    let out = root.join("catalog.sqlite");

    build::run_with(&pin, &csv_dir, &crossrefs, &fixture_ldraw_dir(), &out)
        .expect("build should succeed");

    let conn = Connection::open(&out).unwrap();
    // (qty, qty_spare, year) for a (design, color, set) fact, or None if absent.
    let fact = |design: &str, color: u32, set_id: u32| -> Option<(i64, i64, Option<i64>)> {
        conn.query_row(
            "SELECT qty, qty_spare, year FROM rb_part_color_set
             WHERE design_id = ?1 AND color_id = ?2 AND set_id = ?3",
            rusqlite::params![design, color, set_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()
    };

    // 3001 in 1000-1 (set_id 1, 1985): main qty 2 + spare 1 on one fact row.
    assert_eq!(fact("3001", 4, 1), Some((2, 1, Some(1985))));
    // 3001 in 8880-1 (set_id 3, 1994): qty 5 from the surviving v2 — the v1
    // row (qty 99) was dropped by version dedup.
    assert_eq!(fact("3001", 4, 3), Some((5, 0, Some(1994))));
    // 92693 → collapsed design 92693c01, in 75000-1 (set_id 2, 2023).
    assert_eq!(fact("92693c01", 4, 2), Some((1, 0, Some(2023))));
    // 3023 in 75000-1 was spare-only (qty 0, spare 1); promoted to main
    // qty 1 — and its pin design is an LDraw tombstone, so the fact lands on
    // the renamed design 3023b (#112 tier 2).
    assert_eq!(fact("3023b", 4, 2), Some((1, 0, Some(2023))));
    assert_eq!(fact("3023", 4, 2), None, "nothing keyed on the retired id");
    // 4073 is unpinned and tombstoned: chased to 6141 via the literal path.
    assert_eq!(fact("6141", 4, 2), Some((2, 0, Some(2023))));
    // 3005 is not in the pin but is in the fixture library: mapped via the
    // literal fallback (#112) — and 30071, its hard alias, merges into the
    // SAME fact (3 + 4 = 7), so alias-recorded appearances are never split
    // across two ids.
    assert_eq!(fact("3005", 4, 1), Some((7, 0, Some(1985))));
    assert_eq!(fact("30071", 4, 1), None, "nothing keyed on the alias id");
    // Seven distinct (design,color,set) facts; unmapped part/color made none.
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM rb_part_color_set", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, 7, "seven distinct (design,color,set) facts");

    let meta = read_meta(&out);
    let m = |k: &str| meta.get(k).map(String::as_str);
    assert_eq!(m("rb_part_color_set_rows"), Some("7"));
    // Counters partition the 12 data rows: 9 aggregated + 1 no-set (8880-1 v1,
    // pruned) + 1 unmapped part + 1 unmapped color. Of the aggregated rows,
    // three (3005, 4073, 30071) mapped via the literal fallback and three
    // followed a redirect (3023 from the pin; 4073 and 30071 from the
    // literal path).
    assert_eq!(m("inventory_parts_rows_read"), Some("12"));
    assert_eq!(m("inventory_parts_rows_skipped_no_set"), Some("1"));
    assert_eq!(m("inventory_parts_rows_skipped_unmapped_part"), Some("1"));
    assert_eq!(m("inventory_parts_rows_skipped_unmapped_color"), Some("1"));
    assert_eq!(m("inventory_parts_rows_aggregated"), Some("9"));
    assert_eq!(m("inventory_parts_rows_mapped_literal"), Some("3"));
    assert_eq!(m("inventory_parts_rows_redirected"), Some("3"));
    assert_eq!(m("inventory_parts_rows_spare"), Some("2"));
    // Just the 3023 spare-only fact was promoted to main.
    assert_eq!(m("inventory_parts_facts_promoted"), Some("1"));

    drop(conn);
    let _ = std::fs::remove_dir_all(&root);
}

/// #72: `rb_part_summary` rolls up popularity from **non-spare** appearances
/// and spans years across **all** appearances (spare included).
#[test]
fn build_rolls_up_part_summary() {
    let root = temp_root("inv-summary");
    let (pin, csv_dir) = fake_pin_and_csv_dir(&root);
    let crossrefs = write_crossrefs(&root);
    let out = root.join("catalog.sqlite");

    build::run_with(&pin, &csv_dir, &crossrefs, &fixture_ldraw_dir(), &out)
        .expect("build should succeed");

    let conn = Connection::open(&out).unwrap();
    let summary = |design: &str| -> Option<(i64, i64, Option<i64>, Option<i64>)> {
        conn.query_row(
            "SELECT set_count, qty_sum, year_min, year_max FROM rb_part_summary
             WHERE design_id = ?1",
            rusqlite::params![design],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .ok()
    };

    // 3001: in 2 distinct sets; qty_sum 2+5=7 — its genuine spare (1 extra in
    // 1000-1, where it's also a main part) stays excluded; years span 1985–1994.
    assert_eq!(summary("3001"), Some((2, 7, Some(1985), Some(1994))));
    // 3023 was spare-only in a 2023 set; promoted to main, so it now counts —
    // under the renamed design 3023b, since the pin's 3023 is a tombstone.
    assert_eq!(summary("3023b"), Some((1, 1, Some(2023), Some(2023))));
    assert_eq!(summary("3023"), None, "nothing keyed on the retired id");
    // 3005 mapped via the literal fallback (#112), plus its alias 30071's
    // appearances: one 1985 set, qty 3 + 4.
    assert_eq!(summary("3005"), Some((1, 7, Some(1985), Some(1985))));
    // 4073's appearances roll up under its rename target 6141.
    assert_eq!(summary("6141"), Some((1, 2, Some(2023), Some(2023))));

    let meta = read_meta(&out);
    // Six parts appear in inventories: 3001, 3005, 3626, 3023b, 6141,
    // 92693c01.
    assert_eq!(
        meta.get("rb_part_summary_count").map(String::as_str),
        Some("6")
    );

    drop(conn);
    let _ = std::fs::remove_dir_all(&root);
}

/// #82: `part_relationships` ingests catalog-filtered (one-endpoint), with
/// design-id columns resolved only for endpoints in `ldraw_part`, exact
/// duplicates collapsed, and the skip/dedup counters stamped.
#[test]
fn build_ingests_catalog_filtered_part_relationships() {
    let root = temp_root("part-rels");
    let (pin, csv_dir) = fake_pin_and_csv_dir(&root);
    let crossrefs = write_crossrefs(&root);
    let out = root.join("catalog.sqlite");

    build::run_with(&pin, &csv_dir, &crossrefs, &fixture_ldraw_dir(), &out)
        .expect("build should succeed");

    let conn = Connection::open(&out).unwrap();
    // (child_design_id, parent_design_id) for a relationship row, or None if
    // the row itself is absent.
    let rel =
        |rel_type: &str, child: &str, parent: &str| -> Option<(Option<String>, Option<String>)> {
            conn.query_row(
                "SELECT child_design_id, parent_design_id FROM rb_part_relationships
             WHERE rel_type = ?1 AND child_part_num = ?2 AND parent_part_num = ?3",
                rusqlite::params![rel_type, child, parent],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
        };

    // Both endpoints in-catalog: both design ids resolve.
    assert_eq!(
        rel("A", "3024", "3001"),
        Some((Some("3024".into()), Some("3001".into())))
    );
    // A decorated child with no pin entry: kept via the parent, child NULL.
    assert_eq!(
        rel("P", "3001pr0001", "3001"),
        Some((None, Some("3001".into())))
    );
    // 3626 is pin-mapped but not in the fixture library: kept via the child
    // (one-endpoint), and the mapped-but-absent parent stays NULL.
    assert_eq!(rel("R", "3001", "3626"), Some((Some("3001".into()), None)));
    // The remaining kept types, via one in-catalog endpoint each.
    assert_eq!(
        rel("T", "3024pat0002", "3024"),
        Some((None, Some("3024".into())))
    );
    assert_eq!(rel("M", "3001", "3001a"), Some((Some("3001".into()), None)));
    assert_eq!(
        rel("B", "3024", "973c00"),
        Some((Some("3024".into()), None))
    );
    // 3005 is not in the pin: kept via the literal fallback (#112).
    assert_eq!(rel("R", "3005", "43723"), Some((Some("3005".into()), None)));
    // 4073 is an unpinned tombstone: its design id chases to 6141 (#112 t2).
    assert_eq!(
        rel("M", "4073", "6141"),
        Some((Some("6141".into()), Some("6141".into())))
    );
    // Neither endpoint in-catalog: filtered out.
    assert_eq!(rel("R", "43722", "43723"), None);

    // Eight rows survive: ten read, one out-of-catalog, one duplicate.
    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM rb_part_relationships", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(total, 8);

    let meta = read_meta(&out);
    let m = |k: &str| meta.get(k).map(String::as_str);
    assert_eq!(m("rb_part_relationships_count"), Some("8"));
    assert_eq!(m("part_relationships_rows_read"), Some("10"));
    assert_eq!(
        m("part_relationships_rows_skipped_out_of_catalog"),
        Some("1")
    );
    assert_eq!(m("part_relationships_rows_deduped"), Some("1"));

    // The raw-part-num lookup indexes exist (the design-id ones come from the
    // TableSpec and are covered by its unit tests).
    for idx in [
        "idx_rb_part_relationships_child",
        "idx_rb_part_relationships_parent",
    ] {
        let found: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                rusqlite::params![idx],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "missing index {idx}");
    }

    drop(conn);
    let _ = std::fs::remove_dir_all(&root);
}

/// #73: the `part` view joins summary columns where Rebrickable knows the
/// part and leaves them NULL where it doesn't; the FTS index answers LDraw-
/// name and part-number searches; and the final `meta` stamps land with
/// `build_status = 'complete'` last.
#[test]
fn build_finalizes_part_view_fts_and_meta() {
    let root = temp_root("finalize");
    let (pin, csv_dir) = fake_pin_and_csv_dir(&root);
    let crossrefs = write_crossrefs(&root);
    let out = root.join("catalog.sqlite");

    build::run_with(&pin, &csv_dir, &crossrefs, &fixture_ldraw_dir(), &out)
        .expect("build should succeed");

    let conn = Connection::open(&out).unwrap();

    // A part with Rebrickable appearances: summary columns populated.
    let (name, set_count, qty_sum, year_min, year_max): (String, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT ldraw_name, set_count, qty_sum, year_min, year_max
             FROM part WHERE design_id = '3001'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("part 3001 present in the view");
    assert!(name.starts_with("Brick"), "ldraw_name was {name:?}");
    assert_eq!((set_count, qty_sum), (2, 7));
    assert_eq!((year_min, year_max), (1985, 1994));

    // An LDraw-only part (no inventory appearance): NULL summary — a valid
    // state, not an error.
    let lonely: (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT set_count, qty_sum FROM part WHERE design_id = '15070'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("LDraw-only part 15070 still appears in the view");
    assert_eq!(lonely, (None, None));

    // The view covers exactly the LDraw catalog.
    let view_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM part", [], |r| r.get(0))
        .unwrap();
    let ldraw_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM ldraw_part", [], |r| r.get(0))
        .unwrap();
    assert_eq!(view_rows, ldraw_rows);

    // FTS: an LDraw-name token and a part-number search both hit.
    let fts_hit = |query: &str| -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT design_id FROM part_fts WHERE part_fts MATCH ?1 ORDER BY design_id")
            .unwrap();
        stmt.query_map(rusqlite::params![query], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert!(
        fts_hit("brick").contains(&"3001".to_string()),
        "LDraw-name token finds 3001"
    );
    // 3005's Rebrickable name joins via the literal fallback (#112): a token
    // present only in its rb_name still finds it.
    assert_eq!(fts_hit("classic"), vec!["3005".to_string()]);
    // 4073's Rebrickable name lands on its rename target 6141 (#112 tier 2).
    assert_eq!(fts_hit("legacy"), vec!["6141".to_string()]);
    // alt_ids makes retired and alias part numbers searchable: the old id
    // finds the renamed design, the alias id finds the alias target.
    assert_eq!(fts_hit("4073"), vec!["6141".to_string()]);
    assert_eq!(fts_hit("3023"), vec!["3023b".to_string()]);
    assert_eq!(fts_hit("30071"), vec!["3005".to_string()]);
    assert_eq!(
        fts_hit("3040b"),
        vec!["3040b".to_string()],
        "part-number search"
    );

    // Meta: the finalize counts, the pin date, and the completion status.
    let meta = read_meta(&out);
    let m = |k: &str| meta.get(k).map(String::as_str);
    assert_eq!(
        m("total_part_view_rows"),
        Some(view_rows.to_string().as_str())
    );
    assert_eq!(m("fts_row_count"), Some(view_rows.to_string().as_str()));
    assert_eq!(m("part_mappings_date"), Some("2026-05-27"));
    assert_eq!(m("build_status"), Some("complete"));

    drop(conn);
    let _ = std::fs::remove_dir_all(&root);
}

/// #73: the FTS `rb_name` column makes a part findable by a token that only
/// its Rebrickable (appearance) name carries — and the collapsed actuator is
/// findable by its full part number.
#[test]
fn build_fts_indexes_rebrickable_names() {
    let root = temp_root("fts-rb-name");
    let (pin, csv_dir) = fake_pin_and_csv_dir(&root);
    let crossrefs = write_crossrefs(&root);
    let out = root.join("catalog.sqlite");

    // A throwaway library whose one part (the collapsed actuator) has an
    // LDraw name with no "Long" token; the pin maps rb 92693 → 92693c01, and
    // rb_parts names it "Technic Linear Actuator Long".
    let ldraw = root.join("ldraw");
    let parts = ldraw.join("parts");
    std::fs::create_dir_all(&parts).unwrap();
    std::fs::write(
        parts.join("92693c01.dat"),
        b"0 Technic Linear Actuator Body Assembly\n0 !LDRAW_ORG Shortcut\n",
    )
    .unwrap();

    build::run_with(&pin, &csv_dir, &crossrefs, &ldraw, &out).expect("build should succeed");

    let conn = Connection::open(&out).unwrap();
    let fts_one = |query: &str| -> Option<String> {
        conn.query_row(
            "SELECT design_id FROM part_fts WHERE part_fts MATCH ?1",
            rusqlite::params![query],
            |r| r.get(0),
        )
        .ok()
    };
    // Found by the rb_name-only token…
    assert_eq!(fts_one("long"), Some("92693c01".into()));
    // …and by the collapsed part number (the `unicode61 tokenchars '-'`
    // tokenizer keeps it one term).
    assert_eq!(fts_one("92693c01"), Some("92693c01".into()));
    // The LDraw name still hits too.
    assert_eq!(fts_one("assembly"), Some("92693c01".into()));

    drop(conn);
    let _ = std::fs::remove_dir_all(&root);
}

/// #73 / the M2 gate: two builds from the same pinned inputs produce
/// byte-identical files.
#[test]
fn build_is_deterministic() {
    let root = temp_root("determinism");
    let (pin, csv_dir) = fake_pin_and_csv_dir(&root);
    let crossrefs = write_crossrefs(&root);

    let build_to = |name: &str| -> String {
        let out = root.join(name);
        build::run_with(&pin, &csv_dir, &crossrefs, &fixture_ldraw_dir(), &out)
            .expect("build should succeed");
        util::hash_bytes(&std::fs::read(&out).unwrap())
    };

    let first = build_to("catalog-a.sqlite");
    let second = build_to("catalog-b.sqlite");
    assert_eq!(
        first, second,
        "two builds from the same pins must be byte-identical"
    );

    let _ = std::fs::remove_dir_all(&root);
}
