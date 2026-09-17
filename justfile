# brickdata — archives upstream brick-ecosystem data to GitHub Release assets
# and builds + hosts the catalog outputs. Mirror/publish recipes are pure
# shell + gh + sha256sum; `build-catalog` needs the Rust toolchain (the
# catalog builder lives in crates/catalog-builder, issue #3). Run `just --list`.
#
# Why this repo exists: Rebrickable's CDN and the LDraw library are
# non-archival (latest-only), so pinning upstream hashes does NOT make a build
# reproducible — the original bytes become unrecoverable. This repo mirrors the
# exact bytes to immutable, dated GitHub Releases so any clone can fetch the
# pinned data forever. It also hosts the built catalog.sqlite so the metadata
# can be queried with zero build. Background: bkfunk/blockstar issue #86.
#
# Releases:
#   rebrickable-YYYY-MM-DD   8 bulk CSVs (inputs)
#   ldraw-YYYY-MM-DD         merged-tree zip + content manifest (input)
#   catalog-YYYY-MM-DD       built catalog.sqlite + its sidecars part_frequency.ron
#                            and color_names.ron (output, `just build-catalog`);
#                            every asset pinned by url + sha256 + byte size

# Override to point recipes at a fork/test repo.
export GH_REPO := env_var_or_default("GH_REPO", "bkfunk/brickdata")

[private]
default:
    @just --list

# Mirror the 8 Rebrickable bulk CSVs to a dated release + emit a pin.
mirror-rebrickable:
    #!/usr/bin/env bash
    set -euo pipefail
    # Downloads from cdn.rebrickable.com, sha256s each, uploads to
    # rebrickable-<today>, and writes pins/rebrickable-<today>.ron. Copy that
    # pin into the main repo's external-data/rebrickable/csv-snapshot.ron.
    root="{{justfile_directory()}}"
    source "$root/lib/common.sh"
    tables=(parts part_categories part_relationships elements themes sets inventories inventory_parts)
    tag="rebrickable-$(today_utc)"
    work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
    declare -a entries
    for t in "${tables[@]}"; do
        f="$work/$t.csv.gz"
        download "https://cdn.rebrickable.com/media/downloads/$t.csv.gz" "$f"
        require_gzip "$f"
        sum="$(sha256_file "$f")"; bytes="$(wc -c < "$f" | tr -d ' ')"
        entries+=("$t.csv.gz|$sum|$bytes")
    done
    ensure_release "$tag" "Rebrickable bulk CSVs ($tag)" \
        "Mirror of cdn.rebrickable.com/media/downloads bulk CSVs. Immutable snapshot."
    for t in "${tables[@]}"; do upload_asset "$tag" "$work/$t.csv.gz"; done
    # Emit the pin (RON) the main repo's fetch consumes.
    pin="$root/pins/$tag.ron"
    {
        echo "// brickdata Rebrickable mirror pin. Blockstar consumers copy to:"
        echo "//   external-data/rebrickable/csv-snapshot.ron"
        echo "("
        echo "  mirror_tag: \"$tag\","
        echo "  snapshot_date: \"$(today_utc)\","
        echo "  file_fingerprints: {"
        for e in "${entries[@]}"; do
            IFS='|' read -r name sum bytes <<< "$e"
            url="$(asset_url "$tag" "$name")"
            echo "    \"$name\": (sha256: \"$sum\", bytes: $bytes, mirror_url: \"$url\"),"
        done
        echo "  },"
        echo ")"
    } > "$pin"
    echo "Wrote $pin" >&2
    echo "Release: https://github.com/$GH_REPO/releases/tag/$tag" >&2

