//! The committed cross-ref pin: Rebrickable `part_num` → [`RbPartCrossRefs`],
//! plus its RON (de)serialization, loading, and diffing.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::util::ron_quote;

/// One part's external-id cross-references, **as Rebrickable reports them**.
///
/// `ldraw` is the single canonicalized scalar — the LDraw `design_id` our
/// geometry is keyed on, with `-fN` flexion variants already collapsed (via
/// [`crate::core::canonical_design_id`]). Every other system is
/// the raw id list straight from the API: a `bricklink` entry here means
/// "the BrickLink id(s) *according to Rebrickable*", not authoritative
/// BrickLink ground truth, and a part can legitimately carry several. LDraw
/// is the only system absent from `external_ids` — it's promoted to the
/// `ldraw` scalar.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct RbPartCrossRefs {
    pub ldraw: String,
    /// Other systems: name → raw ids, e.g. `"BrickLink" → ["3001", "3001old"]`.
    pub external_ids: BTreeMap<String, Vec<String>>,
}

/// The whole pin file: a date stamp plus the `part_num → cross-refs` map.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct RbCrossRefPin {
    /// ISO date (`YYYY-MM-DD`) the cross-refs were last regenerated. Stamped
    /// only when the cross-ref content actually changes, so the daily date
    /// never churns the committed file on its own.
    pub generated: String,
    /// Rebrickable `part_num` → cross-refs. `BTreeMap` so iteration (and the
    /// rendered RON) is key-sorted and byte-stable.
    pub parts: BTreeMap<String, RbPartCrossRefs>,
}

impl RbCrossRefPin {
    /// Read and parse the committed pin. A *missing* file is `Ok(None)`
    /// (first run); any other read or parse error surfaces — we won't
    /// silently treat a corrupt or unreadable pin as "no existing data" and
    /// clobber it.
    pub fn from_pinned_file(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("read existing {}", path.display())),
        };
        let pin: RbCrossRefPin =
            ron::from_str(&text).with_context(|| format!("parse existing {}", path.display()))?;
        Ok(Some(pin))
    }

    /// Borrowed `part_num → ldraw design_id` lookup over the whole pin — the
    /// translation the catalog `build` keys `inventory_parts` (and, later, other
    /// Rebrickable tables) on. Borrows from `self`, so the pin must outlive it.
    pub fn part_to_ldraw(&self) -> HashMap<&str, &str> {
        self.parts
            .iter()
            .map(|(part_num, refs)| (part_num.as_str(), refs.ldraw.as_str()))
            .collect()
    }

    /// Render the pin as `part_crossrefs.ron`: a header, the `generated`
    /// date, then one key-sorted `"part_num": (ldraw: …, external_ids: …),`
    /// line per part. Hand-rolled rather than `ron::ser::to_string_pretty`
    /// for the same reasons as `color_names.ron` — a tight, reviewable,
    /// deterministic diff. Determinism is what lets the caller compare maps
    /// to decide whether to rewrite.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(HEADER);
        out.push_str("RbCrossRefPin(\n");
        out.push_str(&format!("    generated: {},\n", ron_quote(&self.generated)));
        out.push_str("    // rebrickable part_num -> external cross-refs (LDraw canonicalized)\n");
        out.push_str("    parts: {\n");
        for (part_num, refs) in &self.parts {
            out.push_str(&format!(
                "        {}: {},\n",
                ron_quote(part_num),
                refs.render()
            ));
        }
        out.push_str("    },\n");
        out.push_str(")\n");
        out
    }
}

impl RbPartCrossRefs {
    /// One-line RON form: `(ldraw: "3001", external_ids: {"BrickLink": ["3001"]})`.
    fn render(&self) -> String {
        let systems: Vec<String> = self
            .external_ids
            .iter()
            .map(|(system, ids)| {
                let quoted: Vec<String> = ids.iter().map(|s| ron_quote(s)).collect();
                format!("{}: [{}]", ron_quote(system), quoted.join(", "))
            })
            .collect();
        format!(
            "(ldraw: {}, external_ids: {{{}}})",
            ron_quote(&self.ldraw),
            systems.join(", "),
        )
    }
}

/// Added / removed / changed part keys between the committed pin and a freshly
/// derived map. Stores the keys (not just counts) so callers can both log a
/// summary and enumerate the per-part lines.
pub struct RbCrossRefDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

impl RbCrossRefDiff {
    /// Compute the diff of `new` against `old` (the committed pin's parts).
    pub fn of(
        old: &BTreeMap<String, RbPartCrossRefs>,
        new: &BTreeMap<String, RbPartCrossRefs>,
    ) -> Self {
        let added = new
            .keys()
            .filter(|k| !old.contains_key(*k))
            .cloned()
            .collect();
        let removed = old
            .keys()
            .filter(|k| !new.contains_key(*k))
            .cloned()
            .collect();
        let changed = new
            .iter()
            .filter(|(k, v)| old.get(*k).is_some_and(|prev| prev != *v))
            .map(|(k, _)| k.clone())
            .collect();
        Self {
            added,
            removed,
            changed,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }

    /// Log a one-line summary at `info`, and the per-part keys at `debug`.
    pub fn log(&self) {
        for k in &self.added {
            tracing::debug!("+ {k}");
        }
        for k in &self.changed {
            tracing::debug!("~ {k}");
        }
        for k in &self.removed {
            tracing::debug!("- {k}");
        }
        tracing::info!(
            "diff vs committed pin: +{} added, -{} removed, ~{} changed",
            self.added.len(),
            self.removed.len(),
            self.changed.len(),
        );
    }
}

const HEADER: &str = "\
// Rebrickable part_num -> external-id cross-references.
//
// GENERATED by `just refresh-part-mappings` from the Rebrickable parts API.
// `ldraw` is the canonical LDraw design_id (-fN flexion variants collapsed);
// every other system is the raw id list AS REBRICKABLE REPORTS IT (a
// BrickLink id here is Rebrickable's cross-ref, not authoritative BrickLink).
//
// Do not edit by hand. Regenerate explicitly when adopting a newer catalog
// snapshot — it is never produced by the routine `build`.
//
// See docs/lego-reference/ldraw-part-numbering.md for the -fN collapse and
// anomaly rules.

";
