//! `build` subcommand — assembles `catalog.sqlite`.
//!
//! Pipeline, in order, all within one open connection before it closes:
//!
//! 1. Resolve the pinned Rebrickable CSVs through the verified
//!    content-addressed cache (`brickdata::Fetcher`): a cache hit is
//!    re-hashed, a miss is downloaded and verified against the pin's
//!    sha256/bytes before becoming visible, so the build is reproducible
//!    from the pin regardless of cache state.
//! 2. Scan the LDraw library into `ldraw_part`.
//! 3. Ingest the small Rebrickable tables, then aggregate `inventory_parts`
//!    into the per-part fact + summary tables (`rb_part_color_set` /
//!    `rb_part_summary`), translating ids via the committed cross-ref pin.
//! 4. Ingest `part_relationships` filtered to rows touching the catalog
//!    (`rb_part_relationships`), translating via the same pin.
//! 5. Finalize: the `part` view + `part_fts` full-text index, `ANALYZE`/
//!    `VACUUM`, and `meta.build_status = 'complete'` as the very last write
//!    (so a half-built DB is detectable by a missing/non-`complete` status).
//!
//! The output is a read-only SQLite DB published as a `catalog-*` release
//! asset; consumers query it and never see this builder.

use anyhow::{Context, Result};
use brickdata::fetch::{Fetcher, HttpTransport};
use brickdata::pin::RebrickablePin;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

use crate::ldraw_part;
use crate::refresh_parts::RbCrossRefPin;
use crate::util;

mod finalize;
mod inventory;
mod rb_elements;
mod rb_ingest;
mod rb_inventories;
mod rb_part_categories;
mod rb_part_relationships;
mod rb_parts;
mod rb_sets;
mod rb_themes;
mod resolve;

/// Bumped whenever the on-disk schema changes in a way the runtime must
/// notice. Stamped into `meta` so a mismatched DB is detectable at load.
const SCHEMA_VERSION: u32 = 1;

/// Build `catalog.sqlite` from a Rebrickable pin file, materializing the
/// pinned CSVs through the verified cache. Thin wrapper over [`run_with`]
/// for the CLI.
pub fn run(pin_path: &Path, cache_dir: &Path, ldraw_dir: &Path, out: &Path) -> Result<()> {
    let pin = RebrickablePin::from_path(pin_path)
        .with_context(|| format!("loading pin {}", pin_path.display()))?;
    let fetcher: Fetcher<HttpTransport> = Fetcher::new(cache_dir);
    let csv_dir = materialize_csv_dir(&fetcher, &pin, cache_dir)?;
    // The cross-ref module owns this path (single source of truth), so `build`
    // and `refresh-part-mappings` can never disagree on where the pin lives.
    let crossrefs_path = crate::refresh_parts::crossrefs_path();
    run_with(&pin, &csv_dir, &crossrefs_path, ldraw_dir, out)
}

/// Fetch every CSV of `pin` through the verified content-addressed cache and
/// hard-link (fall back: copy) each one under `<cache_dir>/rebrickable/
/// <mirror_tag>/<name>`, the flat named layout the ingest slices read.
pub fn materialize_csv_dir(
    fetcher: &Fetcher<HttpTransport>,
    pin: &RebrickablePin,
    cache_dir: &Path,
) -> Result<PathBuf> {
    let files = fetcher
        .fetch_rebrickable(pin)
        .with_context(|| format!("fetching pinned CSVs for {}", pin.mirror_tag))?;
    let dir = cache_dir.join("rebrickable").join(&pin.mirror_tag);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    for (name, src) in &files {
        let dst = dir.join(name);
        // The link target is content-addressed and immutable, so an existing
        // link is already correct; replace anything else.
        if dst.exists() {
            fs::remove_file(&dst).with_context(|| format!("remove stale {}", dst.display()))?;
        }
        fs::hard_link(src, &dst)
            .or_else(|_| fs::copy(src, &dst).map(|_| ()))
            .with_context(|| format!("materialize {}", dst.display()))?;
    }
    Ok(dir)
}

