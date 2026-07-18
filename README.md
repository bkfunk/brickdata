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

## Refreshing the data (maintainers)

Requires only `gh` (authenticated), `just`, `curl`, `unzip`, `zip`, and
`sha256sum`/`shasum`. No Rust toolchain.

```sh
just mirror-rebrickable     # cut rebrickable-<today> + write pins/
just mirror-ldraw           # cut ldraw-<today> (overlay-aware + manifest)
just verify pins/rebrickable-<today>.ron   # prove a clone can reproduce it
```

Each `mirror-*` recipe writes a pin file under `pins/`. Consumers copy the pin
into their own repo to point their build at the new snapshot — e.g. Blockstar
vendors them as `external-data/rebrickable/csv-snapshot.ron` and
`external-data/ldraw/ldraw-snapshot.ron`.

Built outputs (like `catalog.sqlite`) are produced by consumer repos that have
the relevant build tooling, then published here via
`just publish-catalog <path>`.

## Licensing

Mirrored data carries its upstream license — see [`LICENSES/`](LICENSES/):

- **LDraw** library: Creative Commons Attribution (CC BY 2.0 / 4.0) per the
  LDraw Contributor Agreement (`CAlicense*.txt`, `CAreadme.txt`). These files
  travel with every `ldraw-*` release.
- **Rebrickable** data: CC BY 2.0 (`REBRICKABLE.md`).

LEGO® is a trademark of the LEGO Group, which does not sponsor or endorse this
project.
