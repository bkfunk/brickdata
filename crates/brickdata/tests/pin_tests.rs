use brickdata::pin::{CatalogPin, LdrawPin, Pin, PinError, RebrickablePin};

// Trimmed copies of real emitted pins — the shapes under test are exactly
// what the justfile recipes write.
const REBRICKABLE_PIN: &str = r#"// brickdata Rebrickable mirror pin. Blockstar consumers copy to:
//   external-data/rebrickable/csv-snapshot.ron
(
  mirror_tag: "rebrickable-2026-06-01",
  snapshot_date: "2026-06-01",
  file_fingerprints: {
    "parts.csv.gz": (sha256: "8998c8ee3bd5433a88e5ead40ff8d7822c199a49d9fd15e44b3201feea67cc44", bytes: 1033939, mirror_url: "https://github.com/bkfunk/brickdata/releases/download/rebrickable-2026-06-01/parts.csv.gz"),
    "themes.csv.gz": (sha256: "5ed2ad73e58107496dd707d7b596872833df99661b7a613a8003ef75b804da50", bytes: 4490, mirror_url: "https://github.com/bkfunk/brickdata/releases/download/rebrickable-2026-06-01/themes.csv.gz"),
  },
)
"#;

const LDRAW_PIN: &str = r#"// brickdata LDraw mirror pin.
(
  mirror_tag: "ldraw-2026-06-01",
  snapshot_date: "2026-06-01",
  asset_url: "https://github.com/bkfunk/brickdata/releases/download/ldraw-2026-06-01/ldraw-merged.zip",
  asset_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  manifest_url: "https://github.com/bkfunk/brickdata/releases/download/ldraw-2026-06-01/manifest.tsv",
  manifest_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  file_count: 26127,
)
"#;

const CATALOG_PIN: &str = r#"// brickdata built-catalog pin.
(
  mirror_tag: "catalog-2026-07-07",
  asset_url: "https://github.com/bkfunk/brickdata/releases/download/catalog-2026-07-07/catalog.sqlite",
  sha256: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
  bytes: 12345678,
)
"#;

#[test]
fn parses_rebrickable_pin() {
    let pin = RebrickablePin::from_ron_str(REBRICKABLE_PIN).unwrap();
    assert_eq!(pin.mirror_tag, "rebrickable-2026-06-01");
    assert_eq!(pin.snapshot_date, "2026-06-01");
    assert_eq!(pin.file_fingerprints.len(), 2);
    let parts = &pin.file_fingerprints["parts.csv.gz"];
    assert_eq!(parts.bytes, 1033939);
    assert_eq!(
        parts.sha256,
        "8998c8ee3bd5433a88e5ead40ff8d7822c199a49d9fd15e44b3201feea67cc44"
    );
    assert!(parts.mirror_url.ends_with("parts.csv.gz"));
}

#[test]
fn parses_ldraw_pin() {
    let pin = LdrawPin::from_ron_str(LDRAW_PIN).unwrap();
    assert_eq!(pin.mirror_tag, "ldraw-2026-06-01");
    assert_eq!(pin.file_count, 26127);
    assert!(pin.manifest_url.ends_with("manifest.tsv"));
}

#[test]
fn parses_catalog_pin() {
    let pin = CatalogPin::from_ron_str(CATALOG_PIN).unwrap();
    assert_eq!(pin.mirror_tag, "catalog-2026-07-07");
    assert_eq!(pin.bytes, 12345678);
}

#[test]
fn shape_detection_matches_each_kind() {
    assert!(matches!(
        Pin::from_ron_str(REBRICKABLE_PIN).unwrap(),
        Pin::Rebrickable(_)
    ));
    assert!(matches!(
        Pin::from_ron_str(LDRAW_PIN).unwrap(),
        Pin::Ldraw(_)
    ));
    assert!(matches!(
        Pin::from_ron_str(CATALOG_PIN).unwrap(),
        Pin::Catalog(_)
    ));
    assert_eq!(
        Pin::from_ron_str(LDRAW_PIN).unwrap().mirror_tag(),
        "ldraw-2026-06-01"
    );
}

#[test]
fn malformed_pin_is_a_parse_error_not_a_panic() {
    let err = RebrickablePin::from_ron_str("( mirror_tag: 42 )").unwrap_err();
    assert!(matches!(err, PinError::Parse { .. }));
    // Truncated / empty / wrong-shape inputs all error cleanly too.
    assert!(Pin::from_ron_str("").is_err());
    assert!(LdrawPin::from_ron_str(CATALOG_PIN).is_err());
}

#[test]
fn missing_pin_file_is_an_io_error() {
    let err = Pin::from_path("/nonexistent/definitely-missing.ron").unwrap_err();
    assert!(matches!(err, PinError::Io { .. }));
}

/// Every real pin in the repo's `pins/` directory must parse — the pins are
/// the product, and this crate must never fall behind their format.
#[test]
fn all_repo_pins_parse() {
    let pins_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pins");
    if !pins_dir.is_dir() {
        // Running from a published crate archive; the repo pins aren't there.
        return;
    }
    let mut parsed = 0;
    for entry in std::fs::read_dir(&pins_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "ron") {
            Pin::from_path(&path)
                .unwrap_or_else(|e| panic!("repo pin {} failed to parse: {e}", path.display()));
            parsed += 1;
        }
    }
    assert!(parsed >= 3, "expected at least 3 repo pins, found {parsed}");
}
