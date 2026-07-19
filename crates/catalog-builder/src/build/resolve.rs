//! Shared `part_num → LDraw design_id` translation for the build slices
//! (#112).
//!
//! The cross-ref pin is the authoritative mapping, but Rebrickable's API
//! omits `external_ids.LDraw` for many extremely common parts (Plate 1×1
//! Round, Brick 1×1, …) whose part number **literally equals** an LDraw
//! design id. Pin-only translation dropped ~125k `inventory_parts` rows for
//! 189 such parts, leaving them with no popularity data at all.
//!
//! [`PartResolver::translate`] therefore resolves in three steps:
//!
//! 1. **Pin** — the committed `part_crossrefs.ron` mapping, verbatim. Not
//!    gated on library membership: an API-vouched mapping to a design our
//!    library lacks still aggregates under that design (invisible in the
//!    `part` view until the library grows it).
//! 2. **Literal fallback** — when the pin is silent, the canonicalized
//!    part number (`-fN` collapsed) is accepted *only if* `ldraw_part`
//!    contains it. Library membership is what makes a bare string equality
//!    trustworthy as a cross-reference.
//! 3. **Redirect chase** — when either step lands on a design that is
//!    *not* in `ldraw_part`, the union of the `ldraw_moved_to` (tombstone)
//!    and `ldraw_alias` (hard alias) hop maps is followed (chains flattened
//!    at construction) to the design that actually carries the geometry.
//!    This covers ids the two ecosystems canonicalized in opposite
//!    directions — RB's canonical `4073` is an LDraw `~Moved to 6141`
//!    tombstone, RB's `30071` is an LDraw hard alias of `3005` — and pin
//!    entries that predate an LDraw rename.

use std::collections::{HashMap, HashSet};

use crate::core::canonical_design_id;
use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::refresh_parts::RbCrossRefPin;

/// A successful translation: the design id plus which steps produced it, so
/// call sites can count literal fallbacks and redirect chases for `meta`
/// observability. The two flags are orthogonal: `via_redirect` marks a chase
/// applied on top of either source (pin or literal). Tombstone and alias
/// hops are not distinguished — a flattened chain may mix them.
pub(crate) struct Resolved<'a> {
    pub design_id: &'a str,
    pub via_literal: bool,
    pub via_redirect: bool,
}

pub(crate) struct PartResolver<'a> {
    /// part_num → design_id from the committed pin.
    pin_map: HashMap<&'a str, &'a str>,
    /// Every `design_id` in `ldraw_part` — what the literal fallback (and
    /// [`Self::resolve_in_catalog`]) checks membership against. Loaded once
    /// so per-row translation never queries.
    catalog: HashSet<String>,
    /// Redirected design id → the **in-catalog** design that carries the
    /// geometry: the union of the `ldraw_moved_to` and `ldraw_alias` hops,
    /// flattened at construction (chains followed to a catalogued end; dead
    /// or cyclic chains dropped), so a translate-time chase is one lookup.
    /// The two hop kinds are disjoint by construction (one `.dat` per id).
    redirects: HashMap<String, String>,
}

/// Chains longer than this are treated as dead rather than followed — real
/// LDraw rename/alias chains are 1–2 hops; anything deeper is data
/// corruption or a cycle.
const REDIRECT_MAX_HOPS: usize = 8;

impl<'a> PartResolver<'a> {
    /// Build the resolver from the loaded pin and the already-populated
    /// `ldraw_part` + `ldraw_moved_to` + `ldraw_alias` tables (so this must
    /// run after the library scan).
    pub(crate) fn new(conn: &Connection, rb_cross_ref_pin: &'a RbCrossRefPin) -> Result<Self> {
        let catalog = load_column_set(conn, "SELECT design_id FROM ldraw_part")?;
        let mut hops = load_hops(conn, "ldraw_moved_to")?;
        hops.extend(load_hops(conn, "ldraw_alias")?);
        let redirects = flatten_redirects(&hops, &catalog);
        Ok(PartResolver {
            pin_map: rb_cross_ref_pin.part_to_ldraw(),
            catalog,
            redirects,
        })
    }

