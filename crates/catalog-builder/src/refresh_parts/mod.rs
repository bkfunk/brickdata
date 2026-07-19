//! `refresh-part-mappings` subcommand — pulls the full Rebrickable parts
//! listing and regenerates the committed `part_num → external cross-refs`
//! pin that the catalog `build` will ingest (the build-time reader lands
//! with #72/#82; this tool produces the input it depends on).
//!
//! ## Pipeline
//!
//! ```text
//!         GET /api/v3/lego/parts/?page_size=1000   (~64 pages)
//!                              │
//!                              ▼  per part: canonicalize external_ids.LDraw
//!                              │  (strip -fN, dedup) + keep all other systems
//!                              ▼
//!     data/rebrickable/part_crossrefs.ron   ← committed pin
//! ```
//!
//! ## Why this exists
//!
//! `build` is hermetic — it works only from the pinned CSV snapshot and the
//! LDraw library. The bulk CSVs carry **no** external-id cross-refs (LDraw,
//! BrickLink, BrickOwl, …); those live only in the Rebrickable parts API.
//! So this tool is the *only* source for them: it captures every part's
//! cross-refs once and pins them, for `build` to ingest into
//! `catalog.sqlite` (that reader lands with #72/#82). Same hermetic
//! rationale as `csv-snapshot.ron` and `color_names.ron`.
//!
//! The `ldraw` cross-ref is special: it's the design_id our geometry is
//! keyed on, so it's canonicalized to a single id (via the *same*
//! [`canonical_design_id`] the catalog keys on — never a parallel rule that
//! could drift). Every other system is stored as the raw id list Rebrickable
//! reports. See [`pin::RbPartCrossRefs`] and
//! `docs/lego-reference/ldraw-part-numbering.md`.
//!
//! ## Idempotent refresh
//!
//! Only the cross-ref *content* decides whether the pin is rewritten; the
//! `generated` date is ignored for that comparison. A re-run producing an
//! identical map leaves the file (and its old date) untouched.
//!
//! ## Auth
//!
//! Requires a Rebrickable API key, via `--api-key` or the
//! `REBRICKABLE_API_KEY` environment variable. Header sent is
//! `Authorization: key <key>` (Rebrickable's scheme, not `Bearer`).

mod api;
mod pin;

// The committed cross-ref pin is read at build time (the `build` subcommand
// translates `inventory_parts.part_num` → LDraw `design_id` via the `ldraw`
// field), so the pin types are part of the crate's public surface.
pub use pin::{RbCrossRefPin, RbPartCrossRefs};

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::core::canonical_design_id;
use anyhow::{Context, Result};

use crate::util::{atomic_write, today_iso, workspace_root};
use api::{ApiPart, fetch_parts};
use pin::RbCrossRefDiff;

/// Committed location of the cross-ref pin, relative to the workspace root.
/// Owned here so this module is the single source of truth for the path — both
/// `refresh-part-mappings` (which writes it) and the catalog `build` (which
/// reads it) resolve it through [`crossrefs_path`].
const CROSSREFS_REL: &str = "data/rebrickable/part_crossrefs.ron";

/// Absolute path to the committed `part_crossrefs.ron` pin.
pub fn crossrefs_path() -> PathBuf {
    workspace_root().join(CROSSREFS_REL)
}

