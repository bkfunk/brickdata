use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::Write;
use std::rc::Rc;

use brickdata::fetch::{FetchError, Fetcher, Transport, TransportError};
use brickdata::pin::RebrickablePin;
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