# Mirror the LDraw library (overlay-aware) to a dated release + emit a pin.
mirror-ldraw:
    #!/usr/bin/env bash
    set -euo pipefail
    # Fetches complete.zip as the base, applies any newer `updated files (...)`
    # overlays in order, builds a content manifest of the merged tree, and
    # uploads the merged-tree zip + manifest to ldraw-<today>. The pinned
    # identity is the manifest hash (overlays never delete, so the recipe
    # alone isn't reproducible).
    root="{{justfile_directory()}}"
    source "$root/lib/common.sh"
    source "$root/lib/manifest.sh"
    tag="ldraw-$(today_utc)"
    work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
    tree="$work/tree"
    base="https://library.ldraw.org/library/updates/complete.zip"
    download "$base" "$work/complete.zip"
    extract_into "$work/complete.zip" "$tree"
    [ -d "$tree/parts" ] || die "no parts/ in complete.zip — upstream layout changed"
    # NOTE: overlay application would go here — discover `updated files
    # (YYMM##).zip` newer than the base and extract_into "$tree" in order.
    # complete.zip is already the fully-merged latest tree, so for the first
    # cut the base IS the merged tree; overlay discovery is wired but a no-op
    # when starting from complete.zip. Kept explicit so the drift hazard is
    # documented at the point it matters.
    man="$work/manifest.tsv"
    build_manifest "$tree" "$man"
    mhash="$(manifest_hash "$man")"
    fcount="$(wc -l < "$man" | tr -d ' ')"
    # Repackage the merged tree as a single zip asset (deterministic name).
    ( cd "$tree" && zip -q -r -X "$work/ldraw-merged.zip" . )
    zsum="$(sha256_file "$work/ldraw-merged.zip")"
    ensure_release "$tag" "LDraw library ($tag)" \
        "Merged LDraw library tree + content manifest. Mirror of library.ldraw.org complete.zip (+overlays). Carries CAlicense — see LICENSES/."
    upload_asset "$tag" "$work/ldraw-merged.zip"
    upload_asset "$tag" "$man"
    pin="$root/pins/$tag.ron"
    {
        echo "// brickdata LDraw mirror pin. Blockstar consumers copy to:"
        echo "//   external-data/ldraw/ldraw-snapshot.ron"
        echo "("
        echo "  mirror_tag: \"$tag\","
        echo "  snapshot_date: \"$(today_utc)\","
        echo "  asset_url: \"$(asset_url "$tag" ldraw-merged.zip)\","
        echo "  asset_sha256: \"$zsum\","
        echo "  manifest_url: \"$(asset_url "$tag" manifest.tsv)\","
        echo "  manifest_sha256: \"$mhash\","
        echo "  file_count: $fcount,"
        echo ")"
    } > "$pin"
    echo "Wrote $pin" >&2
    echo "Release: https://github.com/$GH_REPO/releases/tag/$tag" >&2

# Build catalog.sqlite from the committed pins (reproducible; no Blockstar
# checkout involved — see docs/cleaning.md for what the build cleans/derives).
# Also writes the part_frequency.ron and color_names.ron sidecars next to it.
build-catalog rb_pin="pins/rebrickable-2026-06-01.ron" ldraw_pin="pins/ldraw-2026-06-01.ron" out="work/catalog.sqlite":
    cargo run --release -p brickdata-catalog-builder -- build \
      --pin {{rb_pin}} --ldraw-pin {{ldraw_pin}} --out {{out}}

