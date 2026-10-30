#!/usr/bin/env bash
# Rebuild the vendored hwcodec tree from its recorded upstream commit and list every difference.
#
# A vendored directory is only reviewable if a reviewer can answer "what exactly did you change"
# without trusting a document. This script answers it mechanically: anything outside the allowlist
# below is reported as unexpected and exits non-zero.
#
#   libs/hwcodec-vendor/scripts/check-vendor-provenance.sh          # check
#   libs/hwcodec-vendor/scripts/check-vendor-provenance.sh --show   # also print the diffs
set -euo pipefail

UPSTREAM="${HWCODEC_UPSTREAM:-https://github.com/ShawnLiann/hwcodec}"
REV="${HWCODEC_REV:-94d44e604820fd0f17cbe1a43177bc0d5ab76b23}"

HERE=$(cd "$(dirname "$0")/.." && pwd)
SHOW=0
[ "${1:-}" = "--show" ] && SHOW=1

# Paths this branch may add or change on top of $REV. Keep in sync with VENDORING.md.
MODIFIED="build.rs src/ffmpeg_ram/encode.rs src/ffmpeg_ram/mod.rs
cpp/common/platform/mac/mac.mm cpp/ffmpeg_ram/ffmpeg_ram_encode.cpp"
ADDED="scripts/check-vendor-provenance.sh scripts/verify-encode-zerocopy.sh scripts/encode_synth3.m
src/ffmpeg_ram/test_vt_pixbuf.m src/ffmpeg_ram/vt_pixbuf.rs
src/ffmpeg_ram/vt_pixbuf_encode.h src/ffmpeg_ram/vt_pixbuf_encode.mm"

TMP=$(mktemp -d "${TMPDIR:-/tmp}/hwcodec-provenance.XXXXXX")
trap 'rm -rf "$TMP"' EXIT

echo "upstream $UPSTREAM"
echo "rev      $REV"
git init -q "$TMP"
git -C "$TMP" fetch -q --depth=1 "$UPSTREAM" "$REV"
git -C "$TMP" checkout -q FETCH_HEAD
echo "checked out $(git -C "$TMP" log --oneline -1)"
echo

in_list() {
    for item in $2; do [ "$1" = "$item" ] && return 0; done
    return 1
}

files_of() { # sorted relative paths, excluding VCS and build output
    ( cd "$1" && find . -type f -not -path './.git/*' -not -path './target/*' \
        -not -name 'VENDORING.md' | sed 's|^\./||' | sort )
}

unexpected=0
LISTS=$(mktemp -d "${TMPDIR:-/tmp}/hwcodec-provenance-lists.XXXXXX")
trap 'rm -rf "$TMP" "$LISTS"' EXIT
files_of "$TMP"  > "$LISTS/up"
files_of "$HERE" > "$LISTS/mine"

# Added: in our tree, not upstream.
while IFS= read -r rel; do
    if ! grep -qxF "$rel" "$LISTS/up"; then
        if in_list "$rel" "$ADDED"; then
            echo "added      $rel"
            [ "$SHOW" = 1 ] && sed -n '1,60p' "$HERE/$rel"
        else
            echo "UNEXPECTED added: $rel"; unexpected=1
        fi
    fi
done < "$LISTS/mine"

# Missing: upstream has it, we do not.
while IFS= read -r rel; do
    grep -qxF "$rel" "$LISTS/mine" || { echo "UNEXPECTED missing: $rel"; unexpected=1; }
done < "$LISTS/up"

# Modified: present in both, bytes differ.
while IFS= read -r rel; do
    grep -qxF "$rel" "$LISTS/mine" || continue
    cmp -s "$TMP/$rel" "$HERE/$rel" && continue
    if in_list "$rel" "$MODIFIED"; then
        echo "modified   $rel"
        [ "$SHOW" = 1 ] && { diff -u "$TMP/$rel" "$HERE/$rel" || true; }
    else
        echo "UNEXPECTED modified: $rel"; unexpected=1
    fi
done < "$LISTS/up"

echo
if [ "$unexpected" = 1 ]; then
    echo "FAIL: the vendored tree differs from $REV in ways VENDORING.md does not describe"
    exit 1
fi
echo "OK: vendor == $REV + the deltas listed in VENDORING.md"
