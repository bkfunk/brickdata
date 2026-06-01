#!/usr/bin/env bash
# Shared helpers for the blockstar-data mirror recipes.
#
# Pure shell — depends only on curl, sha256sum (or shasum), gh, unzip, and
# coreutils. No Rust, no main-checkout. Source this from a recipe:
#
#     source "$(dirname "$0")/lib/common.sh"   # (recipes pass the repo root)
#
# Conventions:
#   - GH_REPO is the data repo these releases live on (override for testing).
#   - All functions log to stderr and exit non-zero on failure (callers run
#     under `set -euo pipefail`).

set -euo pipefail

GH_REPO="${GH_REPO:-bkfunk/blockstar-data}"

log() { printf '  %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# sha256_file <path> -> lowercase hex digest on stdout.
# Prefers coreutils sha256sum; falls back to macOS `shasum -a 256`.
sha256_file() {
    local f="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$f" | awk '{print $1}'
    else
        shasum -a 256 "$f" | awk '{print $1}'
    fi
}

# download <url> <dest> — fetch a URL to a file, failing loudly on HTTP error.
# -f makes curl exit non-zero on 4xx/5xx; -S shows errors; -L follows redirects.
download() {
    local url="$1" dest="$2"
    log "GET $url"
    curl -fSL --retry 3 --progress-bar "$url" -o "$dest" \
        || die "download failed: $url"
}

# require_gzip <path> — reject an HTML error page masquerading as a .gz.
# Rebrickable's CDN returns 200 + HTML on some failures; the gzip magic
# bytes (1f 8b) are the reliable signal. Mirrors the Rust fetch.rs guard.
require_gzip() {
    local f="$1"
    local magic
    magic=$(head -c 2 "$f" | xxd -p 2>/dev/null || true)
    [ "$magic" = "1f8b" ] || die "$f: not a gzip stream (got magic '$magic') — likely an upstream error page"
}

# release_exists <tag> -> 0 if the release already exists on GH_REPO.
release_exists() {
    gh release view "$1" --repo "$GH_REPO" >/dev/null 2>&1
}

# ensure_release <tag> <title> <notes> — create the release if absent
# (idempotent). Releases are immutable once cut, so this never edits an
# existing one's metadata.
ensure_release() {
    local tag="$1" title="$2" notes="$3"
    if release_exists "$tag"; then
        log "release $tag already exists"
    else
        log "creating release $tag"
        gh release create "$tag" --repo "$GH_REPO" --title "$title" --notes "$notes"
    fi
}

# upload_asset <tag> <path> — upload (clobber) one asset onto a release.
upload_asset() {
    local tag="$1" path="$2"
    log "upload $(basename "$path") -> $tag"
    gh release upload "$tag" "$path" --repo "$GH_REPO" --clobber
}

# asset_url <tag> <filename> — the stable public download URL for an asset.
asset_url() {
    local tag="$1" name="$2"
    printf 'https://github.com/%s/releases/download/%s/%s' "$GH_REPO" "$tag" "$name"
}

# today_utc -> YYYY-MM-DD in UTC, for release tags.
today_utc() { date -u +%Y-%m-%d; }
