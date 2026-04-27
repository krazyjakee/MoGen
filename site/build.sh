#!/usr/bin/env bash
# Builds the gh-pages static site from the markdown sources in docs/.
# Output is written to _site/ at the repo root.
#
# Usage:  ./site/build.sh
# CI uses this same script — keep it portable.

set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SITE="$ROOT/site"
OUT="$ROOT/_site"
PANDOC="${PANDOC:-pandoc}"

if ! command -v "$PANDOC" >/dev/null 2>&1; then
  echo "error: pandoc not found on PATH (set PANDOC=/path/to/pandoc to override)" >&2
  exit 1
fi

rm -rf "$OUT"
mkdir -p "$OUT/assets"

# Static assets
cp -R "$SITE/assets/." "$OUT/assets/"
cp "$ROOT/assets/icon.png"   "$OUT/icon.png"
cp "$ROOT/assets/splash.png" "$OUT/splash.png"

# Gallery media + Studio screenshots (referenced from home.html).
mkdir -p "$OUT/gallery" "$OUT/screenshots"
if [ -d "$SITE/gallery" ]; then
  cp -R "$SITE/gallery/." "$OUT/gallery/"
fi
if [ -d "$SITE/screenshots" ]; then
  cp -R "$SITE/screenshots/." "$OUT/screenshots/"
fi

# Home page is hand-crafted (hero, dynamic downloads, build-from-source).
cp "$SITE/home.html" "$OUT/index.html"

# Reference pages: pandoc each markdown source into the shared template.
build_md() {
  local md="$1" out="$2" title="$3" navlabel="$4" desc="$5"
  echo "  -> $(basename "$out")"
  "$PANDOC" "$md" \
    --from gfm \
    --to html5 \
    --standalone \
    --template "$SITE/template.html" \
    --toc --toc-depth=3 \
    --no-highlight \
    --wrap=none \
    --metadata title="$title" \
    --metadata navlabel="$navlabel" \
    --metadata description="$desc" \
    -o "$out"
}

echo "building gh-pages site -> $OUT"
build_md "$ROOT/docs/dsl.md"     "$OUT/dsl.html"     "DSL reference"     "DSL reference"   "Every node kind, attribute, and feature in the .mog DSL."
build_md "$ROOT/docs/cli.md"     "$OUT/cli.html"     "CLI reference"     "CLI reference"   "Every mogen subcommand and flag, with examples."
build_md "$ROOT/docs/studio.md"  "$OUT/studio.html"  "MoGen Studio"      "Studio guide"    "MoGen Studio — the desktop editor for .mog scenes."

# .nojekyll prevents GitHub Pages from running Jekyll on the output.
touch "$OUT/.nojekyll"

echo "done."
