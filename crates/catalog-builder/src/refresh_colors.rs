//! `refresh-color-names` subcommand — pulls the Rebrickable colors
//! listing, caches it as a JSON snapshot, and regenerates the runtime
//! color reference from `snapshot ∖ excludes`.
//!
//! ## Pipeline
//!
//! ```text
//!         GET /api/v3/lego/colors/?inc_external_ids=1
//!                              │
//!                              ▼
//!     data/rebrickable/colors.json    ← cached API response (normalized)
//!                              │
//!                              ▼  apply color_excludes.ron
//!                              │
//!                              ▼
//!     crates/catalog-builder/src/core/color_names.ron   ← generated runtime ref
//! ```
//!
//! The snapshot is the source of truth for "what Rebrickable says about
//! colors right now" — every `results` row from the API listing, preserving
//! every external id and historical name string across every system
//! Rebrickable cross-refs.
//!
//! It is *normalized*, not byte-for-byte verbatim: when the listing
//! endpoint paginates, `fetch_listing` concatenates the per-page
//! `results` arrays into one and drops the per-page pagination fields
//! (`next`, `previous`, the per-page `count`). The on-disk shape is
//! `{"count": <total>, "results": [...]}` — semantically equivalent to a
//! hypothetical single-page response. The `count` written is the count
//! of usable results we ended up with, which equals the API's
//! top-level `count` when all results fit in one page (the normal case
//! at `page_size=1000`).
//!
//! The generated `color_names.ron` is a runtime-shaped derived view: one
//! row per color, canonical names per system, plus `aliases` for search.
//!
//! ## Idempotent refresh
//!
//! Both write steps skip the rewrite when the upstream content is
//! unchanged:
//!
//! - **Snapshot**: parsed as `serde_json::Value` and compared
//!   semantically (key order doesn't matter, only the data). No spurious
//!   diffs when Rebrickable's serializer reorders fields between
//!   requests.
//! - **`color_names.ron`**: rendered, then byte-compared to what's on
//!   disk. We control the formatter so byte-equality and semantic
//!   equality coincide here.
//!
//! A re-run with unchanged upstream produces zero file writes and one
//! log line per stage saying so.
//!
//! ## Example response shape
//!
//! Trimmed-to-essentials sample of one color row from the API:
//!
//! ```json
//! {
//!   "id": 14,
//!   "name": "Yellow",
//!   "external_ids": {
//!     "BrickLink": { "ext_ids": [3],  "ext_descrs": [["Yellow"]] },
//!     "LEGO":      { "ext_ids": [24], "ext_descrs": [["Bright yellow", "BR.YEL", "BRIGHT YELLOW, VERSION 2"]] },
//!     "LDraw":     { "ext_ids": [14], "ext_descrs": [["Yellow"]] },
//!     "BrickOwl":  { "ext_ids": [93], "ext_descrs": [["Yellow"]] },
//!     "Peeron":    { "ext_ids": [null], "ext_descrs": [["yellow"]] }
//!   }
//! }
//! ```
//!
//! From this one row, the generated `color_names.ron` entry is:
//!
//! ```ron
//! (ldraw_code: 14, rb_color_id: 14, names: (
//!     lego: "Bright yellow",                              // first ext_descrs[0][0] from LEGO
//!     bricklink: "Yellow",                                //                            BrickLink
//!     rebrickable: "Yellow",                              // the top-level `name`
//!     aliases: ["BR.YEL", "BRIGHT YELLOW, VERSION 2", "yellow"],
//! ))
//! ```
//!
//! Aliases is a deduplicated bag of every additional name string across
//! every system — the secondary `"BR.YEL"` and `"BRIGHT YELLOW, VERSION 2"`
//! from LEGO's three-element descriptor, historical names from older
//! `ext_descrs` rows, and names from systems we don't surface as named
//! fields (Peeron's lowercase `"yellow"`, anything BrickOwl might
//! contribute). It's what makes "search for 'BR.YEL'" find Yellow.
//!
//! Shape quirks in Rebrickable's response — `id` can be negative
//! (`-1` = [Unknown]), `ext_ids` is a mix of JSON integers and strings,
//! and `ext_descrs` rows can be `[name]`, `[name, source]`, or `[name,
//! source1, source2, …]` — are absorbed by parsing those fields as raw
//! [`serde_json::Value`] arrays.
//!
//! ## Auth
//!
//! Requires a Rebrickable API key. Passed via `--api-key` or the
//! `REBRICKABLE_API_KEY` environment variable. Header sent is
//! `Authorization: key <key>` (Rebrickable's scheme, not `Bearer`).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use crate::core::colors::{ColorNames, ColorRefEntry};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::util::{atomic_write, ron_quote, workspace_root};