    /// Translate a Rebrickable part number to an LDraw design id: pin first,
    /// literal-if-in-catalog second, with the redirect chase applied to
    /// either result that isn't in the catalog (see the module doc for why
    /// the steps gate differently).
    pub(crate) fn translate(&self, part_num: &str) -> Option<Resolved<'_>> {
        if let Some(&design_id) = self.pin_map.get(part_num) {
            // A pin design that LDraw renamed or aliases follows the hop —
            // our geometry lives under the target id. A pin design that is
            // simply absent (no hop) is still returned as-is: the pin is
            // API-vouched, and the aggregation keeps it (invisible in the
            // `part` view until the library grows it).
            if !self.catalog.contains(design_id) {
                if let Some(target) = self.redirects.get(design_id) {
                    return Some(Resolved {
                        design_id: target,
                        via_literal: false,
                        via_redirect: true,
                    });
                }
            }
            return Some(Resolved {
                design_id,
                via_literal: false,
                via_redirect: false,
            });
        }
        let (base, _flexion) = canonical_design_id(part_num);
        if let Some(design_id) = self.catalog.get(base) {
            return Some(Resolved {
                design_id,
                via_literal: true,
                via_redirect: false,
            });
        }
        // The bare id isn't a catalogued design, but it may be a tombstone
        // or an alias — membership of the flattened map's *target* is what
        // vouches for it.
        self.redirects.get(base).map(|target| Resolved {
            design_id: target,
            via_literal: true,
            via_redirect: true,
        })
    }

    /// The redirect map reversed: in-catalog design id → the sorted ids that
    /// redirect to it (retired *and* alias). The FTS build indexes these so a
    /// search for an old or alias part number finds the current design.
    pub(crate) fn redirect_sources_by_target(&self) -> std::collections::BTreeMap<&str, Vec<&str>> {
        let mut by_target: std::collections::BTreeMap<&str, Vec<&str>> =
            std::collections::BTreeMap::new();
        for (source, target) in &self.redirects {
            by_target.entry(target).or_default().push(source);
        }
        for sources in by_target.values_mut() {
            sources.sort_unstable();
        }
        by_target
    }

    /// [`Self::translate`], additionally requiring the design to be in
    /// `ldraw_part` — the membership filter `rb_part_relationships` keys its
    /// design-id columns on. (Literal and redirected translations are
    /// in-catalog by construction; this only further gates pin translations.)
    pub(crate) fn resolve_in_catalog(&self, part_num: &str) -> Option<Resolved<'_>> {
        self.translate(part_num)
            .filter(|r| self.catalog.contains(r.design_id))
    }
}