/// Build the catalog DB from explicit inputs. Separated from [`run`] so
/// tests can point at a temp CSV dir, cross-ref pin, and output without
/// touching the committed pins or any cache.
pub fn run_with(
    snap: &RebrickablePin,
    csv_dir: &Path,
    crossrefs_path: &Path,
    ldraw_dir: &Path,
    out: &Path,
) -> Result<()> {
    // Fail early on a bad library path — later slices scan `parts/` for the
    // `ldraw_part` table, so a missing `parts/` is a setup error, not an
    // empty catalog.
    let parts_dir = ldraw_dir.join("parts");
    if !parts_dir.is_dir() {
        anyhow::bail!(
            "{} does not look like an LDraw library: no parts/ directory at {}",
            ldraw_dir.display(),
            parts_dir.display(),
        );
    }

    // Ensure the output directory exists before staging anything in it.
    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create output dir {}", parent.display()))?;
        }
    }

    // Build into a sibling `.tmp` and rename over `out` on success, so a
    // failed build never leaves a half-written DB at the final path.
    let tmp = tmp_path(out);
    // A leftover `.tmp` from a previously aborted build would let `open`
    // reuse stale contents; start clean.
    if tmp.exists() {
        fs::remove_file(&tmp).with_context(|| format!("remove stale {}", tmp.display()))?;
    }

    build_into(&tmp, snap, csv_dir, crossrefs_path, ldraw_dir)
        .with_context(|| format!("building {}", tmp.display()))?;

    // The build pragmas leave SQLite's pages unflushed (`synchronous = OFF`),
    // so fsync the staged DB before the atomic rename — otherwise a power loss
    // after the rename could replace a previously-good catalog.sqlite with a
    // torn file.
    util::fsync_path(&tmp).with_context(|| format!("fsync {}", tmp.display()))?;
    util::replace_file(&tmp, out)?;
    tracing::info!("wrote {}", out.display());
    Ok(())
}

/// `out` with `.tmp` appended (not replacing the extension), so
/// `catalog.sqlite` stages at `catalog.sqlite.tmp`.
fn tmp_path(out: &Path) -> PathBuf {
    util::with_suffix(out, ".tmp")
}

/// Open `db_path`, apply build pragmas, and write everything the catalog
/// needs. The connection is dropped (closed) when this returns, releasing
/// the exclusive lock before the caller renames the file.
fn build_into(
    db_path: &Path,
    snap: &RebrickablePin,
    csv_dir: &Path,
    crossrefs_path: &Path,
    ldraw_dir: &Path,
) -> Result<()> {
    let conn = Connection::open(db_path).with_context(|| format!("open {}", db_path.display()))?;
    apply_build_pragmas(&conn)?;
    create_meta(&conn)?;
    stamp(&conn, "schema_version", &SCHEMA_VERSION.to_string())?;
    stamp(&conn, "snapshot_date", &snap.snapshot_date)?;
    // The exact blockstar-data release the Rebrickable inputs came from, so a
    // built DB records which immutable snapshot produced it (the
    // embedded-DB <-> mirrored-inputs version match; see issue #86).
    stamp(&conn, "rebrickable_snapshot", &snap.mirror_tag)?;
    stamp(&conn, "builder_version", env!("CARGO_PKG_VERSION"))?;

    let scan = ldraw_part::build(&conn, ldraw_dir)?;
    stamp(&conn, "ldraw_part_count", &scan.parts.to_string())?;
    stamp(&conn, "ldraw_moved_to_count", &scan.moved_to.to_string())?;
    stamp(&conn, "ldraw_alias_count", &scan.aliases.to_string())?;

    ingest_rebrickable_tables(&conn, csv_dir)?;

    // part_num -> LDraw design_id translations come from the committed pin. A
    // missing pin is a setup error: `build` is hermetic and can't derive LDraw
    // ids any other way. Loaded once, shared by every slice that translates.
    let rb_cross_ref_pin = RbCrossRefPin::from_pinned_file(crossrefs_path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "part_crossrefs pin not found at {} — run `just refresh-part-mappings`",
            crossrefs_path.display()
        )
    })?;
    stamp(&conn, "part_mappings_date", &rb_cross_ref_pin.generated)?;

    // The shared part_num → design_id translation: the pin, the literal
    // fallback against the ldraw_part table populated above, and the
    // redirect chase over the ldraw_moved_to + ldraw_alias hops (#112).
    let resolver = resolve::PartResolver::new(&conn, &rb_cross_ref_pin)?;

    // Assign dense set ids, then aggregate the ~1M-row inventory_parts CSV
    // into the per-part fact + summary tables.
    rb_sets::add_set_ids(&conn)?;
    let inv = inventory::build(&conn, csv_dir, &resolver)?;
    stamp_all(&conn, &inv.meta_rows())?;

    // Part↔part relationships, filtered to rows touching the catalog (#82).
    let rels = rb_part_relationships::build(&conn, csv_dir, &resolver)?;
    stamp_all(&conn, &rels.meta_rows())?;

    // Finalize (#73): the `part` view + FTS index, then ANALYZE/VACUUM, then
    // `build_status = 'complete'` as the very last write so an interrupted
    // build (which the atomic rename already keeps off the final path) can
    // also never masquerade as a finished DB.
    let fin = finalize::run(&conn, &resolver)?;
    stamp_all(&conn, &fin.meta_rows())?;
    finalize::optimize(&conn)?;
    stamp(&conn, "build_status", "complete")?;
    Ok(())
}

