//! Verified fetching with a local content-addressed cache.
//!
//! [`Fetcher`] downloads release assets through a [`Transport`] and verifies
//! sha256 (and, when the pin records it, exact byte size) *before* an asset
//! ever becomes visible in the cache. The cache is keyed by expected hash, so
//! a hit is re-hashed locally and served without touching the network; a
//! corrupted cache entry is discarded and re-fetched.
//!
//! Verification failures are hard errors — there is no "keep the bytes
//! anyway" path.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::pin::{CatalogPin, LdrawPin, RebrickablePin};

/// Error from a [`Transport`] implementation.
#[derive(Debug, thiserror::Error)]
#[error("transport error fetching {url}: {reason}")]
pub struct TransportError {
    /// The URL whose fetch failed.
    pub url: String,
    /// Human-readable failure description.
    pub reason: String,
}

/// Byte source for [`Fetcher`]. Implementations stream the body of `url`
/// into `sink` and return the byte count written.
///
/// The built-in [`HttpTransport`] (feature `http`, on by default) covers the
/// normal case; tests and offline consumers can supply their own.
pub trait Transport {
    /// Stream the body of `url` into `sink`, returning the bytes written.
    fn get(&self, url: &str, sink: &mut dyn Write) -> Result<u64, TransportError>;
}

/// HTTP(S) transport backed by [`ureq`].
#[cfg(feature = "http")]
#[derive(Debug)]
pub struct HttpTransport {
    agent: ureq::Agent,
}

#[cfg(feature = "http")]
impl Default for HttpTransport {
    fn default() -> Self {
        Self {
            agent: ureq::agent(),
        }
    }
}

#[cfg(feature = "http")]
impl Transport for HttpTransport {
    fn get(&self, url: &str, sink: &mut dyn Write) -> Result<u64, TransportError> {
        let err = |reason: String| TransportError {
            url: url.to_string(),
            reason,
        };
        let response = self.agent.get(url).call().map_err(|e| err(e.to_string()))?;
        let mut reader = response.into_reader();
        io::copy(&mut reader, sink).map_err(|e| err(format!("read body: {e}")))
    }
}