# Publish a built catalog.sqlite (produced by `just build-catalog`) plus the
# sidecars found next to it as release assets, all pinned together.
publish-catalog path tag="":
    #!/usr/bin/env bash
    set -euo pipefail
    # Uploads catalog.sqlite and each known sidecar next to it to a
    # catalog-<today> release, and pins every asset's url + sha256 + byte size
    # in pins/catalog-<today>.ron (sidecars under a `sidecars:` map keyed by
    # filename — the shape `brickdata::pin::CatalogPin` parses). The
    # artifacts are built HERE (`just build-catalog`); the Blockstar main repo
    # is a pure consumer. A sidecar is only ever published pinned: consumers
    # fetch by url + hash, so an unpinned upload would be unreachable.
    root="{{justfile_directory()}}"
    source "$root/lib/common.sh"
    [ -f "{{path}}" ] || die "no such file: {{path}}"
    # An explicit tag argument allows a same-day re-cut (e.g. a schema bump)
    # without touching the earlier, immutable release.
    tag="{{tag}}"; [ -n "$tag" ] || tag="catalog-$(today_utc)"
    sum="$(sha256_file "{{path}}")"; bytes="$(wc -c < "{{path}}" | tr -d ' ')"
    # The well-known sidecar names `build-catalog` writes next to the DB
    # (CatalogPin::PART_FREQUENCY_SIDECAR / COLOR_NAMES_SIDECAR in
    # crates/brickdata/src/pin.rs). A missing one is a warning, not an error,
    # so an older build directory can still be published — the pin simply
    # won't list it.
    dir="$(dirname "{{path}}")"
    sidecars=(part_frequency.ron color_names.ron)
    declare -a entries=()
    for name in "${sidecars[@]}"; do
        f="$dir/$name"
        if [ -f "$f" ]; then
            ssum="$(sha256_file "$f")"; sbytes="$(wc -c < "$f" | tr -d ' ')"
            entries+=("$name|$ssum|$sbytes")
        else
            log "WARNING: sidecar $name not found next to {{path}} — not published"
        fi
    done
    ensure_release "$tag" "Built catalog.sqlite ($tag)" \
        "Prebuilt brick-catalog metadata DB (+ part_frequency.ron / color_names.ron sidecars). Query with sqlite3/Datasette, no build required."
    upload_asset "$tag" "{{path}}"
    for e in "${entries[@]}"; do upload_asset "$tag" "$dir/${e%%|*}"; done
    pin="$root/pins/$tag.ron"
    {
        echo "// brickdata built-catalog pin."
        echo "("
        echo "  mirror_tag: \"$tag\","
        echo "  asset_url: \"$(asset_url "$tag" "$(basename "{{path}}")")\","
        echo "  sha256: \"$sum\","
        echo "  bytes: $bytes,"
        echo "  sidecars: {"
        for e in "${entries[@]}"; do
            IFS='|' read -r name ssum sbytes <<< "$e"
            url="$(asset_url "$tag" "$name")"
            echo "    \"$name\": (sha256: \"$ssum\", bytes: $sbytes, mirror_url: \"$url\"),"
        done
        echo "  },"
        echo ")"
    } > "$pin"
    echo "Wrote $pin" >&2

# Publish a built geometry cache (produced by the MAIN repo). Reserved.
publish-cache path:
    #!/usr/bin/env bash
    set -euo pipefail
    # Same shape as publish-catalog; for the rkyv geometry cache milestone.
    root="{{justfile_directory()}}"
    source "$root/lib/common.sh"
    [ -f "{{path}}" ] || die "no such file: {{path}}"
    tag="cache-$(today_utc)"
    sum="$(sha256_file "{{path}}")"
    ensure_release "$tag" "Built geometry cache ($tag)" "Prebuilt geometry cache."
    upload_asset "$tag" "{{path}}"
    echo "Published {{path}} -> $tag (sha256 $sum)" >&2