/// Rebrickable listing endpoint. `page_size=1000` exceeds the ~250
/// LEGO color count, so the listing comes in one request in practice;
/// `inc_external_ids=1` attaches the BrickLink / LEGO / LDraw cross-refs.
const COLORS_LISTING_URL: &str =
    "https://rebrickable.com/api/v3/lego/colors/?page_size=1000&inc_external_ids=1";

/// Polite delay between pages, on the off chance Rebrickable splits the
/// listing into multiple pages.
const REQUEST_INTERVAL: Duration = Duration::from_millis(250);

pub fn run(api_key: &str, dry_run: bool) -> Result<()> {
    let ws = workspace_root();
    let snapshot_path = ws.join("data/rebrickable/colors.json");
    let excludes_path = ws.join("data/rebrickable/color_excludes.ron");
    let ron_path = ws.join("crates/catalog-builder/src/core/color_names.ron");

    // ── 1. Fetch full API listing ────────────────────────────────────
    let response = fetch_listing(api_key)?;
    let snapshot_text =
        serde_json::to_string_pretty(&response).context("serialize fetched response")? + "\n";

    // ── 2. Compare to existing snapshot (semantic, not byte) ────────
    let snapshot_changed = match std::fs::read(&snapshot_path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(prev) => prev != response,
            // Existing file unparseable — treat as changed so we overwrite
            // with a clean version.
            Err(_) => true,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(e).context("read existing colors.json"),
    };
    if snapshot_changed {
        if dry_run {
            tracing::info!("colors snapshot WOULD be updated (--dry-run, no write)");
        } else {
            atomic_write(&snapshot_path, snapshot_text.as_bytes())?;
            tracing::info!("colors snapshot updated: {}", snapshot_path.display());
        }
    } else {
        tracing::info!("upstream unchanged — colors.json not rewritten");
    }

    // ── 3. Load excludes + parse colors ──────────────────────────────
    let excludes = load_excludes(&excludes_path)?;
    let api_colors = parse_colors(&response)?;
    let total = api_colors.len();
    let entries = generate_entries(&api_colors, &excludes);
    tracing::info!(
        "generated {} entries from {} colors ({} excluded)",
        entries.len(),
        total,
        total - entries.len(),
    );

    // ── 4. Render + compare RON ──────────────────────────────────────
    let new_ron = render_ron(&entries);
    // Round-trip-check before touching the destination: catches any
    // formatter bug that would produce RON our own parser can't read.
    let _: Vec<ColorRefEntry> = ron::from_str(&new_ron)
        .context("generated RON did not round-trip through ron::from_str (formatter bug)")?;
    // Only treat a *missing* file as "no existing content"; any other
    // read error (permissions, IO failure, encoding issue) needs to
    // surface — we'd otherwise treat the file as empty, conclude
    // "changed", and try to overwrite it, masking a real problem.
    let existing_ron = match std::fs::read_to_string(&ron_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("read existing {}", ron_path.display()));
        }
    };
    let ron_changed = existing_ron != new_ron;
    if ron_changed {
        if dry_run {
            tracing::info!("color_names.ron WOULD be regenerated (--dry-run, no write)");
        } else {
            atomic_write(&ron_path, new_ron.as_bytes())?;
            tracing::info!("color_names.ron regenerated: {}", ron_path.display());
        }
    } else {
        tracing::info!("generated color_names.ron unchanged — not rewritten");
    }

    if !snapshot_changed && !ron_changed {
        tracing::info!("no changes.");
    }
    Ok(())
}

// ─── HTTP ───────────────────────────────────────────────────────────────