/// Error fetching or verifying an asset.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The transport failed to download the asset.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The downloaded bytes hash differently than the pin records.
    #[error("sha256 mismatch for {url}: expected {expected}, got {got}")]
    HashMismatch {
        /// The URL that was fetched.
        url: String,
        /// The sha256 the pin records.
        expected: String,
        /// The sha256 of the bytes actually received.
        got: String,
    },
    /// The downloaded byte count differs from what the pin records.
    #[error("size mismatch for {url}: expected {expected} bytes, got {got}")]
    SizeMismatch {
        /// The URL that was fetched.
        url: String,
        /// The byte count the pin records.
        expected: u64,
        /// The byte count actually received.
        got: u64,
    },
    /// The caller-supplied expected hash is not a 64-char hex string.
    #[error("expected sha256 is not lowercase hex: {0:?}")]
    BadExpectedHash(String),
    /// Reading or writing the cache failed.
    #[error("cache I/O error at {path}: {source}")]
    Io {
        /// The cache path involved.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// Downloads assets into a content-addressed cache directory, verifying
/// hashes/sizes against pin fingerprints.
pub struct Fetcher<T> {
    cache_dir: PathBuf,
    transport: T,
}

#[cfg(feature = "http")]
impl Fetcher<HttpTransport> {
    /// Fetcher with the built-in HTTP transport. `cache_dir` is created on
    /// first use.
    pub fn new(cache_dir: impl Into<PathBuf>) -> Self {
        Self::with_transport(cache_dir, HttpTransport::default())
    }
}

impl<T: Transport> Fetcher<T> {
    /// Fetcher with a caller-supplied transport.
    pub fn with_transport(cache_dir: impl Into<PathBuf>, transport: T) -> Self {
        Self {
            cache_dir: cache_dir.into(),
            transport,
        }
    }

    /// The directory verified assets land in.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Fetch `url`, verifying its sha256 (and byte size, when known), and
    /// return the cached file path. A verified cache hit does no network I/O.
    ///
    /// The cache key is the expected hash, so distinct assets never collide
    /// and a renamed release asset still hits.
    pub fn fetch_verified(
        &self,
        url: &str,
        sha256_hex: &str,
        bytes: Option<u64>,
    ) -> Result<PathBuf, FetchError> {
        if sha256_hex.len() != 64 || !sha256_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(FetchError::BadExpectedHash(sha256_hex.to_string()));
        }
        let expected = sha256_hex.to_ascii_lowercase();
        let cached = self.cache_dir.join(&expected);

        if let Some(hit) = self.verified_hit(&cached, &expected)? {
            return Ok(hit);
        }

        let io_err = |path: &Path| {
            let path = path.display().to_string();
            move |source: io::Error| FetchError::Io { path, source }
        };
        fs::create_dir_all(&self.cache_dir).map_err(io_err(&self.cache_dir))?;
        let mut tmp =
            tempfile::NamedTempFile::new_in(&self.cache_dir).map_err(io_err(&self.cache_dir))?;

        let tmp_path = tmp.path().to_path_buf();
        let mut sink = HashingWriter {
            inner: tmp.as_file_mut(),
            hasher: Sha256::new(),
            written: 0,
        };
        self.transport.get(url, &mut sink)?;
        sink.flush().map_err(io_err(&tmp_path))?;
        let got_hash = hex::encode(sink.hasher.finalize());
        let got_bytes = sink.written;

        // Verify BEFORE the bytes become visible under the cache key.
        if got_hash != expected {
            return Err(FetchError::HashMismatch {
                url: url.to_string(),
                expected,
                got: got_hash,
            });
        }
        if let Some(expected_bytes) = bytes
            && got_bytes != expected_bytes
        {
            return Err(FetchError::SizeMismatch {
                url: url.to_string(),
                expected: expected_bytes,
                got: got_bytes,
            });
        }

        tmp.persist(&cached).map_err(|e| io_err(&cached)(e.error))?;
        Ok(cached)
    }

    /// Fetch every asset of a Rebrickable pin. Returns asset filename
    /// (e.g. `parts.csv.gz`) → cached path.
    pub fn fetch_rebrickable(
        &self,
        pin: &RebrickablePin,
    ) -> Result<BTreeMap<String, PathBuf>, FetchError> {
        pin.file_fingerprints
            .iter()
            .map(|(name, fp)| {
                let path = self.fetch_verified(&fp.mirror_url, &fp.sha256, Some(fp.bytes))?;
                Ok((name.clone(), path))
            })
            .collect()
    }

    /// Fetch an LDraw pin's merged-tree zip and content manifest.
    pub fn fetch_ldraw(&self, pin: &LdrawPin) -> Result<LdrawAssets, FetchError> {
        Ok(LdrawAssets {
            archive: self.fetch_verified(&pin.asset_url, &pin.asset_sha256, None)?,
            manifest: self.fetch_verified(&pin.manifest_url, &pin.manifest_sha256, None)?,
        })
    }

    /// Fetch a catalog pin's built `catalog.sqlite`.
    pub fn fetch_catalog(&self, pin: &CatalogPin) -> Result<PathBuf, FetchError> {
        self.fetch_verified(&pin.asset_url, &pin.sha256, Some(pin.bytes))
    }

    /// Re-hash an existing cache entry; discard it if corrupted.
    fn verified_hit(&self, cached: &Path, expected: &str) -> Result<Option<PathBuf>, FetchError> {
        let mut file = match fs::File::open(cached) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(FetchError::Io {
                    path: cached.display().to_string(),
                    source: e,
                });
            }
        };
        let mut hasher = Sha256::new();
        io::copy(&mut file, &mut hasher).map_err(|e| FetchError::Io {
            path: cached.display().to_string(),
            source: e,
        })?;
        if hex::encode(hasher.finalize()) == expected {
            Ok(Some(cached.to_path_buf()))
        } else {
            // Corrupted entry: remove and signal a re-fetch. Removal failure
            // is ignored — the re-downloaded file replaces it via rename.
            let _ = fs::remove_file(cached);
            Ok(None)
        }
    }
}

/// Cached assets of an LDraw release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LdrawAssets {
    /// The merged library tree as a zip archive.
    pub archive: PathBuf,
    /// The content manifest (TSV) whose hash is the tree's pinned identity.
    pub manifest: PathBuf,
}

/// Tees writes into a sha256 hasher and byte counter.
struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    written: u64,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
