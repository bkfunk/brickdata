//! Parsing for `pins/*.ron` — the immutable records emitted by the mirror
//! recipes, one per dated release. Three shapes exist:
//!
//! - `rebrickable-*.ron`: per-file fingerprints for the bulk-CSV assets
//! - `ldraw-*.ron`: merged-tree zip + content manifest fingerprints
//! - `catalog-*.ron`: a single built `catalog.sqlite` fingerprint
//!
//! The shapes are distinguished structurally (an LDraw pin has
//! `manifest_sha256`; a Rebrickable pin has `file_fingerprints`), mirroring
//! how the shell `verify` recipe dispatches.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Fingerprint of one release asset: content hash, size, and where the
/// mirror hosts it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetFingerprint {
    /// Lowercase hex sha256 of the asset bytes.
    pub sha256: String,
    /// Exact asset size in bytes.
    pub bytes: u64,
    /// Release-asset download URL.
    pub mirror_url: String,
}

/// Pin for a `rebrickable-YYYY-MM-DD` release: the 8 bulk CSVs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebrickablePin {
    /// Release tag, e.g. `rebrickable-2026-06-01`.
    pub mirror_tag: String,
    /// UTC date the snapshot was cut (YYYY-MM-DD).
    pub snapshot_date: String,
    /// Asset filename (e.g. `parts.csv.gz`) → fingerprint.
    pub file_fingerprints: BTreeMap<String, AssetFingerprint>,
}

/// Pin for an `ldraw-YYYY-MM-DD` release: merged library tree + manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LdrawPin {
    /// Release tag, e.g. `ldraw-2026-06-01`.
    pub mirror_tag: String,
    /// UTC date the snapshot was cut (YYYY-MM-DD).
    pub snapshot_date: String,
    /// Download URL of the merged-tree zip.
    pub asset_url: String,
    /// Lowercase hex sha256 of the merged-tree zip.
    pub asset_sha256: String,
    /// Download URL of the content manifest (TSV).
    pub manifest_url: String,
    /// Lowercase hex sha256 of the manifest — the pinned identity of the
    /// merged tree (the zip alone isn't reproducible from the recipe).
    pub manifest_sha256: String,
    /// Number of files in the merged tree (manifest line count).
    pub file_count: u64,
}

/// Pin for a `catalog-YYYY-MM-DD` release: one built `catalog.sqlite`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPin {
    /// Release tag, e.g. `catalog-2026-07-07`.
    pub mirror_tag: String,
    /// Download URL of the built catalog.
    pub asset_url: String,
    /// Lowercase hex sha256 of the catalog bytes.
    pub sha256: String,
    /// Exact size in bytes.
    pub bytes: u64,
}

/// Any pin, shape-detected. Use [`Pin::from_path`] / [`Pin::from_str`] when
/// the caller doesn't know which release kind a pin file describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    /// A `rebrickable-*` release pin.
    Rebrickable(RebrickablePin),
    /// An `ldraw-*` release pin.
    Ldraw(LdrawPin),
    /// A `catalog-*` release pin.
    Catalog(CatalogPin),
}

/// Error parsing a pin file.
#[derive(Debug, thiserror::Error)]
pub enum PinError {
    /// The pin file could not be read.
    #[error("failed to read pin file {path}: {source}")]
    Io {
        /// Path of the unreadable file.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The pin text is not valid RON of the expected shape.
    #[error("failed to parse pin{}: {source}", path_suffix(path))]
    Parse {
        /// Path, when parsing from a file (empty when from a string).
        path: String,
        /// Underlying RON parse error.
        #[source]
        source: Box<ron::error::SpannedError>,
    },
}

fn path_suffix(path: &str) -> String {
    if path.is_empty() {
        String::new()
    } else {
        format!(" file {path}")
    }
}

fn parse<T: for<'de> Deserialize<'de>>(text: &str, path: &str) -> Result<T, PinError> {
    ron::from_str(text).map_err(|e| PinError::Parse {
        path: path.to_string(),
        source: Box::new(e),
    })
}

fn read(path: &Path) -> Result<String, PinError> {
    std::fs::read_to_string(path).map_err(|e| PinError::Io {
        path: path.display().to_string(),
        source: e,
    })
}

macro_rules! impl_pin_parse {
    ($ty:ty) => {
        impl $ty {
            /// Parse from RON text.
            pub fn from_ron_str(text: &str) -> Result<Self, PinError> {
                parse(text, "")
            }

            /// Read and parse a pin file.
            pub fn from_path(path: impl AsRef<Path>) -> Result<Self, PinError> {
                let path = path.as_ref();
                parse(&read(path)?, &path.display().to_string())
            }
        }
    };
}

impl_pin_parse!(RebrickablePin);
impl_pin_parse!(LdrawPin);
impl_pin_parse!(CatalogPin);

impl Pin {
    /// Parse RON text, detecting the pin shape structurally.
    pub fn from_ron_str(text: &str) -> Result<Self, PinError> {
        Self::detect(text, "")
    }

    /// Read and parse a pin file, detecting the pin shape structurally.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, PinError> {
        let path = path.as_ref();
        Self::detect(&read(path)?, &path.display().to_string())
    }

    fn detect(text: &str, path: &str) -> Result<Self, PinError> {
        // Same structural dispatch as the shell `verify` recipe: field
        // presence, not filename, decides the shape.
        if text.contains("manifest_sha256") {
            parse::<LdrawPin>(text, path).map(Pin::Ldraw)
        } else if text.contains("file_fingerprints") {
            parse::<RebrickablePin>(text, path).map(Pin::Rebrickable)
        } else {
            parse::<CatalogPin>(text, path).map(Pin::Catalog)
        }
    }

    /// The release tag this pin records (e.g. `rebrickable-2026-06-01`).
    pub fn mirror_tag(&self) -> &str {
        match self {
            Pin::Rebrickable(p) => &p.mirror_tag,
            Pin::Ldraw(p) => &p.mirror_tag,
            Pin::Catalog(p) => &p.mirror_tag,
        }
    }
}