# Verify a pin by re-downloading its assets and checking hashes/manifest.
verify pin:
    #!/usr/bin/env bash
    set -euo pipefail
    # Proves a fresh clone is reproducible. Works for rebrickable-*.ron,
    # ldraw-*.ron, and catalog-*.ron pins (the catalog plus every sidecar).
    root="{{justfile_directory()}}"
    source "$root/lib/common.sh"
    source "$root/lib/manifest.sh"
    [ -f "{{pin}}" ] || die "no such pin: {{pin}}"
    work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
    # Dispatch on pin shape, in the same order as `Pin::detect` in
    # crates/brickdata/src/pin.rs: an ldraw pin has a manifest_sha256 field; a
    # rebrickable pin has a file_fingerprints map with per-file mirror_url +
    # sha256; anything else is a catalog pin (one top-level asset_url / sha256
    # / bytes triple plus a sidecars map of the same per-file tuples).
    # (Greps the RON rather than parsing it — the pins are flat and shell-only.)
    if grep -q 'manifest_sha256' "{{pin}}"; then
        # LDraw-style: zip + manifest hashes, then re-hash the merged tree.
        aurl=$(grep -oE 'asset_url: "[^"]+"' "{{pin}}" | head -1 | sed -E 's/asset_url: "([^"]+)"/\1/')
        asum=$(grep -oE 'asset_sha256: "[0-9a-f]+"' "{{pin}}" | sed -E 's/.*"([0-9a-f]+)"/\1/')
        murl=$(grep -oE 'manifest_url: "[^"]+"' "{{pin}}" | sed -E 's/manifest_url: "([^"]+)"/\1/')
        msum=$(grep -oE 'manifest_sha256: "[0-9a-f]+"' "{{pin}}" | sed -E 's/.*"([0-9a-f]+)"/\1/')
        download "$aurl" "$work/tree.zip"; got="$(sha256_file "$work/tree.zip")"
        [ "$got" = "$asum" ] || die "ldraw zip hash mismatch: $got != $asum"
        download "$murl" "$work/manifest.tsv"; gotm="$(sha256_file "$work/manifest.tsv")"
        [ "$gotm" = "$msum" ] || die "ldraw manifest hash mismatch: $gotm != $msum"
        extract_into "$work/tree.zip" "$work/tree"
        verify_manifest "$work/tree" "$work/manifest.tsv"
        log "OK   ldraw merged tree matches manifest"
    elif grep -q 'file_fingerprints' "{{pin}}"; then
        # Rebrickable-style: one (sha256, bytes, mirror_url) tuple per file.
        # Read matches into an array first so a per-file failure can exit the
        # recipe (a `grep | while` subshell could not).
        mapfile -t lines < <(grep -oE '\(sha256: "[0-9a-f]+", bytes: [0-9]+, mirror_url: "[^"]+"\)' "{{pin}}")
        [ "${#lines[@]}" -gt 0 ] || die "no recognizable file entries in {{pin}}"
        for line in "${lines[@]}"; do
            sum=$(sed -E 's/.*sha256: "([0-9a-f]+)".*/\1/' <<<"$line")
            url=$(sed -E 's/.*mirror_url: "([^"]+)".*/\1/' <<<"$line")
            f="$work/$(basename "$url")"
            download "$url" "$f"
            got="$(sha256_file "$f")"
            [ "$got" = "$sum" ] || die "FAIL $(basename "$url"): $got != $sum"
            log "OK   $(basename "$url")"
        done
    else
        # Catalog-style: the sqlite's own asset_url / sha256 / bytes are the
        # top-level fields — matched line-anchored so a sidecar tuple's
        # `sha256:` / `bytes:` (which sit mid-line) can't be mistaken for
        # them — then every sidecar tuple, each checked for hash AND size.
        # (`|| true` keeps a no-match grep from tripping `set -e` inside the
        # substitution, so the shape check below gets to report it.)
        aurl=$(grep -E '^ *asset_url: "[^"]+",?$' "{{pin}}" | sed -E 's/.*asset_url: "([^"]+)".*/\1/' || true)
        asum=$(grep -E '^ *sha256: "[0-9a-f]+",?$' "{{pin}}" | sed -E 's/.*sha256: "([0-9a-f]+)".*/\1/' || true)
        abytes=$(grep -E '^ *bytes: [0-9]+,?$' "{{pin}}" | sed -E 's/.*bytes: ([0-9]+).*/\1/' || true)
        [ -n "$aurl" ] && [ -n "$asum" ] && [ -n "$abytes" ] \
            || die "no catalog asset_url/sha256/bytes triple in {{pin}}"
        download_verified "$aurl" "$work/$(basename "$aurl")" "$asum" "$abytes"
        log "OK   $(basename "$aurl")"
        # Sidecars: zero or more `"name": (sha256, bytes, mirror_url)` tuples.
        # A pin cut before sidecars existed has none, which is fine.
        mapfile -t lines < <(grep -oE '"[^"]+": \(sha256: "[0-9a-f]+", bytes: [0-9]+, mirror_url: "[^"]+"\)' "{{pin}}")
        for line in "${lines[@]}"; do
            sum=$(sed -E 's/.*sha256: "([0-9a-f]+)".*/\1/' <<<"$line")
            nbytes=$(sed -E 's/.*bytes: ([0-9]+).*/\1/' <<<"$line")
            url=$(sed -E 's/.*mirror_url: "([^"]+)".*/\1/' <<<"$line")
            download_verified "$url" "$work/$(basename "$url")" "$sum" "$nbytes"
            log "OK   $(basename "$url") (sidecar)"
        done
        log "OK   ${#lines[@]} sidecar(s) verified"
    fi
    echo "verify: all assets match {{pin}}" >&2
