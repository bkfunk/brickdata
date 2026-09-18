use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::Write;
use std::rc::Rc;

use brickdata::fetch::{FetchError, Fetcher, Transport, TransportError};
use brickdata::pin::{CatalogPin, RebrickablePin};
use sha2::{Digest, Sha256};

/// In-memory transport: url -> bytes, counting calls so tests can prove
/// cache hits skip the network.
struct MapTransport {
    responses: BTreeMap<String, Vec<u8>>,
    calls: Rc<Cell<u32>>,
}

impl MapTransport {
    fn new(responses: impl IntoIterator<Item = (&'static str, &'static [u8])>) -> Self {
        Self {
            responses: responses
                .into_iter()
                .map(|(url, body)| (url.to_string(), body.to_vec()))
                .collect(),
            calls: Rc::new(Cell::new(0)),
        }
    }

    /// Handle to the download counter, usable after the transport moves
    /// into a `Fetcher`.
    fn call_counter(&self) -> Rc<Cell<u32>> {
        Rc::clone(&self.calls)
    }
}

impl Transport for MapTransport {
    fn get(&self, url: &str, sink: &mut dyn Write) -> Result<u64, TransportError> {
        self.calls.set(self.calls.get() + 1);
        let body = self.responses.get(url).ok_or_else(|| TransportError {
            url: url.to_string(),
            reason: "404".to_string(),
        })?;
        sink.write_all(body).unwrap();
        Ok(body.len() as u64)
    }
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

const BODY: &[u8] = b"hello, pinned bytes";

#[test]
fn fetch_verifies_and_caches() {
    let cache = tempfile::tempdir().unwrap();
    let fetcher = Fetcher::with_transport(cache.path(), MapTransport::new([("u://a", BODY)]));
    let hash = sha256_hex(BODY);

    let path = fetcher
        .fetch_verified("u://a", &hash, Some(BODY.len() as u64))
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), BODY);
    assert!(path.starts_with(cache.path()));
}

#[test]
fn verified_cache_hit_skips_network() {
    let cache = tempfile::tempdir().unwrap();
    let transport = MapTransport::new([("u://a", BODY)]);
    let calls = transport.call_counter();
    let fetcher = Fetcher::with_transport(cache.path(), transport);
    let hash = sha256_hex(BODY);

    let first = fetcher.fetch_verified("u://a", &hash, None).unwrap();
    let second = fetcher.fetch_verified("u://a", &hash, None).unwrap();
    assert_eq!(first, second);
    assert_eq!(calls.get(), 1, "second fetch must be served from cache");
}

#[test]
fn hash_mismatch_is_a_hard_error_and_caches_nothing() {
    let cache = tempfile::tempdir().unwrap();
    let fetcher = Fetcher::with_transport(cache.path(), MapTransport::new([("u://a", BODY)]));
    let wrong = sha256_hex(b"other bytes");

    let err = fetcher.fetch_verified("u://a", &wrong, None).unwrap_err();
    assert!(matches!(err, FetchError::HashMismatch { .. }));
    // Nothing became visible under the cache key.
    assert!(!cache.path().join(&wrong).exists());
}

#[test]
fn size_mismatch_is_a_hard_error() {
    let cache = tempfile::tempdir().unwrap();
    let fetcher = Fetcher::with_transport(cache.path(), MapTransport::new([("u://a", BODY)]));
    let hash = sha256_hex(BODY);

    let err = fetcher
        .fetch_verified("u://a", &hash, Some(BODY.len() as u64 + 1))
        .unwrap_err();
    assert!(matches!(err, FetchError::SizeMismatch { expected, got, .. }
        if expected == BODY.len() as u64 + 1 && got == BODY.len() as u64));
}

#[test]
fn corrupted_cache_entry_is_refetched() {
    let cache = tempfile::tempdir().unwrap();
    let transport = MapTransport::new([("u://a", BODY)]);
    let calls = transport.call_counter();
    let fetcher = Fetcher::with_transport(cache.path(), transport);
    let hash = sha256_hex(BODY);

    let path = fetcher.fetch_verified("u://a", &hash, None).unwrap();
    std::fs::write(&path, b"tampered").unwrap();
    let path2 = fetcher.fetch_verified("u://a", &hash, None).unwrap();
    assert_eq!(path, path2);
    assert_eq!(std::fs::read(&path2).unwrap(), BODY);
    assert_eq!(calls.get(), 2, "tampered entry must trigger a re-download");
}

