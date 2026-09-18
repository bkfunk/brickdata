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

// ── Catalog sidecars ────────────────────────────────────────────────────

/// Blockstar's checked-in `external-data/catalog/catalog-snapshot.ron`,
/// verbatim: a catalog pin cut before sidecars existed, so it has no
/// `sidecars` field at all. It must keep parsing unchanged.
const PRE_SIDECAR_CATALOG_PIN: &str = r#"// brickdata built-catalog pin.
(
  mirror_tag: "catalog-2026-07-19a",
  asset_url: "https://github.com/bkfunk/brickdata/releases/download/catalog-2026-07-19a/catalog.sqlite",
  sha256: "79d2356827710537101d6c48d335d205663a4ab18e26ae6a25306eec13461c30",
  bytes: 88342528,
)
"#;

/// Exactly what `just publish-catalog` writes (captured from a run of the
/// recipe with the release/upload helpers stubbed out): the catalog triple
/// plus a `sidecars` map keyed by asset filename, each entry the same
/// `(sha256, bytes, mirror_url)` tuple the Rebrickable pins use.
const CATALOG_PIN_WITH_SIDECARS: &str = r#"// brickdata built-catalog pin.
(
  mirror_tag: "catalog-2026-09-16",
  asset_url: "https://github.com/bkfunk/brickdata/releases/download/catalog-2026-09-16/catalog.sqlite",
  sha256: "5f4153cb90540859b7da97328c900e2d36002e61890a731c4ad01b624a9a5a9a",
  bytes: 20,
  sidecars: {
    "part_frequency.ron": (sha256: "60aa5a84772a8955c03633987a97e82d37283495156ea2d2225ba64460ec2d3a", bytes: 25, mirror_url: "https://github.com/bkfunk/brickdata/releases/download/catalog-2026-09-16/part_frequency.ron"),
    "color_names.ron": (sha256: "2f554eda7b99c60fe75f3689c13d1085b4dfdf906ba4b1f5960dfba0698270ac", bytes: 29, mirror_url: "https://github.com/bkfunk/brickdata/releases/download/catalog-2026-09-16/color_names.ron"),
  },
)
"#;

#[test]
fn catalog_pin_without_sidecars_field_parses_with_an_empty_map() {
    let pin = CatalogPin::from_ron_str(PRE_SIDECAR_CATALOG_PIN).unwrap();
    assert_eq!(pin.mirror_tag, "catalog-2026-07-19a");
    assert_eq!(pin.bytes, 88342528);
    assert!(pin.sidecars.is_empty());
    assert!(pin.sidecar(CatalogPin::PART_FREQUENCY_SIDECAR).is_none());
    assert!(pin.sidecar(CatalogPin::COLOR_NAMES_SIDECAR).is_none());
}

#[test]
fn catalog_pin_with_sidecars_parses_the_shape_publish_catalog_writes() {
    let pin = CatalogPin::from_ron_str(CATALOG_PIN_WITH_SIDECARS).unwrap();
    assert_eq!(pin.mirror_tag, "catalog-2026-09-16");
    assert_eq!(
        pin.sha256,
        "5f4153cb90540859b7da97328c900e2d36002e61890a731c4ad01b624a9a5a9a"
    );
    assert_eq!(pin.bytes, 20);
    assert_eq!(pin.sidecars.len(), 2);

    let freq = pin
        .sidecar(CatalogPin::PART_FREQUENCY_SIDECAR)
        .expect("part_frequency.ron pinned");
    assert_eq!(
        freq.sha256,
        "60aa5a84772a8955c03633987a97e82d37283495156ea2d2225ba64460ec2d3a"
    );
    assert_eq!(freq.bytes, 25);
    assert_eq!(
        freq.mirror_url,
        "https://github.com/bkfunk/brickdata/releases/download/catalog-2026-09-16/part_frequency.ron"
    );

    let colors = pin
        .sidecar(CatalogPin::COLOR_NAMES_SIDECAR)
        .expect("color_names.ron pinned");
    assert_eq!(colors.bytes, 29);
    assert!(
        colors
            .mirror_url
            .ends_with("/catalog-2026-09-16/color_names.ron")
    );

    // The sidecars are keyed by the well-known filenames the build writes.
    assert_eq!(
        pin.sidecars.keys().collect::<Vec<_>>(),
        ["color_names.ron", "part_frequency.ron"]
    );
}

#[test]
fn catalog_pin_round_trips_through_ron_with_and_without_sidecars() {
    for text in [PRE_SIDECAR_CATALOG_PIN, CATALOG_PIN_WITH_SIDECARS] {
        let pin = CatalogPin::from_ron_str(text).unwrap();
        // Serializing always emits the map (empty or not); that is acceptable
        // — the shell recipe is the pin *writer*, this crate only reads — and
        // the emitted text must parse back to the same pin.
        let serialized = ron::to_string(&pin).unwrap();
        assert!(serialized.contains("sidecars"), "{serialized}");
        let back = CatalogPin::from_ron_str(&serialized).unwrap();
        assert_eq!(back, pin);
    }
}

#[test]
fn shape_detection_routes_a_catalog_pin_with_sidecars_to_catalog() {
    // The sidecar tuples look like a Rebrickable pin's per-file entries, but
    // the structural dispatch keys on `file_fingerprints` / `manifest_sha256`
    // presence, neither of which a catalog pin has.
    match Pin::from_ron_str(CATALOG_PIN_WITH_SIDECARS).unwrap() {
        Pin::Catalog(pin) => assert_eq!(pin.sidecars.len(), 2),
        other => panic!("expected Pin::Catalog, got {other:?}"),
    }
    assert!(matches!(
        Pin::from_ron_str(PRE_SIDECAR_CATALOG_PIN).unwrap(),
        Pin::Catalog(_)
    ));
}
