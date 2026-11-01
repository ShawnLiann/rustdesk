#!/bin/bash
# Print the hwcodec source tree that Cargo builds this app against.
#
# Cargo.toml pins hwcodec to a git rev, so the wrapper's C++ lives in Cargo's checkout cache
# and not in this repository. Anything that compiles that C++ on its own -- this directory's
# verify script, the CI smoke job -- has to find the same tree, and "the same tree" means the
# rev recorded in Cargo.lock.
#
#   resolve-hwcodec-root.sh [--fetch]
#
# Order: $HWCODEC_ROOT, the cargo checkout for the pinned rev, then a clone of that rev into
# $TMPDIR. The clone happens only with --fetch, so a local run never quietly tests a tree other
# than the one Cargo just built.
set -uo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
LOCK="$REPO/Cargo.lock"
MARKER=src/ffmpeg_ram/vt_pixbuf_encode.mm

usable() { [ -n "${1:-}" ] && [ -f "$1/$MARKER" ]; }

if usable "${HWCODEC_ROOT:-}"; then
  echo "$HWCODEC_ROOT"; exit 0
fi

SOURCE=$(awk '/^name = "hwcodec"$/ {inpkg=1} inpkg && /^source = / {print $3; exit}' "$LOCK" | tr -d '"')
REV=${SOURCE##*#}
if [ -z "${REV:-}" ] || [ "$REV" = "$SOURCE" ]; then
  echo "cannot read the hwcodec rev out of $LOCK" >&2; exit 1
fi
URL=${SOURCE#git+}; URL=${URL%%\?*}

for dir in "${CARGO_HOME:-$HOME/.cargo}"/git/checkouts/hwcodec-*/*/; do
  dir=${dir%/}
  usable "$dir" || continue
  [ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" = "$REV" ] && { echo "$dir"; exit 0; }
done

if [ "${1:-}" = "--fetch" ]; then
  DEST=$(mktemp -d "${TMPDIR:-/tmp}/hwcodec-$REV.XXXXXX")
  rmdir "$DEST"
  git init -q "$DEST" &&
    git -C "$DEST" fetch -q --depth=1 "$URL" "$REV" &&
    git -C "$DEST" checkout -q FETCH_HEAD || { echo "cannot fetch $URL $REV" >&2; exit 1; }
  usable "$DEST" && { echo "$DEST"; exit 0; }
fi

echo "hwcodec rev $REV is not checked out under ${CARGO_HOME:-$HOME/.cargo}/git/checkouts" >&2
echo "run 'cargo fetch' once, set HWCODEC_ROOT, or pass --fetch" >&2
exit 1
