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
| `catalog-YYYY-MM-DD` | built `catalog.sqlite` | output |

Releases are **immutable** once cut — a new snapshot is a new dated release.
Each release has a matching pin file under [`pins/`](pins/) recording asset
URLs + sha256 hashes; consumers vendor the pin and fetch/verify against it.

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
produce a byte-identical DB. The cleaned Rust-friendly inputs it consumes
(`data/rebrickable/part_crossrefs.ron`, the compiled-in color reference)
are committed and versioned here.

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
