//! Schema-v2 cross-reference slices (#12): the `colors` table (from the
//! compiled-in color reference) and `rb_part_external_id` (from the
//! committed cross-ref pin). Together they make `catalog.sqlite` the single
//! self-sufficient cleaned artifact — before v2, the color reference lived
//! only inside this binary and the BrickLink/BrickOwl/LEGO id strings only
//! reached the DB as the lossy `part_fts.alt_ids` text bag (the studkit
//! `gen-data` xref consumer needs both structured; bkfunk/studkit#41).

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::build::resolve::PartResolver;
use crate::core::colors::color_reference;
use crate::refresh_parts::RbCrossRefPin;

/// Write the `colors` table from the compiled-in color reference. `aliases`
/// is a JSON array string — a scalar per row keeps the table
/// `WITHOUT ROWID`-friendly and directly readable in Datasette/sqlite3.
pub(crate) fn build_colors(conn: &Connection) -> Result<usize> {
    conn.execute(
        "CREATE TABLE colors (
            ldraw_code       INTEGER PRIMARY KEY,
            rb_color_id      INTEGER,
            name_lego        TEXT,
            name_bricklink   TEXT,
            name_rebrickable TEXT,
            aliases          TEXT NOT NULL
        ) WITHOUT ROWID",
        [],
    )
    .context("create colors table")?;
    conn.execute(
        "CREATE INDEX colors_rb_color_id ON colors (rb_color_id)",
        [],
    )
    .context("index colors")?;

    let mut insert = conn.prepare(
        "INSERT INTO colors (ldraw_code, rb_color_id, name_lego, name_bricklink,
                             name_rebrickable, aliases)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut rows = 0usize;
    for entry in color_reference().entries() {
        let aliases = serde_json::to_string(&entry.names.aliases)
            .context("serialize color aliases to JSON")?;
        insert
            .execute(rusqlite::params![
                entry.ldraw_code,
                entry.rb_color_id,
                entry.names.lego,
                entry.names.bricklink,
                entry.names.rebrickable,
                aliases,
            ])
            .with_context(|| format!("insert color {}", entry.ldraw_code))?;
        rows += 1;
    }
    Ok(rows)
}

/// Counters for the external-id ingest, stamped into `meta` like the other
/// slices' stats.
pub(crate) struct ExternalIdStats {
    /// Rows written.
    pub rows: usize,
    /// Rows whose `part_num` resolved to an in-catalog `design_id`.
    pub resolved: usize,
}

impl ExternalIdStats {
    pub(crate) fn meta_rows(&self) -> [(&'static str, String); 2] {
        [
            ("rb_part_external_id_count", self.rows.to_string()),
            ("rb_part_external_id_resolved", self.resolved.to_string()),
        ]
    }
}

/// Write `rb_part_external_id` from the cross-ref pin: one row per
/// `(part_num, system, external_id)`, with the LDraw `design_id` resolved
/// through the shared ladder (membership-gated like the relationships
/// slice — NULL when the part isn't in the scanned library). The pin's ids
/// are second-hand via the Rebrickable API, not authoritative — see
/// LICENSES/REBRICKABLE.md.
pub(crate) fn build_external_ids(
    conn: &Connection,
    pin: &RbCrossRefPin,
    resolver: &PartResolver,
) -> Result<ExternalIdStats> {
    conn.execute(
        "CREATE TABLE rb_part_external_id (
            part_num    TEXT NOT NULL,
            system      TEXT NOT NULL,
            external_id TEXT NOT NULL,
            design_id   TEXT,
            PRIMARY KEY (part_num, system, external_id)
        ) WITHOUT ROWID",
        [],
    )
    .context("create rb_part_external_id table")?;
    conn.execute(
        "CREATE INDEX rb_part_external_id_design ON rb_part_external_id (design_id)",
        [],
    )
    .context("index rb_part_external_id")?;

    let mut insert = conn.prepare(
        "INSERT INTO rb_part_external_id (part_num, system, external_id, design_id)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    let mut stats = ExternalIdStats {
        rows: 0,
        resolved: 0,
    };
    for (part_num, xrefs) in &pin.parts {
        let design_id = resolver
            .resolve_in_catalog(part_num)
            .map(|r| r.design_id.to_string());
        for (system, ids) in &xrefs.external_ids {
            for external_id in ids {
                insert
                    .execute(rusqlite::params![part_num, system, external_id, design_id])
                    .with_context(|| {
                        format!("insert external id {system}:{external_id} for {part_num}")
                    })?;
                stats.rows += 1;
                if design_id.is_some() {
                    stats.resolved += 1;
                }
            }
        }
    }
    Ok(stats)
}
