# brickdata

Pinnable, immutable snapshots of brick-ecosystem data (Rebrickable, LDraw),
plus built catalog outputs — hosted as GitHub Release assets.

> Formerly `blockstar-data`. GitHub redirects the old repo and release-asset
> URLs, so existing pins keep resolving; new pins carry `brickdata` URLs.

## Why this exists

Reproducible builds need reproducible inputs, but the major brick-data
upstreams are **non-archival** — they serve only the latest data (Rebrickable
regenerates its bulk CSVs daily; LDraw's `complete.zip` is latest-only).
Pinning upstream hashes therefore can't make a build reproducible: a clone
fetching next month gets *different* bytes whose hashes won't match, and the
original bytes are unrecoverable.

This repo solves that by **mirroring the exact bytes to immutable, dated
releases**, which GitHub hosts with free storage and bandwidth. Any consumer
pins a release URL + hash and can fetch that exact data forever.

It also **hosts the built `catalog.sqlite`**, so the parts metadata can be
queried with `sqlite3` or [Datasette](https://datasette.io/) with zero build.

Original design rationale: [bkfunk/blockstar#86](https://github.com/bkfunk/blockstar/issues/86).

## Relationship to studkit

[studkit](https://github.com/bkfunk/studkit) is the **library** (compute:
LDraw parsing/baking, coupling detection, mass estimation); brickdata is the
**dataset** (archived inputs and evaluated outputs). brickdata's build
pipeline may use studkit's tools to derive enrichment columns; studkit never
depends on brickdata at runtime — it consumes pinned snapshots at build/data
generation time only.

## Releases

| Tag | Contents | Kind |
|-----|----------|------|
| `rebrickable-YYYY-MM-DD` | 8 Rebrickable bulk CSVs (`parts`, `elements`, …) | input |
| `ldraw-YYYY-MM-DD` | merged LDraw library tree (zip) + content manifest | input |
| `catalog-YYYY-MM-DD` | built `catalog.sqlite` + sidecars `part_frequency.ron`, `color_names.ron` | output |

Releases are **immutable** once cut — a new snapshot is a new dated release.
Each release has a matching pin file under [`pins/`](pins/) recording asset
URLs + sha256 hashes (+ byte sizes); consumers vendor the pin and
fetch/verify against it. Every asset on a release is pinned — including the
catalog sidecars, see [Catalog sidecars](#catalog-sidecars) — because
consumers fetch by url + hash, so an unpinned asset would be unreachable.

## Consuming snapshots from Rust

The [`brickdata` crate](crates/brickdata/) is the consumer-side API: it
parses the pin files, downloads assets with mandatory sha256/size
verification into a local content-addressed cache (a verified cache hit does
no network I/O), and provides gunzip/unzip helpers for the asset encodings.
Verification failures are hard errors. Typed row structs for the cleaned
catalog tables are next (#4); the crate is unpublished until that API
settles.

```rust
let pin = RebrickablePin::from_path("pins/rebrickable-2026-06-01.ron")?;
let tables = Fetcher::new(cache_dir).fetch_rebrickable(&pin)?;

// A catalog release: the DB plus its pinned sidecars (name -> verified path).
let pin = CatalogPin::from_path("pins/catalog-2026-09-16.ron")?;
let fetcher = Fetcher::new(cache_dir);
let db = fetcher.fetch_catalog(&pin)?;
let freq = fetcher.fetch_catalog_sidecar(&pin, CatalogPin::PART_FREQUENCY_SIDECAR)?;
let sidecars = fetcher.fetch_catalog_sidecars(&pin)?;
```

## Building the catalog

The catalog builder (`crates/catalog-builder`, migrated from Blockstar in
#3) parses and cleans the pinned snapshots into `catalog.sqlite` — see
[`docs/cleaning.md`](docs/cleaning.md) for the cleaning/reconciliation
rules, which are the package's main added value.

```sh
just build-catalog          # pins in → work/catalog.sqlite out (reproducible)
just publish-catalog work/catalog.sqlite   # cut catalog-<today> + pin
```

`build-catalog` needs only the committed pins and network access to this
repo's release assets: no Blockstar checkout, no API keys. Identical pins
produce a byte-identical DB (and byte-identical sidecars). The cleaned
Rust-friendly inputs it consumes (`data/rebrickable/part_crossrefs.ron`,
the compiled-in color reference) are committed and versioned here.

### Catalog sidecars

`build-catalog` writes two small **sidecar** files next to `catalog.sqlite`,
and `publish-catalog` uploads them to the same `catalog-*` release and pins
each one (url + sha256 + byte size) alongside the DB. The `brickdata` crate
verifies a sidecar exactly like the catalog before it becomes visible
(`Fetcher::fetch_catalog_sidecar` / `fetch_catalog_sidecars`).

| Sidecar | What it is | Consumer contract |
|---|---|---|
| `part_frequency.ron` | Per-part usage figures projected from the finished DB: for every part that appears in a set, distinct-set counts and quantities, all-time and per calendar year (the year window is anchored on the newest set year in the data, so the file is a pure function of the pinned inputs). Raw and unfiltered — which parts are pickable is the consumer's policy. | Deserialize the `PartFrequency(...)` document by field name (the file's header comment is the field reference); read it when only usage figures are needed, instead of opening the ~88 MB DB (blockstar#137). |
| `color_names.ron` | The color-name reference the catalog was built with: the builder's compiled-in `crates/catalog-builder/src/core/color_names.ron`, copied out byte-for-byte, so by construction it is the data the DB's `colors` table was written from. | Vendor it verbatim into a consumer that compiles the reference in (Blockstar's `blockstar-core`, blockstar#143), so the app's color names match the catalog it ships; never regenerate it downstream — the `refresh-color-names` subcommand here is the only writer. |

The catalog pin records the DB and every sidecar. This is exactly what
`publish-catalog` writes — the DB's own triple at the top level, then a
`sidecars` map keyed by asset filename whose entries are the same
`(sha256, bytes, mirror_url)` tuple the Rebrickable pins use:

```ron
// brickdata built-catalog pin.
(
  mirror_tag: "catalog-2026-09-16",
  asset_url: "https://github.com/bkfunk/brickdata/releases/download/catalog-2026-09-16/catalog.sqlite",
  sha256: "…",
  bytes: 88342528,
  sidecars: {
    "part_frequency.ron": (sha256: "…", bytes: 412345, mirror_url: "https://github.com/bkfunk/brickdata/releases/download/catalog-2026-09-16/part_frequency.ron"),
    "color_names.ron": (sha256: "…", bytes: 31454, mirror_url: "https://github.com/bkfunk/brickdata/releases/download/catalog-2026-09-16/color_names.ron"),
  },
)
```

Pins cut before sidecars existed (`catalog-2026-07-19a` and earlier) have no
`sidecars` field; `CatalogPin` parses them with an empty map, so existing
consumers and their vendored pins keep working unchanged. A sidecar missing
from the build directory at publish time is a warning, not an error — the
pin simply doesn't list it.

The two `refresh-*` subcommands of `catalog-builder` (color names, part
cross-refs) hit the authenticated Rebrickable API. They are maintainer-only
and rare — never part of the routine build and never run in CI:

```sh
cargo run --release -p brickdata-catalog-builder -- refresh-part-mappings --dry-run
```

## Refreshing the data (maintainers)

Requires only `gh` (authenticated), `just`, `curl`, `unzip`, `zip`, and
`sha256sum`/`shasum`. No Rust toolchain.

```sh
just mirror-rebrickable     # cut rebrickable-<today> + write pins/
just mirror-ldraw           # cut ldraw-<today> (overlay-aware + manifest)
just verify pins/rebrickable-<today>.ron   # prove a clone can reproduce it
just verify pins/catalog-<tag>.ron         # same for a catalog: the DB + every sidecar
```

Each `mirror-*` recipe writes a pin file under `pins/`. Consumers copy the pin
into their own repo to point their build at the new snapshot.

## Licensing

Code in this repo (the `brickdata` crate, the `just` recipes) is licensed
MIT OR Apache-2.0 ([`LICENSE-MIT`](LICENSE-MIT) /
[`LICENSE-APACHE`](LICENSE-APACHE)), matching studkit.

Mirrored data carries its upstream license — see [`LICENSES/`](LICENSES/):

- **LDraw** library: Creative Commons Attribution (CC BY 2.0 / 4.0) per the
  LDraw Contributor Agreement (`CAlicense*.txt`, `CAreadme.txt`). These files
  travel with every `ldraw-*` release.
- **Rebrickable** data: CC BY 2.0 (`REBRICKABLE.md`).

LEGO® is a trademark of the LEGO Group, which does not sponsor or endorse this
project.
