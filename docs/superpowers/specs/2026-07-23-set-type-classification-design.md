# Set-type classification & build-based frequency filtering (brickdata #19)

**Status:** design approved, pending spec review
**Issue:** [brickdata#19](https://github.com/bkfunk/brickdata/issues/19)
**Relates to:** #17 / #18 (the `part_frequency` sidecar this cleans up),
blockstar#137 (cache-subset ranking consumes the sidecar), blockstar#130
(runtime popularity sort)

## Summary

Rebrickable counts many things as "sets" that nobody would build: bulk parts
packs (chain, gears, bricks by the hundred), baseplates, storage tubs, buckets,
and non-build merchandise (watches, magnets). These pollute every set-based
popularity metric the `part_frequency.ron` sidecar produces — worst for the
`qty` (total-quantity) series, mildly for the `sets` (breadth) series.

This work does three things:

1. **Classifies every set** with a new `rb_sets.set_type` column
   (`build` / `parts_pack` / `baseplate` / `merchandise` / `unknown`), computed
   from numeric signals (a set's true distinct-mold count and its
   pieces-per-mold concentration) plus curated set-name keywords. **No LDraw
   part categories are consulted** — see the follow-up note below.
2. **Makes "build" the fundamental unit of the popularity signal.** The sidecar
   is rebuilt to count *builds*, not *sets*: its per-part fields are renamed
   `sets → builds` and `qty → build_qty`, and it aggregates only sets with
   `set_type = 'build'`.
3. **Records the classification policy and thresholds in `meta`** (and in the
   sidecar's RON header) so the projection stays a reproducible function of the
   pinned input.

## Vocabulary: *set* vs *build*

Adopted deliberately, because the distinction is the whole point of the issue:

- **set** — any `rb_sets` row (a Rebrickable "set", generic; unchanged).
- **build** — a set that is an actual buildable model: `set_type = 'build'`.
  This is what an AFOL means by "a set." The popularity metric that matters is
  "how many *builds* use this part," so the sidecar is expressed in builds.

We use "build" rather than LEGO's own term "model" because *model* is hopelessly
overloaded in Blockstar's domain (Blockstar is a LEGO **model** editor, full of
3D **models**). "Build" is AFOL-native, pluralizes and compounds cleanly
(`builds`, `build_qty`), and carries no collision.

This also removes a latent trap: before the rename, the sidecar's `sets`
(packs excluded) and `rb_part_summary.set_count` (packs included) were different
populations sharing the word *set*. As `builds` vs `set_count`, the difference
is legible from the names alone.

## The classifier

### Inputs (per set)

- **`distinct_part_count`** — the *true* number of distinct part molds in the
  set, counted from the **raw** `inventory_parts` rows **before** the
  part_num → LDraw mapping drops anything. `NULL` for a set with no catalogued
  inventory. This is the primary signal.
- **`pieces_main`** — Σ non-spare `quantity` over the set's raw inventory. Used
  only to derive pieces-per-mold concentration; not stored.
- **`name`** — `rb_sets.name`.

`distinct_part_count` and `pieces_main` are accumulated per `set_id` **inside
the existing `inventory_parts` streaming pass** (`build/inventory.rs`) — no
extra CSV read. They are the raw, pre-mapping figures precisely so a small
licensed set full of un-mapped printed/minifig parts is not undercounted and
mislabeled a pack (the data-correctness note in the issue).

### Rules

The load-bearing insight from calibration: **genuine builds that happen to have
a pack-like name (e.g. "…Battle Pack") have a high distinct-mold count** (47–93
in the data), whereas real packs live in the low-distinct region. So **every
non-build rule is gated behind a low distinct-mold ceiling**; above it, a set is
always a `build` no matter what its name says.

```
classify(distinct_part_count: Option<u32>, pieces_main: Option<u32>, name):

  eligible = distinct_part_count.is_none()            # no inventory: name-only
             || distinct_part_count <= DISTINCT_CEILING

  if !eligible:                                        # distinct > ceiling
      return build                                     # protects Battle Packs etc.

  # name keywords first (highest-precision labels), gated by `eligible`
  if name matches BASEPLATE_KEYWORDS   → baseplate
  if name matches MERCHANDISE_KEYWORDS → merchandise
  if name matches PACK_PHRASES         → parts_pack

  match distinct_part_count:
    Some(1)                                        → parts_pack   # single element
    Some(d) if pieces_main / d >= CONCENTRATION    → parts_pack   # bulk / monotype
    Some(_)                                        → build        # small diverse build
    None                                           → unknown      # no data, no keyword
```

- `classify` is a **pure function**, so the calibration is unit-testable in
  isolation. This is the reason set classification is its own module rather than
  inlined into the streaming loop.
- **Precision-oriented on purpose.** In the distinct 2–14 zone there is no clean
  cutoff — real builds ("Safari Hippo", "Aeroplane") and real packs ("Jumbo
  Building Tub") sit at the same concentration. So an ambiguous small set stays
  `build`. That is acceptable: a low-concentration small set (few molds, few
  pieces each) barely distorts the metric anyway.

### Thresholds (named constants, calibration documented in-code)

| Constant | Value | Why (from `catalog-v3.sqlite`, 19,574 sets w/ inventory) |
|---|---|---|
| `DISTINCT_CEILING` | 14 | 37% of sets have <15 distinct molds; that band holds essentially all packs. Battle Packs / Boosters — real builds — sit at 47–93 distinct, so a ceiling of 14 excludes them from every non-build rule. |
| `CONCENTRATION` | 15 | pieces-per-mold ≥ 15 catches multi-mold bulk (tubs, buckets, mosaics — e.g. "Basic Bulk Tub" = 10 molds / 1200 pieces = 120). Real small builds in the transition zone top out ~11 ("Safari Hippo"), so 15 avoids them. |

Both constants carry a doc comment naming the calibration query that set them.
**No config file** — the "tunable policy" is these documented constants plus the
`meta`/header record of the values used per build (agreed in design).

### Taxonomy — `set_type` values

| value | meaning | in default drop-set? |
|---|---|---|
| `build` | a buildable model (the default / kept type) | no |
| `parts_pack` | bulk / monotype / assortment pack, or a single-element "set" | yes |
| `baseplate` | named baseplate / building-plate | yes |
| `merchandise` | named non-build product (watch, magnet, …) | yes |
| `unknown` | **no catalogued inventory** and no name-keyword hit — genuinely unclassifiable | (moot — see below) |

`unknown` is real and required: **7,391 of 26,965 sets (27%) have no catalogued
inventory at all**. It is *not* "the classifier gave up on a build" — it is
"there is no part data to classify by." Crucially, **every one of those sets has
zero rows in `rb_part_color_set`**, so they never reach the frequency
aggregation; whether `unknown` is nominally in the drop-set is moot for the
sidecar. It is simply the honest column value.

Dropping `unknown` (as floated in the issue) was rejected once the data showed
these 7,391 genuine no-data sets exist.

### Keyword lists (initial; refined with precision tests during implementation)

Matched case-insensitively, on word/phrase boundaries, and **only when
`eligible`** (so a 90-mold set named "…Baseplate" stays a `build`). Deliberately
high-precision / low-recall — keywords confirm, they are not the sole test.

- **`BASEPLATE_KEYWORDS`**: `baseplate`, `base plate`, `building plate`,
  `brickplate`.
- **`MERCHANDISE_KEYWORDS`**: `watch`, `magnet`, `keychain` / `key chain`,
  `ornament`, `clock`. (Kept conservative; most merchandise is already `unknown`
  with no inventory.)
- **`PACK_PHRASES`**: `pack of`, `parts pack`, `spare`, `assorted`,
  `assortment`, `bulk`, `bucket`, ` tub`, `supplementary`. **Bare "pack" is
  deliberately excluded** — it matches "Battle Pack", "Booster Pack", "Backpack"
  (all builds); the specific phrases do not.

## Pipeline & storage

Build order today (`build.rs::build_into`): ingest small Rebrickable tables
(incl. `rb_sets`) → `rb_sets::add_set_ids` → `inventory::build` → … → finalize.
The new work slots between `inventory::build` and finalize:

1. **`build/inventory.rs`** — during the existing stream, accumulate per
   `set_id`: a set of distinct raw `part_num` (→ `distinct_part_count`) and the
   non-spare `quantity` sum (→ `pieces_main`). Return them to the driver
   alongside `InventoryStats`. (Accumulating a per-set `HashSet` of part numbers
   over ~1.5M rows is acceptable for a build tool; detail left to the plan.)

2. **`build/set_classify.rs` (new)** — the pure `classify(...)` function, a
   `SetType` enum, and a build step that:
   - `ALTER TABLE rb_sets ADD COLUMN distinct_part_count INTEGER` (nullable;
     `NULL` for no-inventory sets),
   - `ALTER TABLE rb_sets ADD COLUMN set_type TEXT NOT NULL DEFAULT 'unknown'`,
   - fills both per set (numeric inputs from step 1, `name` from `rb_sets`),
   - returns per-type counts for `meta`.

3. **Schema bump** `SCHEMA_VERSION` 3 → **4** (new `rb_sets` columns); update the
   version comment in `build.rs`.

4. **`meta` records** (observability + reproducibility), stamped via the existing
   `stamp_all` pattern:
   - `set_type_build_count`, `set_type_parts_pack_count`,
     `set_type_baseplate_count`, `set_type_merchandise_count`,
     `set_type_unknown_count`
   - `set_class_distinct_ceiling` = `14`, `set_class_concentration` = `15`
   - `part_frequency_included_set_types` = `build` (the sidecar's filter policy)

## Sidecar changes (`build/part_frequency.rs` → `part_frequency.ron`)

1. **Build-based aggregation.** Both aggregation queries join
   `rb_part_color_set → rb_sets` and keep only `set_type = 'build'`. A part that
   appears *only* in non-build sets drops out of the sidecar entirely — correct
   (it is not used in builds); the cache consumer (blockstar#137) falls back to
   a runtime parse on a miss, so this is self-healing.
2. **Field rename.** Per-part RON fields `sets → builds`, `qty → build_qty`. The
   Rust `PartFreq` struct fields and the `Series` render sites rename to match.
   The file name `part_frequency.ron` is unchanged.
3. **Header / provenance.** The `HEADER` comment is rewritten: the file now
   counts **builds** (excludes parts packs / baseplates / merchandise / sets
   with no catalogued inventory), and the header records the filter policy and
   the classifier thresholds used, so a consumer can reproduce the semantics
   from the artifact alone.

Because the sidecar is not yet published (that is #17's still-open task) and
blockstar#137 has not been built, the field rename breaks no existing consumer.

## Scope boundaries & deferrals

- **`rb_part_summary` and the `part` view stay raw** — they keep counting every
  set in `set_count` / `qty_sum`. This is intentional (honoring the #17 deferral
  of the summary-column work). The build filter applies **only to the sidecar**
  in this issue. When blockstar#130 (runtime frequency sort) is picked up, it
  adds `build_count` / `build_qty` to the summary / `part` view using this same
  `set_type` column — a small, clean extension. The rename keeps the two
  legible in the meantime.
- **LDraw part categories are not used.** A follow-up issue will investigate
  whether classifying by the dominant part's LDraw category (which brickdata
  owns) improves baseplate/merchandise recall. Kept out here to see how far
  name + numeric signals get, and to avoid coupling set-classification to the
  part taxonomy.

## Acceptance criteria

- [ ] True `distinct_part_count` per set computed from raw `inventory_parts`
      (pre-mapping) and stored on `rb_sets`.
- [ ] `rb_sets.set_type` stored for every set
      (`build`/`parts_pack`/`baseplate`/`merchandise`/`unknown`).
- [ ] The `part_frequency` sidecar aggregates only `set_type = 'build'`, with
      per-part fields renamed `builds` / `build_qty`; policy + thresholds
      recorded in `meta` and in the RON header.
- [ ] Schema version bumped to 4; per-type set counts in `meta`.
- [ ] Spot-check: known packs (bulk chain / gears / bricks, baseplates, tubs)
      classify non-build; known small builds ("American Flag", small licensed
      sets) stay `build`; Battle Packs (high distinct) stay `build`.

## Testing

- **`set_classify` unit tests** on `classify(...)`: the ceiling gate (high-distinct
  "Battle Pack" → build), `distinct == 1` → parts_pack, concentration boundary,
  each keyword class, `None` inventory → unknown, keyword-on-no-inventory →
  labeled (not unknown).
- **`inventory` tests**: distinct raw part_num counted before mapping/dropping;
  non-spare piece sum excludes spares.
- **`part_frequency` tests**: extend the in-memory seed with a `rb_sets`
  (`set_type`) table; assert non-build sets are excluded from both the all-time
  and per-year aggregates, and that the RON emits `builds` / `build_qty`.
- **Determinism** unchanged: the classifier is a pure function of already-
  deterministic inputs, so the byte-identical-rebuild guarantee (#73) holds.

## Known limitations

- A brand-new part appearing only in a handful of recent builds ranks low
  (unchanged from #17; self-healing for the cache consumer).
- Low-concentration small assortments (e.g. "12 doors and 5 hinges") that don't
  match a keyword remain labeled `build`. They distort the metric negligibly, so
  this is an accepted precision/recall trade, not a defect.
- Mosaics (few molds, thousands of tiles) classify as `parts_pack` via the
  concentration rule. The label is coarse, but excluding them from build-based
  popularity is correct — they are not representative part usage.
