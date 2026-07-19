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
#   catalog-YYYY-MM-DD       built catalog.sqlite (output, `just build-catalog`)

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
build-catalog rb_pin="pins/rebrickable-2026-06-01.ron" ldraw_pin="pins/ldraw-2026-06-01.ron" out="work/catalog.sqlite":
    cargo run --release -p brickdata-catalog-builder -- build \
      --pin {{rb_pin}} --ldraw-pin {{ldraw_pin}} --out {{out}}

# Publish a built catalog.sqlite (produced by `just build-catalog`) as a
# release asset.
publish-catalog path tag="":
    #!/usr/bin/env bash
    set -euo pipefail
    # Uploads to a catalog-<today> release and pins its sha256 in
    # pins/catalog-<today>.ron. The artifact is built HERE (`just
    # build-catalog`); the Blockstar main repo is a pure consumer.
    root="{{justfile_directory()}}"
    source "$root/lib/common.sh"
    [ -f "{{path}}" ] || die "no such file: {{path}}"
    # An explicit tag argument allows a same-day re-cut (e.g. a schema bump)
    # without touching the earlier, immutable release.
    tag="{{tag}}"; [ -n "$tag" ] || tag="catalog-$(today_utc)"
    sum="$(sha256_file "{{path}}")"; bytes="$(wc -c < "{{path}}" | tr -d ' ')"
    ensure_release "$tag" "Built catalog.sqlite ($tag)" \
        "Prebuilt brick-catalog metadata DB. Query with sqlite3/Datasette, no build required."
    upload_asset "$tag" "{{path}}"
    pin="$root/pins/$tag.ron"
    {
        echo "// brickdata built-catalog pin."
        echo "("
        echo "  mirror_tag: \"$tag\","
        echo "  asset_url: \"$(asset_url "$tag" "$(basename "{{path}}")")\","
        echo "  sha256: \"$sum\","
        echo "  bytes: $bytes,"
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
    # Proves a fresh clone is reproducible. Works for rebrickable-*.ron and
    # ldraw-*.ron pins.
    root="{{justfile_directory()}}"
    source "$root/lib/common.sh"
    source "$root/lib/manifest.sh"
    [ -f "{{pin}}" ] || die "no such pin: {{pin}}"
    work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
    # Dispatch on pin shape: an ldraw pin has a manifest_sha256 field; a
    # rebrickable pin has a files map with per-file mirror_url + sha256.
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
    else
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
    fi
    echo "verify: all assets match {{pin}}" >&2