fn fetch_listing(api_key: &str) -> Result<Value> {
    // The listing endpoint paginates if results exceed page_size, so
    // collect across pages. In practice page_size=1000 returns the
    // full ~250-color list in one request, but pagination is cheap to
    // support.
    let mut all_results: Vec<Value> = Vec::new();
    let mut url = String::from(COLORS_LISTING_URL);
    let mut page = 1;
    loop {
        tracing::info!("GET colors page {page}");
        tracing::debug!("  url: {url}");
        let resp = ureq::get(&url)
            .set("Authorization", &format!("key {api_key}"))
            .set("Accept", "application/json")
            .call()
            .with_context(|| format!("GET {url}"))?;
        let body: Value = resp
            .into_json()
            .with_context(|| format!("parse JSON from {url}"))?;
        let results = body
            .get("results")
            .and_then(|v| v.as_array())
            .with_context(|| format!("response from {url} missing `results` array"))?;
        all_results.extend(results.iter().cloned());
        let next = body.get("next").and_then(|v| v.as_str()).map(String::from);
        tracing::info!(
            "  page {page}: {} colors (total {})",
            results.len(),
            all_results.len(),
        );
        match next {
            Some(n) => {
                url = n;
                page += 1;
                sleep(REQUEST_INTERVAL);
            }
            None => break,
        }
    }
    // Stitch all pages back into a single response object that mirrors
    // Rebrickable's listing shape minus the pagination fields. This is
    // what gets cached — semantically equivalent to a hypothetical
    // single-page response with all results.
    Ok(serde_json::json!({
        "count": all_results.len(),
        "results": all_results,
    }))
}

// ─── Typed view of the API ──────────────────────────────────────────────

/// One color row, only the fields we consume to generate `color_names.ron`.
/// Everything else (rgb, is_trans, BrickOwl/Peeron name strings) stays in
/// the cached snapshot for downstream consumers (catalog builder,
/// future export-to-BrickLink, etc.) to read directly.
#[derive(Deserialize, Debug)]
struct ApiColor {
    /// `i32` because Rebrickable uses negative ids for sentinels
    /// (`-1` = "[Unknown]"). Filtered downstream via `color_excludes.ron`.
    id: i32,
    name: String,
    #[serde(default)]
    external_ids: ApiExternalIds,
}

#[derive(Deserialize, Debug, Default)]
struct ApiExternalIds {
    #[serde(default, rename = "BrickLink")]
    bricklink: Option<ApiExtIds>,
    #[serde(default, rename = "LEGO")]
    lego: Option<ApiExtIds>,
    #[serde(default, rename = "LDraw")]
    ldraw: Option<ApiExtIds>,
    /// Unknown systems get captured here as raw values so their name
    /// strings can still be folded into the `aliases` bag for search.
    #[serde(flatten)]
    other: BTreeMap<String, ApiExtIds>,
}

#[derive(Deserialize, Debug, Default)]
struct ApiExtIds {
    /// JSON integers and JSON strings both show up here historically;
    /// we project at the consumer rather than constraining at parse time.
    #[serde(default)]
    ext_ids: Vec<Value>,
    /// Variable-length arrays: `[name]`, `[name, source]`, `[name,
    /// source1, source2, …]`. Kept as raw `Value` and walked at the
    /// consumer.
    #[serde(default)]
    ext_descrs: Vec<Value>,
}

impl ApiExtIds {
    /// First descriptor name — the canonical current name in this system.
    fn canonical_name(&self) -> Option<String> {
        extract_string(self.ext_descrs.first()?, 0)
    }

    /// Every additional name string this system carries (descriptors past
    /// the first, plus secondary names within each descriptor row).
    fn alias_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for (i, descr) in self.ext_descrs.iter().enumerate() {
            let Some(arr) = descr.as_array() else {
                continue;
            };
            // Every string element of the descriptor is a name candidate.
            // For the first descriptor, skip index 0 (it's the canonical
            // name, already captured separately).
            let start = if i == 0 { 1 } else { 0 };
            for (j, v) in arr.iter().enumerate() {
                if j < start {
                    continue;
                }
                // Some Rebrickable rows embed a sources array (`["BL"]`)
                // as the second element. Skip nested arrays; we only want
                // the bare name strings.
                if let Some(s) = v.as_str() {
                    names.push(s.to_string());
                }
            }
        }
        names
    }

    /// LDraw external ids parsed as `u32`, skipping non-numeric entries.
    fn u32_ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.ext_ids.iter().filter_map(parse_u32_value)
    }
}

