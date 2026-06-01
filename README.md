# blockstar-data

Mirrored external data + built outputs for [Blockstar](https://github.com/bkfunk/blockstar).

This repo holds **GitHub Release assets**, not a working tree. Its job is to
make Blockstar's build reproducible and its catalog queryable without a build.

## Why this exists

Blockstar builds a metadata catalog from Rebrickable bulk CSVs and the LDraw
parts library. Both upstreams are **non-archival** — they serve only the latest
data (Rebrickable regenerates daily; LDraw's `complete.zip` is latest-only).
Pinning upstream hashes in the main repo therefore can't make a build
reproducible: a clone fetching next month gets *different* bytes whose hashes
won't match, and the original bytes are unrecoverable.

This repo solves that by **mirroring the exact bytes to immutable, dated
releases**, which GitHub hosts with free storage and bandwidth. The main repo
pins a release URL + hash; anyone can fetch the pinned data forever.

It also **hosts the built `catalog.sqlite`**, so the metadata can be queried
with `sqlite3` or [Datasette](https://datasette.io/) with zero build.

Full design rationale: [bkfunk/blockstar#86](https://github.com/bkfunk/blockstar/issues/86).

## Releases

| Tag | Contents | Kind |
|-----|----------|------|
| `rebrickable-YYYY-MM-DD` | 8 Rebrickable bulk CSVs (`parts`, `elements`, …) | input |
| `ldraw-YYYY-MM-DD` | merged LDraw library tree (zip) + content manifest | input |
| `catalog-YYYY-MM-DD` | built `catalog.sqlite` | output |

Releases are **immutable** once cut — a new snapshot is a new dated release.

## Refreshing the data (maintainers)

Requires only `gh` (authenticated), `just`, `curl`, `unzip`, `zip`, and
`sha256sum`/`shasum`. No Rust toolchain.

```sh
just mirror-rebrickable     # cut rebrickable-<today> + write pins/
just mirror-ldraw           # cut ldraw-<today> (overlay-aware + manifest)
just verify pins/rebrickable-<today>.ron   # prove a clone can reproduce it
```

Each `mirror-*` recipe writes a pin file under `pins/`. Copy the pin into the
main repo to point its build at the new snapshot:

- `pins/rebrickable-*.ron` → `external-data/rebrickable/csv-snapshot.ron`
- `pins/ldraw-*.ron` → `external-data/ldraw/ldraw-snapshot.ron`

The main repo's `just data-setup` clones this repo for you and points you here.

Built outputs are published *from the main repo* (which has the Rust builder),
which shells out to `just publish-catalog <path>` here.

## Licensing

Mirrored data carries its upstream license — see [`LICENSES/`](LICENSES/):

- **LDraw** library: Creative Commons Attribution (CC BY 2.0 / 4.0) per the
  LDraw Contributor Agreement (`CAlicense*.txt`, `CAreadme.txt`). These files
  travel with every `ldraw-*` release.
- **Rebrickable** data: CC BY 2.0 (`REBRICKABLE.md`).

LEGO® is a trademark of the LEGO Group, which does not sponsor or endorse this
project.
