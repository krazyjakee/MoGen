#!/usr/bin/env bash
# Builds the gh-pages static site from the markdown sources in docs/.
# Output is written to _site/ at the repo root.
#
# Usage:  ./site/build.sh
# CI uses this same script — keep it portable.

set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SITE="$ROOT/site"
WEB="$ROOT/web"
OUT="$ROOT/_site"
PANDOC="${PANDOC:-pandoc}"
WASM_PACK="${WASM_PACK:-wasm-pack}"
NPM="${NPM:-npm}"

# `SKIP_PLAYGROUND=1 ./site/build.sh` skips the wasm + Vite build entirely —
# useful for fast docs-only iteration when you don't have the Rust wasm target
# or Node installed locally.
SKIP_PLAYGROUND="${SKIP_PLAYGROUND:-0}"

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

# Playground: build the wasm crate, then the Vite app, then drop the static
# bundle into _site/playground/. The home page (and shared template nav) link
# to playground/index.html — keep that path stable.
if [ "$SKIP_PLAYGROUND" = "1" ]; then
  echo "skipping playground build (SKIP_PLAYGROUND=1)"
elif ! command -v "$WASM_PACK" >/dev/null 2>&1; then
  echo "warning: wasm-pack not found — skipping playground build" >&2
  echo "  install with: cargo install wasm-pack" >&2
elif ! command -v "$NPM" >/dev/null 2>&1; then
  echo "warning: npm not found — skipping playground build" >&2
else
  echo "building playground -> $OUT/playground"

  # 1. Compile mogen-wasm to JS + .wasm under web/wasm/. The Vite entry point
  #    in web/src/main.ts imports from `../wasm/mogen_wasm.js`; keeping the
  #    out-dir layout matches the dev-server flow exactly.
  echo "  -> wasm-pack build crates/mogen-wasm"
  "$WASM_PACK" build "$ROOT/crates/mogen-wasm" \
    --target web \
    --out-dir "$WEB/wasm" \
    --out-name mogen_wasm \
    --release

  # 2. Vite bundle. Use `npm ci` when a lockfile is present so CI gets a
  #    deterministic install; fall back to `npm install` for first-time local
  #    runs that may not have a lockfile yet.
  if [ -f "$WEB/package-lock.json" ]; then
    (cd "$WEB" && "$NPM" ci)
  else
    (cd "$WEB" && "$NPM" install)
  fi
  (cd "$WEB" && "$NPM" run build)

  # 3. Publish the bundle.
  mkdir -p "$OUT/playground"
  cp -R "$WEB/dist/." "$OUT/playground/"
fi

# .nojekyll prevents GitHub Pages from running Jekyll on the output.
touch "$OUT/.nojekyll"

echo "done."