#[test]
fn malformed_expected_hash_is_rejected() {
    let cache = tempfile::tempdir().unwrap();
    let fetcher = Fetcher::with_transport(cache.path(), MapTransport::new([("u://a", BODY)]));
    for bad in ["deadbeef", "zz", &"a".repeat(63), &"g".repeat(64)] {
        let err = fetcher.fetch_verified("u://a", bad, None).unwrap_err();
        assert!(
            matches!(err, FetchError::BadExpectedHash(_)),
            "hash {bad:?}"
        );
    }
}

#[test]
fn transport_failure_propagates() {
    let cache = tempfile::tempdir().unwrap();
    let fetcher = Fetcher::with_transport(cache.path(), MapTransport::new([]));
    let err = fetcher
        .fetch_verified("u://missing", &sha256_hex(BODY), None)
        .unwrap_err();
    assert!(matches!(err, FetchError::Transport(_)));
}

#[test]
fn fetch_rebrickable_returns_all_tables() {
    let parts = b"parts,bytes".as_slice();
    let themes = b"themes,bytes".as_slice();
    let pin_text = format!(
        r#"(
  mirror_tag: "rebrickable-2026-06-01",
  snapshot_date: "2026-06-01",
  file_fingerprints: {{
    "parts.csv.gz": (sha256: "{}", bytes: {}, mirror_url: "u://parts"),
    "themes.csv.gz": (sha256: "{}", bytes: {}, mirror_url: "u://themes"),
  }},
)"#,
        sha256_hex(parts),
        parts.len(),
        sha256_hex(themes),
        themes.len(),
    );
    let pin = RebrickablePin::from_ron_str(&pin_text).unwrap();

    let cache = tempfile::tempdir().unwrap();
    let fetcher = Fetcher::with_transport(
        cache.path(),
        MapTransport::new([("u://parts", parts), ("u://themes", themes)]),
    );
    let tables = fetcher.fetch_rebrickable(&pin).unwrap();
    assert_eq!(tables.len(), 2);
    assert_eq!(std::fs::read(&tables["parts.csv.gz"]).unwrap(), parts);
    assert_eq!(std::fs::read(&tables["themes.csv.gz"]).unwrap(), themes);
}

// ── Catalog sidecars ────────────────────────────────────────────────────

const SQLITE: &[u8] = b"pretend sqlite bytes";
const FREQ: &[u8] = b"PartFrequency(parts: {})";
const COLORS: &[u8] = b"#![enable(implicit_some)]\n[]\n";

/// A catalog pin in exactly the shape `just publish-catalog` writes, with
/// two sidecars whose fingerprints match the in-memory transport's bodies.
fn catalog_pin_with_sidecars() -> CatalogPin {
    let text = format!(
        r#"// brickdata built-catalog pin.
(
  mirror_tag: "catalog-2026-09-16",
  asset_url: "u://catalog.sqlite",
  sha256: "{}",
  bytes: {},
  sidecars: {{
    "part_frequency.ron": (sha256: "{}", bytes: {}, mirror_url: "u://part_frequency.ron"),
    "color_names.ron": (sha256: "{}", bytes: {}, mirror_url: "u://color_names.ron"),
  }},
)
"#,
        sha256_hex(SQLITE),
        SQLITE.len(),
        sha256_hex(FREQ),
        FREQ.len(),
        sha256_hex(COLORS),
        COLORS.len(),
    );
    CatalogPin::from_ron_str(&text).unwrap()
}

fn catalog_transport() -> MapTransport {
    MapTransport::new([
        ("u://catalog.sqlite", SQLITE),
        ("u://part_frequency.ron", FREQ),
        ("u://color_names.ron", COLORS),
    ])
}

#[test]
fn fetch_catalog_sidecar_verifies_and_caches_like_the_catalog() {
    let cache = tempfile::tempdir().unwrap();
    let transport = catalog_transport();
    let calls = transport.call_counter();
    let fetcher = Fetcher::with_transport(cache.path(), transport);
    let pin = catalog_pin_with_sidecars();

    let path = fetcher
        .fetch_catalog_sidecar(&pin, CatalogPin::COLOR_NAMES_SIDECAR)
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), COLORS);
    // Content-addressed: the cache key is the sidecar's own hash.
    assert_eq!(path, cache.path().join(sha256_hex(COLORS)));

    // A second fetch is a verified cache hit — no network.
    let again = fetcher
        .fetch_catalog_sidecar(&pin, CatalogPin::COLOR_NAMES_SIDECAR)
        .unwrap();
    assert_eq!(again, path);
    assert_eq!(
        calls.get(),
        1,
        "second sidecar fetch must be served from cache"
    );

    // The catalog itself is untouched by sidecar fetches.
    assert_eq!(
        std::fs::read(fetcher.fetch_catalog(&pin).unwrap()).unwrap(),
        SQLITE
    );
}