/// Get the i-th element of a JSON array as a string, if both the array
/// access and the cast succeed.
fn extract_string(v: &Value, i: usize) -> Option<String> {
    v.as_array()?.get(i)?.as_str().map(String::from)
}

/// Accept a Rebrickable external id as either a JSON integer or a JSON
/// string, projecting to `u32`. Anything else returns `None`.
fn parse_u32_value(v: &Value) -> Option<u32> {
    v.as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn parse_colors(response: &Value) -> Result<Vec<ApiColor>> {
    let results = response
        .get("results")
        .and_then(|v| v.as_array())
        .context("response missing `results` array")?;
    let mut out = Vec::with_capacity(results.len());
    for (i, raw) in results.iter().enumerate() {
        let color: ApiColor =
            serde_json::from_value(raw.clone()).with_context(|| format!("parse results[{i}]"))?;
        out.push(color);
    }
    Ok(out)
}

// ─── Excludes ───────────────────────────────────────────────────────────

fn load_excludes(path: &Path) -> Result<HashSet<i32>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let ids: Vec<i32> =
        ron::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(ids.into_iter().collect())
}

// ─── Generation ─────────────────────────────────────────────────────────

fn generate_entries(api_colors: &[ApiColor], excludes: &HashSet<i32>) -> Vec<ColorRefEntry> {
    let mut entries = Vec::new();
    for color in api_colors {
        if excludes.contains(&color.id) {
            continue;
        }
        let Some(entry) = entry_for(color) else {
            // A color with no LDraw cross-ref has no `ldraw_code` to
            // anchor the entry on. Skip — the catalog only knows how to
            // talk about colors with an LDraw equivalent.
            tracing::debug!(
                "rb_color_id {} (\"{}\"): no LDraw cross-ref, skipping",
                color.id,
                color.name
            );
            continue;
        };
        entries.push(entry);
    }
    entries.sort_unstable_by_key(|e| e.ldraw_code);
    entries
}

fn entry_for(color: &ApiColor) -> Option<ColorRefEntry> {
    // The LDraw code is the cross-source key. If Rebrickable's row has
    // multiple LDraw cross-refs (sentinels do; ordinary colors don't),
    // we take the first as the primary mapping.
    let ldraw_code = color.external_ids.ldraw.as_ref()?.u32_ids().next()?;
    let rb_color_id = u32::try_from(color.id).ok();

    let lego = color
        .external_ids
        .lego
        .as_ref()
        .and_then(|x| x.canonical_name());
    let bricklink = color
        .external_ids
        .bricklink
        .as_ref()
        .and_then(|x| x.canonical_name());

    // Build the set of canonical names so we can deduplicate them out
    // of the aliases bag.
    let canonical: BTreeSet<&str> = [
        Some(color.name.as_str()),
        lego.as_deref(),
        bricklink.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();

    // Collect every other name string Rebrickable knows for this color,
    // across every system. Deduplicated and sorted for stable diffs.
    let mut aliases: BTreeSet<String> = BTreeSet::new();
    let systems = [
        color.external_ids.lego.as_ref(),
        color.external_ids.bricklink.as_ref(),
        color.external_ids.ldraw.as_ref(),
    ]
    .into_iter()
    .flatten()
    .chain(color.external_ids.other.values());
    for system in systems {
        for name in system.alias_names() {
            if !canonical.contains(name.as_str()) {
                aliases.insert(name);
            }
        }
        // LDraw's / unknown systems' canonical name isn't a separate
        // field on ColorNames, but the string is still useful for
        // search — fold it into aliases too.
        if let Some(name) = system.canonical_name()
            && !canonical.contains(name.as_str())
        {
            aliases.insert(name);
        }
    }

    Some(ColorRefEntry {
        ldraw_code,
        rb_color_id,
        names: ColorNames {
            lego,
            bricklink,
            rebrickable: Some(color.name.clone()),
            aliases: aliases.into_iter().collect(),
        },
    })
}

// ─── RON rendering ──────────────────────────────────────────────────────

/// Render a list of entries as a `color_names.ron` file body. Uses the
/// same single-line-per-entry style the file was hand-curated in, with
/// `implicit_some` shorthand (`bricklink: "Red"`) and `None` fields
/// omitted. Deterministic — the same entries in the same order produce
/// byte-identical output, which is what makes the "skip rewrite if
/// unchanged" check in `run` work via plain byte equality.
///
/// ## Why hand-rolled and not `ron::ser::to_string_pretty`
///
/// `ron::ser::to_string_pretty` would produce, per row:
///
/// ```ron
/// (
///     ldraw_code: 4,
///     rb_color_id: Some(4),
///     names: (
///         lego: None,
///         bricklink: Some("Red"),
///         rebrickable: Some("Red"),
///         aliases: [],
///     ),
/// ),
/// ```
///
/// vs. what this formatter produces:
///
/// ```ron
/// (ldraw_code: 4, rb_color_id: 4, names: (bricklink: "Red", rebrickable: "Red")),
/// ```
///
/// Two losses with the default serializer would matter at every refresh:
///
/// 1. **Diff readability.** A refresh that changes 50 name variants out
///    of 250 produces a ~500-line diff of mostly whitespace under the
///    default; the curated single-line form produces a ~50-line
///    one-line-per-changed-row diff. The whole reason `color_names.ron`
///    is committed (rather than generated at build time) is so the
///    refresh diff is reviewable in a PR — that requires a tight format.
/// 2. **`implicit_some` shorthand.** `Extensions::IMPLICIT_SOME` is a
///    *parser* extension in RON; the writer always emits `Some(...)` /
///    `None`. Hand-rolling lets us match the parser-friendly shorthand
///    the curated file uses.
///
/// The hand-rolled formatter is ~25 LOC, lives next to its only use, is
/// round-trip-checked through `ron::from_str` before write (so a
/// formatter bug fails loudly rather than corrupting the file), and
/// enforces the sort-by-`ldraw_code` and omit-`None`-fields rules the
/// default serializer would clobber.
fn render_ron(entries: &[ColorRefEntry]) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    out.push_str("[\n");
    for e in entries {
        out.push_str(&format_entry(e));
        out.push_str(",\n");
    }
    out.push_str("]\n");
    out
}

