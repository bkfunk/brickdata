//! `fetch` subcommand and pinned-LDraw materialization: warm the verified
//! content-addressed cache from the committed pins, and extract a pinned
//! LDraw library tree for `build --ldraw-pin`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use brickdata::extract::unzip_to;
use brickdata::fetch::{Fetcher, HttpTransport};
use brickdata::pin::{LdrawPin, RebrickablePin};

use crate::build;

/// Warm the cache with every pinned Rebrickable CSV (idempotent: verified
/// hits do no network I/O) and materialize the named per-tag layout the
/// build reads.
pub fn run(pin_path: &Path, cache_dir: &Path) -> Result<()> {
    let pin = RebrickablePin::from_path(pin_path)
        .with_context(|| format!("loading pin {}", pin_path.display()))?;
    let fetcher: Fetcher<HttpTransport> = Fetcher::new(cache_dir);
    let dir = build::materialize_csv_dir(&fetcher, &pin, cache_dir)?;
    tracing::info!(
        "cached {} CSVs at {}",
        pin.file_fingerprints.len(),
        dir.display()
    );
    Ok(())
}

/// Fetch + extract the pinned LDraw library zip; returns the library root
/// (the directory containing `parts/`). Reuses a previously extracted tree;
/// a half-extracted one (no `parts/`) is rebuilt via extract-to-tmp + rename
/// so an interrupted extraction can never be mistaken for a library.
pub fn pinned_ldraw_root(pin_path: &Path, cache_dir: &Path) -> Result<PathBuf> {
    let pin = LdrawPin::from_path(pin_path)
        .with_context(|| format!("loading pin {}", pin_path.display()))?;
    let fetcher: Fetcher<HttpTransport> = Fetcher::new(cache_dir);

    let tree = cache_dir.join("trees").join(&pin.mirror_tag);
    if tree.join("parts").is_dir() {
        return Ok(tree);
    }
    if tree.exists() {
        fs::remove_dir_all(&tree)
            .with_context(|| format!("remove half-extract {}", tree.display()))?;
    }

    let assets = fetcher
        .fetch_ldraw(&pin)
        .with_context(|| format!("fetching LDraw snapshot {}", pin.mirror_tag))?;
    let tmp = cache_dir
        .join("trees")
        .join(format!(".extract-{}", pin.mirror_tag));
    if tmp.exists() {
        fs::remove_dir_all(&tmp).with_context(|| format!("remove stale {}", tmp.display()))?;
    }
    unzip_to(&assets.archive, &tmp)
        .with_context(|| format!("extracting {}", assets.archive.display()))?;
    if !tmp.join("parts").is_dir() {
        anyhow::bail!(
            "snapshot {} contains no parts/ directory — not an LDraw library tree",
            pin.mirror_tag
        );
    }
    fs::rename(&tmp, &tree)
        .with_context(|| format!("moving tree into place at {}", tree.display()))?;
    tracing::info!("extracted LDraw tree {}", tree.display());
    Ok(tree)
}