#[test]
fn fetch_catalog_sidecar_unknown_name_is_an_error_not_a_panic() {
    let cache = tempfile::tempdir().unwrap();
    let transport = catalog_transport();
    let calls = transport.call_counter();
    let fetcher = Fetcher::with_transport(cache.path(), transport);
    let pin = catalog_pin_with_sidecars();

    let err = fetcher
        .fetch_catalog_sidecar(&pin, "geometry_cache.rkyv")
        .unwrap_err();
    assert!(
        matches!(&err, FetchError::NoSuchSidecar { mirror_tag, name }
            if mirror_tag == "catalog-2026-09-16" && name == "geometry_cache.rkyv"),
        "{err:?}"
    );
    assert!(err.to_string().contains("geometry_cache.rkyv"), "{err}");
    assert_eq!(
        calls.get(),
        0,
        "an unknown sidecar must not touch the network"
    );
}

#[test]
fn fetch_catalog_sidecars_returns_every_sidecar_keyed_by_name() {
    let cache = tempfile::tempdir().unwrap();
    let fetcher = Fetcher::with_transport(cache.path(), catalog_transport());
    let pin = catalog_pin_with_sidecars();

    let sidecars = fetcher.fetch_catalog_sidecars(&pin).unwrap();
    assert_eq!(
        sidecars.keys().collect::<Vec<_>>(),
        ["color_names.ron", "part_frequency.ron"]
    );
    assert_eq!(
        std::fs::read(&sidecars["part_frequency.ron"]).unwrap(),
        FREQ
    );
    assert_eq!(std::fs::read(&sidecars["color_names.ron"]).unwrap(), COLORS);
}

#[test]
fn fetch_catalog_sidecars_of_a_pre_sidecar_pin_is_empty_not_an_error() {
    // The shape Blockstar's checked-in catalog-snapshot.ron has today.
    let pin = CatalogPin::from_ron_str(
        r#"(
  mirror_tag: "catalog-2026-07-19a",
  asset_url: "u://catalog.sqlite",
  sha256: "79d2356827710537101d6c48d335d205663a4ab18e26ae6a25306eec13461c30",
  bytes: 88342528,
)"#,
    )
    .unwrap();
    let cache = tempfile::tempdir().unwrap();
    let transport = catalog_transport();
    let calls = transport.call_counter();
    let fetcher = Fetcher::with_transport(cache.path(), transport);

    assert!(fetcher.fetch_catalog_sidecars(&pin).unwrap().is_empty());
    assert_eq!(calls.get(), 0);
    assert!(matches!(
        fetcher
            .fetch_catalog_sidecar(&pin, CatalogPin::PART_FREQUENCY_SIDECAR)
            .unwrap_err(),
        FetchError::NoSuchSidecar { .. }
    ));
}

#[test]
fn sidecar_hash_and_size_mismatches_are_hard_errors() {
    let cache = tempfile::tempdir().unwrap();
    // color_names.ron is served tampered (exercises the hash gate);
    // part_frequency.ron is served correctly and its *pinned* size is then
    // bumped below (exercises the size gate on a sidecar).
    let mut pin = catalog_pin_with_sidecars();
    let fetcher = Fetcher::with_transport(
        cache.path(),
        MapTransport::new([
            (
                "u://part_frequency.ron",
                b"PartFrequency(parts: {})".as_slice(),
            ),
            (
                "u://color_names.ron",
                b"tampered sidecar bytes!!!!!!!".as_slice(),
            ),
        ]),
    );

    let err = fetcher
        .fetch_catalog_sidecar(&pin, CatalogPin::COLOR_NAMES_SIDECAR)
        .unwrap_err();
    assert!(matches!(err, FetchError::HashMismatch { .. }), "{err:?}");
    assert!(!cache.path().join(sha256_hex(COLORS)).exists());

    // Right bytes, wrong pinned size: the size gate rejects it too.
    pin.sidecars
        .get_mut(CatalogPin::PART_FREQUENCY_SIDECAR)
        .unwrap()
        .bytes += 1;
    let err = fetcher
        .fetch_catalog_sidecar(&pin, CatalogPin::PART_FREQUENCY_SIDECAR)
        .unwrap_err();
    assert!(matches!(err, FetchError::SizeMismatch { .. }), "{err:?}");
    assert!(fetcher.fetch_catalog_sidecars(&pin).is_err());
}
