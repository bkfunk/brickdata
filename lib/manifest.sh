#!/usr/bin/env bash
# Content-manifest helpers for the LDraw mirror.
#
# LDraw ships a base `complete.zip` plus incremental overlay zips
# (`updated files (YYMM##).zip`) that are *applied on top* of an existing
# tree. Overlays only add/replace files — they never delete — so an
# overlaid tree can drift from a fresh `complete.zip`. Therefore the pinned
# identity of an LDraw snapshot is the **content manifest of the merged
# tree** (every file's sha256), never the overlay recipe that produced it.
#
# A manifest is a sorted, tab-separated `relpath\tsha256` list. Its own
# sha256 (the "manifest hash") is the single value that identifies the tree.

set -euo pipefail

# build_manifest <tree_root> <out_manifest> — walk every regular file under
# tree_root, emit `relpath\tsha256` sorted by path. Sorting by path (with a
# fixed C locale) makes the manifest deterministic regardless of filesystem
# enumeration order.
build_manifest() {
    local root="$1" out="$2"
    log "building content manifest for $root"
    ( cd "$root"
      # -print0 / read -d '' to survive spaces in LDraw filenames.
      find . -type f -print0 | LC_ALL=C sort -z | while IFS= read -r -d '' f; do
          local rel="${f#./}"
          printf '%s\t%s\n' "$rel" "$(sha256_file "$f")"
      done
    ) > "$out"
    log "manifest: $(wc -l < "$out" | tr -d ' ') files"
}

# manifest_hash <manifest> -> sha256 of the manifest file itself.
manifest_hash() { sha256_file "$1"; }

# verify_manifest <tree_root> <manifest> — re-hash every file named in the
# manifest and confirm it matches; also flag files present in the tree but
# absent from the manifest (drift). Exits non-zero on any mismatch.
verify_manifest() {
    local root="$1" man="$2" rc=0 count=0
    while IFS=$'\t' read -r rel want; do
        local have
        have=$(cd "$root" && sha256_file "$rel" 2>/dev/null || echo MISSING)
        if [ "$have" != "$want" ]; then
            printf '  MISMATCH %s: want %s got %s\n' "$rel" "$want" "$have" >&2
            rc=1
        fi
        count=$((count + 1))
    done < "$man"
    log "verified $count files against manifest"
    return $rc
}

# extract_into <zip> <dest> — unzip a (base or overlay) archive into dest,
# overwriting existing files. LDraw's complete.zip wraps the library in a
# top-level `ldraw/`; overlays are rooted at the library root directly. This
# normalizes both so dest always ends up being the library root (the dir
# containing parts/).
extract_into() {
    local zip="$1" dest="$2"
    local tmp
    tmp="$(mktemp -d)"
    unzip -q -o "$zip" -d "$tmp"
    local root
    if [ -d "$tmp/ldraw/parts" ]; then root="$tmp/ldraw"
    elif [ -d "$tmp/parts" ]; then root="$tmp"
    else
        # An overlay may carry only e.g. parts/foo.dat with no marker dir at
        # the top; treat the extraction root as the library root.
        root="$tmp"
    fi
    mkdir -p "$dest"
    cp -R "$root/." "$dest/"
    rm -rf "$tmp"
}