pub fn run(api_key: &str, dry_run: bool) -> Result<()> {
    let path = crossrefs_path();

    // ── 1. Fetch the full parts listing and derive cross-refs ────────
    let parts = fetch_parts(api_key)?;
    let crossrefs = build_crossrefs(&parts);
    tracing::info!(
        "derived cross-refs for {} parts from {} API rows",
        crossrefs.len(),
        parts.len(),
    );

    // ── 2. Diff against the committed pin (content only) ─────────────
    let existing = RbCrossRefPin::from_pinned_file(&path)?;
    let empty = BTreeMap::new();
    let old = existing.as_ref().map(|p| &p.parts).unwrap_or(&empty);
    if existing.is_none() {
        tracing::info!(
            "no existing pin — {} parts would be written",
            crossrefs.len()
        );
    }
    let diff = RbCrossRefDiff::of(old, &crossrefs);
    diff.log();
    let unchanged = diff.is_empty();

    if dry_run {
        tracing::info!(
            "part_crossrefs.ron {} (--dry-run, no write)",
            if unchanged {
                "up to date"
            } else {
                "WOULD be regenerated"
            },
        );
        return Ok(());
    }
    if unchanged {
        tracing::info!("cross-refs unchanged — part_crossrefs.ron not rewritten");
        return Ok(());
    }

    // ── 3. Render, round-trip-check, write ───────────────────────────
    let new_pin = RbCrossRefPin {
        generated: today_iso(),
        parts: crossrefs,
    };
    let new_ron = new_pin.render();
    // Catch a formatter bug before it corrupts the committed file: the
    // rendered RON must parse back into the same value.
    let parsed: RbCrossRefPin = ron::from_str(&new_ron)
        .context("generated RON did not round-trip through ron::from_str (formatter bug)")?;
    anyhow::ensure!(
        parsed == new_pin,
        "generated RON round-tripped to a different value (formatter bug)"
    );
    atomic_write(&path, new_ron.as_bytes())?;
    tracing::info!("part_crossrefs.ron regenerated: {}", path.display());
    Ok(())
}

// ─── Cross-ref derivation ───────────────────────────────────────────────

/// Outcome of collapsing a part's `external_ids.LDraw` array to one
/// canonical design id.
#[derive(Debug, PartialEq, Eq)]
enum Canonical {
    /// Exactly one distinct id after canonicalization.
    One(String),
    /// No LDraw ids — the part has no LDraw geometry.
    None,
    /// More than one distinct id survived — bad data, sorted candidates.
    Anomaly(Vec<String>),
}

/// Collapse the raw LDraw id array to a single canonical design id, using the
/// catalog's own [`canonical_design_id`] rule (strip `-fN`, with `-f0` and
/// overflow left intact) so the pin can never disagree with how the catalog
/// keys a design — then dedup.
fn canonical_ldraw(ldraw_ids: &[String]) -> Canonical {
    let distinct: BTreeSet<&str> = ldraw_ids
        .iter()
        .map(|id| canonical_design_id(id).0)
        .collect();
    match distinct.len() {
        0 => Canonical::None,
        1 => Canonical::One(distinct.into_iter().next().unwrap().to_string()),
        _ => Canonical::Anomaly(distinct.into_iter().map(String::from).collect()),
    }
}

/// Build the `part_num → cross-refs` map: one entry per part with a
/// resolvable LDraw design (the geometry key), carrying every other external
/// system raw. Parts without LDraw geometry, and LDraw anomalies, are skipped.
fn build_crossrefs(parts: &[ApiPart]) -> BTreeMap<String, RbPartCrossRefs> {
    let mut map = BTreeMap::new();
    let mut no_ldraw = 0usize;
    let mut anomalies = 0usize;
    for part in parts {
        let ldraw = match canonical_ldraw(&part.ids("LDraw")) {
            Canonical::One(id) => id,
            Canonical::None => {
                no_ldraw += 1;
                continue;
            }
            Canonical::Anomaly(candidates) => {
                anomalies += 1;
                tracing::warn!(
                    "part {} has multiple distinct LDraw ids after -fN collapse: {:?} — skipping",
                    part.part_num,
                    candidates,
                );
                continue;
            }
        };
        // Keep every other system raw; LDraw is promoted to the `ldraw`
        // scalar, so drop it from the cross-ref map.
        let mut external_ids = part.all_ids();
        external_ids.remove("LDraw");
        map.insert(
            part.part_num.clone(),
            RbPartCrossRefs {
                ldraw,
                external_ids,
            },
        );
    }
    tracing::info!(
        "{} parts without LDraw geometry skipped; {} anomalies skipped",
        no_ldraw,
        anomalies,
    );
    map
}

// ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::api::parse_parts;
    use super::*;
    use serde_json::Value;

    const FIXTURE_PAGE: &str = include_str!("../../tests/fixtures/api/parts-page-sample.json");

    fn fixture_parts() -> Vec<ApiPart> {
        let body: Value = serde_json::from_str(FIXTURE_PAGE).expect("parse fixture");
        parse_parts(&body).expect("parse parts")
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn canonical_collapses_fn_variants_to_one() {
        // The worked example: two flexion positions, one design.
        assert_eq!(
            canonical_ldraw(&ids(&["92693c01-f1", "92693c01-f2"])),
            Canonical::One("92693c01".into()),
        );
    }

    #[test]
    fn canonical_identity_for_simple_part() {
        assert_eq!(
            canonical_ldraw(&ids(&["3001"])),
            Canonical::One("3001".into())
        );
    }

    #[test]
    fn canonical_none_when_no_ldraw_ids() {
        assert_eq!(canonical_ldraw(&[]), Canonical::None);
    }

    #[test]
    fn canonical_anomaly_for_two_distinct_ids() {
        assert_eq!(
            canonical_ldraw(&ids(&["3001", "3002"])),
            Canonical::Anomaly(vec!["3001".into(), "3002".into()]),
        );
    }

    #[test]
    fn canonical_uses_catalog_rule_for_f0_edge() {
        // Reusing core's canonical_design_id (not a parallel stripper) means
        // `-f0` is NOT a flexion suffix — it stays its own canonical id.
        assert_eq!(
            canonical_ldraw(&ids(&["92693c01-f0"])),
            Canonical::One("92693c01-f0".into()),
        );
    }

    #[test]
    fn build_captures_all_external_ids_with_ldraw_promoted() {
        let map = build_crossrefs(&fixture_parts());
        let brick = map.get("3001").expect("3001 mapped");
        assert_eq!(brick.ldraw, "3001");
        // LDraw is promoted to the scalar, never duplicated in external_ids.
        assert!(!brick.external_ids.contains_key("LDraw"));
        // Other systems kept raw, multiple ids preserved.
        assert_eq!(
            brick.external_ids.get("BrickLink"),
            Some(&ids(&["3001", "3001old"]))
        );
        assert_eq!(
            brick.external_ids.get("LEGO"),
            Some(&ids(&["3001", "300126"]))
        );
    }

    #[test]
    fn build_skips_no_ldraw_and_anomalies() {
        let map = build_crossrefs(&fixture_parts());
        // 3001 (identity), 92693 (-fN collapse), 33299c01 (single).
        assert_eq!(map.get("92693").map(|r| r.ldraw.as_str()), Some("92693c01"));
        assert_eq!(map.get("33299c01").map(|r| r.ldraw.as_str()), Some("33299"));
        assert!(!map.contains_key("rb-only-99999"), "no LDraw geometry");
        assert!(!map.contains_key("anomaly-2distinct"), "anomaly skipped");
        assert!(!map.contains_key("973c00"), "empty external_ids");
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn ron_round_trips_through_parser() {
        let pin = RbCrossRefPin {
            generated: "2026-06-21".into(),
            parts: build_crossrefs(&fixture_parts()),
        };
        let text = pin.render();
        let parsed: RbCrossRefPin = ron::from_str(&text).expect("parse generated RON");
        assert_eq!(parsed, pin);
    }

    #[test]
    fn render_is_byte_stable_across_runs() {
        let pin = RbCrossRefPin {
            generated: "2026-06-21".into(),
            parts: build_crossrefs(&fixture_parts()),
        };
        assert_eq!(pin.render(), pin.render());
    }

    #[test]
    fn diff_reports_added_changed_removed() {
        let parts = build_crossrefs(&fixture_parts());
        let empty = BTreeMap::new();
        let added = RbCrossRefDiff::of(&empty, &parts);
        assert_eq!(added.added.len(), parts.len());
        assert!(added.removed.is_empty() && added.changed.is_empty());
        // Identical maps → no diff.
        let none = RbCrossRefDiff::of(&parts, &parts);
        assert!(none.is_empty());
    }
}
