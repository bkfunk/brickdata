# Catalog cleaning & reconciliation rules

The catalog builder (`crates/catalog-builder`) parses the pinned upstream
snapshots into `catalog.sqlite`. Its added value over the raw data is the
cleaning below — each rule names the module that implements it, which is
the authority if this document drifts.

## ID canonicalization (`-fN` flexion collapse)

`core::canonical_design_id` (in `src/core/catalog.rs`). LDraw ships
flexion-position files (`32580-f1.dat`, `32580-f2.dat`, …) that are one
pickable design: ids collapse to the stem, positions are recorded as
`flexion_variants`. `-f0` and out-of-range position numbers are
deliberately NOT stripped (they are not flexion positions). The same
function is reused by `refresh-part-mappings` and the build-time resolver,
so the pin and the catalog can never disagree on canonical form. A
Rebrickable mapping whose ids collapse to more than one distinct LDraw id
is an anomaly: logged and skipped, never guessed.

## Cross-source part resolution (Rebrickable `part_num` → LDraw `design_id`)

`src/build/resolve.rs` (`PartResolver::translate`), a three-step ladder:

1. **Committed pin** — `data/rebrickable/part_crossrefs.ron`, regenerated
   only by the maintainer-run `refresh-part-mappings` subcommand.
2. **Literal fallback** — an unpinned `part_num` counts as its own
   `design_id` only if that id exists in the scanned `ldraw_part` table.
3. **Redirect chase** — pinned or literal ids are chased through the union
   of `ldraw_moved_to` (retirement tombstones) + `ldraw_alias` (duplicate
   geometry) hop maps, max 8 hops; cycles and dead ends drop the row.
   This resolves cases where Rebrickable and LDraw canonicalized in
   opposite directions (e.g. RB `4073` vs LDraw `~Moved to 6141`; RB
   `30071` as a hard alias of `3005`).

## Color reconciliation

`src/refresh_colors.rs`. Keys on the LDraw color code; picks canonical
LEGO/BrickLink display names; folds every other name string Rebrickable
knows (across systems and history) into a deduplicated, sorted `aliases`
bag for search. `data/rebrickable/color_excludes.ron` removes colors that
must not surface. Output is the compiled-in
`crates/catalog-builder/src/core/color_names.ron`; at build time
`src/build/inventory.rs` uses it to translate Rebrickable color ids to
LDraw codes.

## Inventory cleaning

`src/build/rb_inventories.rs`: when a set has several inventory versions,
only the highest survives. `src/build/inventory.rs`: spare-only
part/color/set rows are promoted to main rows (a part that only ever
appears as a spare still counts as appearing); rows whose part has no
LDraw mapping or whose color has no LDraw code are skipped and counted
(the counts land in `meta` for observability, `inventory_parts_*` keys).

## Ingest guards

`src/build/rb_ingest.rs`: every CSV's header is validated against the
pinned column order; integer fields parse strictly; unknown enum values
(`is_spare`, `rel_type`) are hard errors, not warnings. A snapshot that
drifts from the expected shape fails the build rather than producing a
silently wrong catalog.

## Relationships filter

`src/build/rb_part_relationships.rs`: only rows whose rel type is one of
the six catalog-relevant codes (P/T/M/A/R/B) and that touch at least one
in-catalog part are ingested; exact duplicates collapse.

## Cross-reference tables (schema v2)

`src/build/xref.rs` (#12). The `colors` table is the compiled-in color
reference written out as data (LDraw code ↔ RB color id ↔ canonical
LEGO/BrickLink/Rebrickable names, aliases as a JSON array).
`rb_part_external_id` is the cross-ref pin flattened to one row per
`(part_num, system, external_id)` with the LDraw `design_id` resolved
through the same membership-gated ladder as the relationships slice (NULL
when the part isn't in the scanned library). External ids are second-hand
via the Rebrickable API — not authoritative (see `LICENSES/REBRICKABLE.md`).

Schema v3 adds the spine that story rests on: a resolved `design_id`
column on `rb_parts` itself, covering every Rebrickable part (not just
those with external ids), same ladder, NULL when unresolvable.

## Determinism

Identical pins produce byte-identical `catalog.sqlite` (integration test
`build_is_deterministic`). The build stages
into `<out>.tmp`, fsyncs, and atomically renames, so a failed build never
leaves a half-written DB; `build_status = 'complete'` is the very last
`meta` write. Inputs are fetched through a sha256-verified
content-addressed cache (`brickdata::Fetcher`), so the bytes are the
pin's or the build fails.