/// Run `sql` and collect its result rows into a `HashSet`.
///
/// `sql` must be a `SELECT` yielding exactly one `TEXT` column — e.g.
/// `"SELECT design_id FROM ldraw_part"`, which is how the resolver loads
/// its catalog-membership set. Every row's value becomes one set element
/// (so duplicates, if the query can produce them, collapse). Fails — with
/// the offending SQL in the error chain — if the statement doesn't
/// prepare, the query errors, or a row's first column isn't `TEXT`.
fn load_column_set(conn: &Connection, sql: &str) -> Result<HashSet<String>> {
    let mut stmt = conn
        .prepare(sql)
        .with_context(|| format!("prepare {sql:?}"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .with_context(|| format!("query {sql:?}"))?;
    let mut set = HashSet::new();
    for row in rows {
        set.insert(row.with_context(|| format!("read row of {sql:?}"))?);
    }
    Ok(set)
}

/// Load one redirect hop table into a `design_id → target_design_id` map.
///
/// `table` must be one of the scan's hop tables — `ldraw_moved_to` or
/// `ldraw_alias` — i.e. a table with `design_id TEXT PRIMARY KEY` and
/// `target_design_id TEXT NOT NULL` columns (the shape
/// `ldraw_part::populate_hops` creates). The returned map holds the raw
/// **single** hops exactly as stored: no chain flattening, no
/// catalog-membership filtering — both are [`flatten_redirects`]'s job.
/// The primary key guarantees one target per source id.
fn load_hops(conn: &Connection, table: &str) -> Result<HashMap<String, String>> {
    let mut stmt = conn
        .prepare(&format!("SELECT design_id, target_design_id FROM {table}"))
        .with_context(|| format!("prepare {table} query"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .with_context(|| format!("query {table}"))?;
    let mut hops = HashMap::new();
    for row in rows {
        let (from, to) = row.with_context(|| format!("read {table} row"))?;
        hops.insert(from, to);
    }
    Ok(hops)
}

/// Flatten the one-hop redirect map: each redirected id maps to the first
/// design along its chain that is in the catalog. Chains that never reach
/// the catalog — dead ends, cycles, or anything past [`REDIRECT_MAX_HOPS`]
/// — are dropped (such an id stays untranslatable).
fn flatten_redirects(
    hops: &HashMap<String, String>,
    catalog: &HashSet<String>,
) -> HashMap<String, String> {
    let mut flat = HashMap::new();
    for (start, first) in hops {
        let mut current = first;
        for _ in 0..REDIRECT_MAX_HOPS {
            if catalog.contains(current) {
                flat.insert(start.clone(), current.clone());
                break;
            }
            match hops.get(current) {
                Some(next) => current = next,
                None => break,
            }
        }
    }
    flat
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refresh_parts::RbPartCrossRefs;

    fn pin_of(pairs: &[(&str, &str)]) -> RbCrossRefPin {
        RbCrossRefPin {
            generated: "2026-01-01".into(),
            parts: pairs
                .iter()
                .map(|(part_num, design)| {
                    (
                        (*part_num).to_owned(),
                        RbPartCrossRefs {
                            ldraw: (*design).to_owned(),
                            external_ids: Default::default(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn resolver_with<'a>(
        pin: &'a RbCrossRefPin,
        catalog: &[&str],
        moved: &[(&str, &str)],
        aliases: &[(&str, &str)],
    ) -> PartResolver<'a> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE ldraw_part (design_id TEXT PRIMARY KEY);
             CREATE TABLE ldraw_moved_to (
                 design_id TEXT PRIMARY KEY,
                 target_design_id TEXT NOT NULL
             );
             CREATE TABLE ldraw_alias (
                 design_id TEXT PRIMARY KEY,
                 target_design_id TEXT NOT NULL
             );",
        )
        .unwrap();
        for d in catalog {
            conn.execute("INSERT INTO ldraw_part VALUES (?1)", [d])
                .unwrap();
        }
        for (from, to) in moved {
            conn.execute("INSERT INTO ldraw_moved_to VALUES (?1, ?2)", [from, to])
                .unwrap();
        }
        for (from, to) in aliases {
            conn.execute("INSERT INTO ldraw_alias VALUES (?1, ?2)", [from, to])
                .unwrap();
        }
        PartResolver::new(&conn, pin).unwrap()
    }

    #[test]
    fn pin_wins_over_literal_and_skips_membership() {
        // "92693" is pinned to a design NOT in the catalog; the pin still
        // answers (inventory aggregates it), unflagged as literal.
        let pin = pin_of(&[("92693", "92693c01")]);
        let resolver = resolver_with(&pin, &["92693"], &[], &[]);
        let r = resolver.translate("92693").expect("pin answers");
        assert_eq!(r.design_id, "92693c01");
        assert!(!r.via_literal, "pin translation is not a literal fallback");
        assert!(!r.via_redirect);
        // …but resolve_in_catalog gates it out.
        assert!(resolver.resolve_in_catalog("92693").is_none());
    }

    #[test]
    fn literal_fallback_requires_catalog_membership() {
        let pin = pin_of(&[]);
        let resolver = resolver_with(&pin, &["6141"], &[], &[]);
        let r = resolver.translate("6141").expect("literal match answers");
        assert_eq!(r.design_id, "6141");
        assert!(r.via_literal);
        // In-catalog by construction, so resolve_in_catalog agrees.
        assert!(resolver.resolve_in_catalog("6141").is_some());
        // A part in neither the pin nor the library stays unmapped (no
        // tombstone registered here).
        assert!(resolver.translate("4073").is_none());
    }

    #[test]
    fn literal_fallback_collapses_flexion_suffix() {
        let pin = pin_of(&[]);
        let resolver = resolver_with(&pin, &["92693c01"], &[], &[]);
        let r = resolver
            .translate("92693c01-f1")
            .expect("flexion variant collapses to base");
        assert_eq!(r.design_id, "92693c01");
        assert!(r.via_literal);
    }

    #[test]
    fn moved_to_chases_unpinned_tombstones() {
        // RB's canonical 4073 is an LDraw tombstone pointing at 6141.
        let pin = pin_of(&[]);
        let resolver = resolver_with(&pin, &["6141"], &[("4073", "6141")], &[]);
        let r = resolver.translate("4073").expect("tombstone chases");
        assert_eq!(r.design_id, "6141");
        assert!(r.via_literal, "unpinned, so the literal path found it");
        assert!(r.via_redirect);
        // In-catalog by construction.
        assert!(resolver.resolve_in_catalog("4073").is_some());
    }

    #[test]
    fn moved_to_chases_pinned_tombstones() {
        // The pin says 3023, but LDraw renamed 3023 → 3023b; the geometry
        // lives under the target, so the translation follows the rename.
        let pin = pin_of(&[("3023", "3023")]);
        let resolver = resolver_with(&pin, &["3023b"], &[("3023", "3023b")], &[]);
        let r = resolver.translate("3023").expect("pin design chases");
        assert_eq!(r.design_id, "3023b");
        assert!(!r.via_literal, "the pin supplied the starting design");
        assert!(r.via_redirect);
        // A pinned design that IS in the catalog is never chased, even if a
        // stale tombstone row exists for it.
        let pin2 = pin_of(&[("3001", "3001")]);
        let resolver2 = resolver_with(&pin2, &["3001", "9999"], &[("3001", "9999")], &[]);
        let r2 = resolver2.translate("3001").unwrap();
        assert_eq!(r2.design_id, "3001");
        assert!(!r2.via_redirect);
    }

    #[test]
    fn redirects_flatten_chains_and_drop_dead_or_cyclic_ones() {
        let pin = pin_of(&[]);
        let resolver = resolver_with(
            &pin,
            &["c"],
            &[
                // a → b (moved) → c (alias): a mixed chain flattens to the
                // in-catalog end.
                ("a", "b"),
                // dead → gone: never reaches the catalog.
                ("dead", "gone"),
                // x → y → x: a cycle must neither hang nor resolve.
                ("x", "y"),
                ("y", "x"),
            ],
            &[("b", "c")],
        );
        let r = resolver.translate("a").expect("chain flattens");
        assert_eq!(r.design_id, "c");
        assert!(r.via_redirect);
        assert!(resolver.translate("dead").is_none());
        assert!(resolver.translate("x").is_none());
        assert!(resolver.translate("y").is_none());
    }

    #[test]
    fn aliases_chase_like_tombstones_and_reverse_index() {
        // RB's 30071 is an LDraw hard alias of 3005: unpinned, so the
        // literal path finds the alias hop.
        let pin = pin_of(&[]);
        let resolver = resolver_with(&pin, &["3005"], &[], &[("30071", "3005")]);
        let r = resolver.translate("30071").expect("alias chases");
        assert_eq!(r.design_id, "3005");
        assert!(r.via_literal);
        assert!(r.via_redirect);
        assert!(resolver.resolve_in_catalog("30071").is_some());
        // The reverse view groups sources under their in-catalog target,
        // sorted — what the FTS alt_ids column indexes.
        let by_target = resolver.redirect_sources_by_target();
        assert_eq!(by_target["3005"], vec!["30071"]);
    }
}