/// File header — generated, but stable. The `implicit_some` directive
/// must remain so the shorthand (`bricklink: "Red"`) parses.
const HEADER: &str = "\
// LEGO color name variants per LDraw color code.
//
// GENERATED by `just refresh-color-names` from
//   data/rebrickable/colors.json
// ∖ color_excludes.ron
//
// Do not edit by hand. To exclude a color, add its `rb_color_id` to
// `color_excludes.ron` and re-run `just refresh-color-names`.
//
// `ldraw_code` is the canonical LDraw color id (the cross-source key).
// `rb_color_id` is the Rebrickable id used at build time to translate
// elements.csv / inventory_parts.csv color refs into LDraw ColorId — the
// catalog DB ships only LDraw ColorIds. Absent name fields mean
// \"unknown\"; `Palette::load` leaves them as None and the frontend falls
// back to the LDraw name. `aliases` carries every additional name
// Rebrickable knows for this color (across systems and history), for
// search across any spelling a user might type.
#![enable(implicit_some)]

";

fn format_entry(e: &ColorRefEntry) -> String {
    let mut parts = vec![format!("ldraw_code: {}", e.ldraw_code)];
    if let Some(rb) = e.rb_color_id {
        parts.push(format!("rb_color_id: {rb}"));
    }
    parts.push(format!("names: {}", format_names(&e.names)));
    format!("    ({})", parts.join(", "))
}

fn format_names(n: &ColorNames) -> String {
    let mut parts = Vec::new();
    if let Some(s) = &n.lego {
        parts.push(format!("lego: {}", ron_quote(s)));
    }
    if let Some(s) = &n.bricklink {
        parts.push(format!("bricklink: {}", ron_quote(s)));
    }
    if let Some(s) = &n.rebrickable {
        parts.push(format!("rebrickable: {}", ron_quote(s)));
    }
    if !n.aliases.is_empty() {
        let quoted: Vec<String> = n.aliases.iter().map(|s| ron_quote(s)).collect();
        parts.push(format!("aliases: [{}]", quoted.join(", ")));
    }
    format!("({})", parts.join(", "))
}

// ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 20-color slice of an actual `/api/v3/lego/colors/` response.
    /// Drives the integration-style tests that prove the full pipeline
    /// (parse → exclude → generate → render → parse) handles every shape
    /// variant Rebrickable ships.
    const FIXTURE_PAGE: &str = include_str!("../tests/fixtures/api/colors-page-20.json");

    fn fixture_response() -> Value {
        serde_json::from_str(FIXTURE_PAGE).expect("parse fixture page")
    }

    fn default_excludes() -> HashSet<i32> {
        // Same as color_excludes.ron ships with.
        let mut s = HashSet::new();
        s.insert(-1);
        s
    }

    #[test]
    fn parses_full_fixture_response() {
        let response = fixture_response();
        let colors = parse_colors(&response).expect("parse");
        assert_eq!(colors.len(), 20);
        // Sentinel preserved at parse boundary — filtered later by excludes.
        assert!(colors.iter().any(|c| c.id == -1));
    }

    #[test]
    fn generate_drops_excluded_ids() {
        let response = fixture_response();
        let colors = parse_colors(&response).expect("parse");
        let entries = generate_entries(&colors, &default_excludes());
        // 20 raw - 1 sentinel = 19 generated.
        assert_eq!(entries.len(), 19);
    }

    #[test]
    fn generate_picks_canonical_names_from_each_system() {
        let response = fixture_response();
        let colors = parse_colors(&response).expect("parse");
        let entries = generate_entries(&colors, &default_excludes());
        // id=4 "Red": LDraw 4, LEGO canonical "Bright red", BL "Red".
        let red = entries.iter().find(|e| e.ldraw_code == 4).expect("red");
        assert_eq!(red.rb_color_id, Some(4));
        assert_eq!(red.names.lego.as_deref(), Some("Bright red"));
        assert_eq!(red.names.bricklink.as_deref(), Some("Red"));
        assert_eq!(red.names.rebrickable.as_deref(), Some("Red"));
    }

    #[test]
    fn generate_collects_aliases_across_systems() {
        // id=14 "Yellow": LEGO's descriptors are
        // [["Bright yellow", "BR.YEL", "BRIGHT YELLOW, VERSION 2"]].
        // Canonical = "Bright yellow"; "BR.YEL" + "BRIGHT YELLOW, VERSION 2"
        // become aliases.
        let response = fixture_response();
        let colors = parse_colors(&response).expect("parse");
        let entries = generate_entries(&colors, &default_excludes());
        let yellow = entries.iter().find(|e| e.ldraw_code == 14).expect("yellow");
        assert_eq!(yellow.names.lego.as_deref(), Some("Bright yellow"));
        assert!(yellow.names.aliases.contains(&"BR.YEL".to_string()));
        assert!(
            yellow
                .names
                .aliases
                .contains(&"BRIGHT YELLOW, VERSION 2".to_string())
        );
    }

    #[test]
    fn generate_aliases_exclude_canonical_names() {
        // id=0 "Black": LEGO ext_descrs[0] = ["Black", "BLACK"].
        // Canonical lego name = "Black", which equals the rebrickable
        // name too. The aliases bag should NOT contain "Black" (it's a
        // canonical), but should include "BLACK" (secondary string).
        let response = fixture_response();
        let colors = parse_colors(&response).expect("parse");
        let entries = generate_entries(&colors, &default_excludes());
        let black = entries.iter().find(|e| e.ldraw_code == 0).expect("black");
        assert!(
            !black.names.aliases.contains(&"Black".to_string()),
            "canonical name 'Black' must not appear in aliases"
        );
        assert!(
            black.names.aliases.contains(&"BLACK".to_string()),
            "secondary 'BLACK' should appear in aliases"
        );
    }

    #[test]
    fn generate_aliases_include_other_system_names() {
        // BrickOwl / Peeron live in `other` (not surfaced as a struct
        // field), but their name strings still feed `aliases` for search.
        let response = fixture_response();
        let colors = parse_colors(&response).expect("parse");
        let entries = generate_entries(&colors, &default_excludes());
        // id=3 "Dark Turquoise" — Peeron has "teal" as a name. Aliases
        // should pick it up so users typing "teal" find this color.
        let teal = entries.iter().find(|e| e.ldraw_code == 3).expect("teal");
        assert!(teal.names.aliases.contains(&"teal".to_string()));
    }

    #[test]
    fn ldraw_canonical_name_makes_it_into_aliases() {
        // LDraw's canonical name isn't a separate field on ColorNames
        // (the runtime already has the LDraw name on PaletteColor.name
        // from LDConfig.ldr), but the string is still search-useful —
        // the LDraw form often uses underscores ("Light_Bluish_Gray")
        // which a user might type. Should appear in aliases.
        let response = fixture_response();
        let colors = parse_colors(&response).expect("parse");
        let entries = generate_entries(&colors, &default_excludes());
        // id=3 LDraw descriptor is ["Dark_Turquoise"] — should be in aliases.
        let teal = entries.iter().find(|e| e.ldraw_code == 3).expect("teal");
        assert!(teal.names.aliases.contains(&"Dark_Turquoise".to_string()));
    }

    #[test]
    fn parse_u32_value_handles_mixed_inputs() {
        use serde_json::json;
        assert_eq!(parse_u32_value(&json!(42)), Some(42));
        assert_eq!(parse_u32_value(&json!("42")), Some(42));
        assert_eq!(parse_u32_value(&json!("not a number")), None);
        assert_eq!(parse_u32_value(&json!(-1)), None);
        assert_eq!(parse_u32_value(&json!(null)), None);
        assert_eq!(parse_u32_value(&json!(1u64 << 33)), None);
    }

    #[test]
    fn extract_string_handles_short_and_typed_arrays() {
        use serde_json::json;
        assert_eq!(extract_string(&json!(["a"]), 0), Some("a".into()));
        assert_eq!(extract_string(&json!(["a", "b"]), 1), Some("b".into()));
        assert_eq!(extract_string(&json!(["a"]), 5), None);
        assert_eq!(extract_string(&json!([42]), 0), None);
        assert_eq!(extract_string(&json!("a"), 0), None);
    }

    #[test]
    fn rendered_ron_round_trips_through_parser() {
        let entries = vec![
            ColorRefEntry {
                ldraw_code: 4,
                rb_color_id: Some(4),
                names: ColorNames {
                    lego: Some("Bright red".into()),
                    bricklink: Some("Red".into()),
                    rebrickable: Some("Red".into()),
                    aliases: vec!["BR.RED".into()],
                },
            },
            ColorRefEntry {
                ldraw_code: 14,
                rb_color_id: Some(14),
                names: ColorNames {
                    lego: Some("Bright yellow".into()),
                    bricklink: Some("Yellow".into()),
                    rebrickable: Some("Yellow".into()),
                    aliases: vec!["BR.YEL".into(), "BRIGHT YELLOW, VERSION 2".into()],
                },
            },
        ];
        let text = render_ron(&entries);
        let parsed: Vec<ColorRefEntry> = ron::from_str(&text).expect("parse generated RON");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].names.aliases, vec!["BR.RED"]);
        assert_eq!(
            parsed[1].names.aliases,
            vec!["BR.YEL", "BRIGHT YELLOW, VERSION 2"],
        );
    }

    #[test]
    fn rendered_ron_is_byte_stable_across_runs() {
        // Determinism is what makes the "skip rewrite if unchanged"
        // logic work — same entries in same order must produce
        // byte-identical text.
        let entries = vec![ColorRefEntry {
            ldraw_code: 4,
            rb_color_id: Some(4),
            names: ColorNames {
                lego: Some("Bright red".into()),
                bricklink: Some("Red".into()),
                rebrickable: Some("Red".into()),
                aliases: vec!["BR.RED".into()],
            },
        }];
        let a = render_ron(&entries);
        let b = render_ron(&entries);
        assert_eq!(a, b);
    }

    #[test]
    fn full_pipeline_against_fixture_produces_round_trip_ron() {
        let response = fixture_response();
        let colors = parse_colors(&response).expect("parse");
        let entries = generate_entries(&colors, &default_excludes());
        let text = render_ron(&entries);
        let parsed: Vec<ColorRefEntry> = ron::from_str(&text).expect("round trip");
        assert_eq!(parsed.len(), 19);
        let red = parsed.iter().find(|e| e.ldraw_code == 4).expect("red");
        assert_eq!(red.names.rebrickable.as_deref(), Some("Red"));
        assert!(!red.names.aliases.is_empty());
    }

    #[test]
    fn semantic_value_eq_skips_byte_level_differences() {
        // Demonstrate the property the "skip rewrite if unchanged"
        // logic depends on: Value::eq is semantic, not byte-level.
        let a: Value = serde_json::from_str(r#"{"id": 4, "name": "Red"}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"name": "Red", "id": 4}"#).unwrap();
        assert_eq!(a, b);
    }
}