/// Raw-ingest the small pinned Rebrickable CSVs from the metadata cache (the
/// directory of pinned `.csv.gz` files — not the geometry cache), stamping each
/// table's row count into `meta` for observability. Any table's `build` failing
/// propagates here via `?`, aborting the whole build; since the caller writes to
/// a temp DB and only renames on success, a failure leaves no partial catalog.
/// `part_relationships` and `inventory_parts` are deliberately not ingested here
/// — both translate ids via the cross-ref pin and run after this
/// (see [`rb_part_relationships`] and the [`inventory`] aggregation).
fn ingest_rebrickable_tables(conn: &Connection, csv_dir: &Path) -> Result<()> {
    let counts = [
        ("rb_parts_count", rb_parts::build(conn, csv_dir)?),
        (
            "rb_part_categories_count",
            rb_part_categories::build(conn, csv_dir)?,
        ),
        ("rb_elements_count", rb_elements::build(conn, csv_dir)?),
        ("rb_themes_count", rb_themes::build(conn, csv_dir)?),
        ("rb_sets_count", rb_sets::build(conn, csv_dir)?),
        (
            "rb_inventories_count",
            rb_inventories::build(conn, csv_dir)?,
        ),
    ];
    for (key, count) in counts {
        stamp(conn, key, &count.to_string())?;
    }
    Ok(())
}

/// Pragmas for a build-once / read-only-at-runtime DB: skip the journal and
/// per-write fsyncs for speed (a crashed mid-build is simply re-run from the
/// same pin), and hold an exclusive lock so nothing else touches the file
/// mid-build. The final DB is fsync'd once in [`run_with`] before the atomic
/// rename, so the committed file is still durable. `execute_batch` is used
/// because `PRAGMA journal_mode` returns a row, which `pragma_update` does not
/// expect.
fn apply_build_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA locking_mode = EXCLUSIVE;",
    )
    .context("set build pragmas")
}

fn create_meta(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID",
        [],
    )
    .context("create meta table")?;
    Ok(())
}

/// Upsert one `meta` row. Idempotent so later slices can re-stamp a key
/// (e.g. finalize `build_status`) without caring whether it exists yet.
fn stamp(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )
    .with_context(|| format!("stamp meta[{key}]"))?;
    Ok(())
}

/// Upsert several `meta` rows at once — the `(key, value)` array a stats struct
/// emits from its `meta_rows()`, so adding a counter can't drift from its stamp
/// site (the whole point of [`inventory::InventoryStats::meta_rows`]).
fn stamp_all(conn: &Connection, rows: &[(&str, String)]) -> Result<()> {
    for (key, value) in rows {
        stamp(conn, key, value)?;
    }
    Ok(())
}
