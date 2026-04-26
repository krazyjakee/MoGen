#!/bin/bash
set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

usage() {
    echo "Usage: $0 <new-version>"
    echo "Example: $0 0.2.0"
    echo ""
    echo "Updates the workspace version in Cargo.toml. All crates under"
    echo "crates/ inherit via 'version.workspace = true', so this is the"
    echo "only source of truth. Cargo.lock is regenerated to match."
    exit 1
}

# Run from the repo root regardless of where the script was invoked.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

if [ $# -ne 1 ]; then
    usage
fi

NEW_VERSION=$1

if ! [[ $NEW_VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
    echo -e "${RED}Error: Invalid version format. Use semver (e.g., 0.2.0 or 0.2.0-beta.1)${NC}"
    exit 1
fi

ROOT_CARGO="Cargo.toml"
if [ ! -f "$ROOT_CARGO" ]; then
    echo -e "${RED}Error: $ROOT_CARGO not found at $REPO_ROOT${NC}"
    exit 1
fi

# Read the current version from inside [workspace.package] only — never from
# a [workspace.dependencies] line that might also start with `version =`.
CURRENT_VERSION=$(perl -ne 'if (/^\[workspace\.package\]/.../^\[/) { if (/^version\s*=\s*"([^"]+)"/) { print $1; exit } }' "$ROOT_CARGO")

if [ -z "$CURRENT_VERSION" ]; then
    echo -e "${RED}Error: Could not find [workspace.package] version in $ROOT_CARGO${NC}"
    exit 1
fi

if [ "$CURRENT_VERSION" = "$NEW_VERSION" ]; then
    echo -e "${YELLOW}Version is already ${NEW_VERSION}; nothing to do.${NC}"
    exit 0
fi

echo -e "${YELLOW}Current version: ${CURRENT_VERSION}${NC}"
echo -e "${YELLOW}New version:     ${NEW_VERSION}${NC}"
echo ""

read -p "Proceed with version bump? (y/n) " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Aborted."
    exit 0
fi

echo ""
echo "Updating $ROOT_CARGO …"

# Replace the version line, but only inside the [workspace.package] table —
# never touch dependency version pins elsewhere in the file.
perl -i -pe '
    if (/^\[workspace\.package\]/) { $in_pkg = 1 }
    elsif (/^\[/)                  { $in_pkg = 0 }
    s/^(version\s*=\s*")[^"]+(")/${1}'"$NEW_VERSION"'${2}/ if $in_pkg;
' "$ROOT_CARGO"

# Verify the edit landed.
WROTE_VERSION=$(perl -ne 'if (/^\[workspace\.package\]/.../^\[/) { if (/^version\s*=\s*"([^"]+)"/) { print $1; exit } }' "$ROOT_CARGO")
if [ "$WROTE_VERSION" != "$NEW_VERSION" ]; then
    echo -e "${RED}Error: $ROOT_CARGO version did not update (still $WROTE_VERSION)${NC}"
    exit 1
fi

echo "Regenerating Cargo.lock …"
cargo generate-lockfile

echo ""
echo -e "${GREEN}✓ Version bumped from ${CURRENT_VERSION} to ${NEW_VERSION}${NC}"
echo ""
echo "Changed files:"
git diff --name-only Cargo.toml Cargo.lock 2>/dev/null || echo "  (git not available or no changes detected)"
echo ""
echo "Next steps:"
echo "  1. Review changes:   git diff Cargo.toml Cargo.lock"
echo "  2. Commit:           git commit -am \"Bump version to ${NEW_VERSION}\""
echo "  3. Tag:              git tag v${NEW_VERSION}"
echo "  4. Push w/ tags:     git push && git push --tags"
