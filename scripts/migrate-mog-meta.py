#!/usr/bin/env python3
"""One-off migration: rewrite legacy .mog comment headers
(`// mogen-generate seed=...`, `// mogen-generate thinking=...`, `// prompt: ...`)
into the new `meta(seed=..., thinking=..., prompt=...)` block.

Defaults to migrating `examples/` and `/Users/jake/Documents/mogen_assetpacks`.
Safe to delete after one successful run.

Usage:
    scripts/migrate-mog-meta.py             # migrate default paths
    scripts/migrate-mog-meta.py --dry-run   # preview only
    scripts/migrate-mog-meta.py path1 path2 # migrate custom paths
"""

import argparse
import os
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_PATHS = [
    REPO_ROOT / "examples",
    Path("/Users/jake/Documents/mogen_assetpacks"),
]

LEGACY_SEED = "// mogen-generate seed="
LEGACY_THINKING = "// mogen-generate thinking="
LEGACY_PROMPT = "// prompt:"


def sanitise(value: str) -> str:
    """Mirror crates/mogen-dsl/src/meta.rs::escape_dsl_string — the DSL
    grammar has no string escapes, so unsafe characters get substituted."""
    out = []
    for ch in value:
        if ch == '"':
            out.append("'")
        elif ch == "\\":
            out.append("/")
        elif ch in "\n\r\t":
            out.append(" ")
        else:
            out.append(ch)
    return "".join(out).strip()


def scan_legacy(text: str):
    """Return (seed, thinking, prompt) parsed from leading comment header.
    Each is None when absent. Stops at the first non-comment, non-blank line."""
    seed = thinking = prompt = None
    for raw in text.splitlines()[:16]:
        line = raw.lstrip()
        if line.startswith(LEGACY_SEED):
            seed = line[len(LEGACY_SEED):].strip()
        elif line.startswith(LEGACY_THINKING):
            thinking = line[len(LEGACY_THINKING):].strip()
        elif line.startswith(LEGACY_PROMPT):
            prompt = line[len(LEGACY_PROMPT):].strip()
        elif line and not line.startswith("//"):
            break
    return seed, thinking, prompt


def strip_legacy_header(text: str) -> str:
    """Remove leading legacy header lines from text. Stops the first time
    it sees real DSL content; later occurrences (inside string literals or
    trailing comments) are left alone."""
    out_lines = []
    header_active = True
    for raw in text.splitlines(keepends=True):
        if header_active:
            line = raw.lstrip()
            if (line.startswith(LEGACY_SEED)
                    or line.startswith(LEGACY_THINKING)
                    or line.startswith(LEGACY_PROMPT)):
                continue
            body = line.rstrip("\r\n")
            if body and not body.startswith("//"):
                header_active = False
        out_lines.append(raw)
    return "".join(out_lines)


def find_meta_block(text: str):
    """Return (open_paren_idx, close_paren_idx) of the top-level meta(...)
    block, or None when absent. open_paren_idx points at the `(`."""
    n = len(text)
    i = 0
    while i < n:
        while i < n and text[i] in " \t\r\n":
            i += 1
        if i >= n:
            return None
        if text.startswith("//", i):
            while i < n and text[i] != "\n":
                i += 1
            continue
        break
    if not text.startswith("meta", i):
        return None
    j = i + 4
    if j < n and (text[j].isalnum() or text[j] == "_"):
        return None
    while j < n and text[j] in " \t":
        j += 1
    if j >= n or text[j] != "(":
        return None
    open_idx = j
    depth = 0
    in_string = False
    k = open_idx
    while k < n:
        c = text[k]
        if in_string:
            if c == '"':
                in_string = False
        else:
            if c == '"':
                in_string = True
            elif c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if depth == 0:
                    return (open_idx, k)
        k += 1
    return None


def upsert_in_inner(inner: str, key: str, value: str) -> str:
    """Replace `key = ...` (up to next top-level comma) or append it."""
    n = len(inner)
    i = 0
    # Walk attrs.
    while i < n:
        # Skip whitespace and comments.
        while i < n and inner[i] in " \t\r\n":
            i += 1
        if i + 1 < n and inner[i] == "/" and inner[i + 1] == "/":
            while i < n and inner[i] != "\n":
                i += 1
            continue
        # Read ident.
        start = i
        while i < n and (inner[i].isalnum() or inner[i] == "_"):
            i += 1
        ident = inner[start:i]
        # Find the next top-level comma.
        j = i
        depth = 0
        in_string = False
        while j < n:
            c = inner[j]
            if in_string:
                if c == '"':
                    in_string = False
            else:
                if c == '"':
                    in_string = True
                elif c in "([":
                    depth += 1
                elif c in ")]":
                    depth -= 1
                elif c == "," and depth == 0:
                    break
            j += 1
        if ident == key:
            new_attr = f'{key} = "{value}"'
            return inner[:start] + new_attr + inner[j:]
        # Skip past the comma to continue.
        if j < n and inner[j] == ",":
            i = j + 1
        else:
            i = j
    # Not found — append.
    trimmed_end = inner.rstrip()
    trailing = inner[len(trimmed_end):]
    sep = ""
    if trimmed_end and not trimmed_end.endswith(","):
        sep = ","
    if trimmed_end:
        sep += " "
    return f'{trimmed_end}{sep}{key} = "{value}"{trailing}'


def upsert_meta(text: str, attrs: dict) -> str:
    """Insert or update each (key, value) in attrs inside the meta(...) block.
    Creates a fresh block at the top of the file if none exists."""
    block = find_meta_block(text)
    if block is None:
        # Find the insertion point: after any leading `//` comments / blank lines.
        n = len(text)
        i = 0
        while i < n:
            line_end = text.find("\n", i)
            if line_end < 0:
                line_end = n
            line = text[i:line_end]
            stripped = line.strip()
            if stripped == "" or stripped.startswith("//"):
                i = line_end + 1 if line_end < n else line_end
                continue
            break
        body = "\n".join(f'  {k} = "{sanitise(v)}",' for k, v in attrs.items())
        block_text = f"meta (\n{body}\n)\n\n"
        return text[:i] + block_text + text[i:]
    open_idx, close_idx = block
    inner = text[open_idx + 1:close_idx]
    for key, value in attrs.items():
        inner = upsert_in_inner(inner, key, sanitise(value))
    return text[:open_idx + 1] + inner + text[close_idx:]


def migrate_file(path: Path, dry_run: bool) -> bool:
    src = path.read_text()
    seed, thinking, prompt = scan_legacy(src)
    if seed is None and thinking is None and prompt is None:
        return False
    attrs = {}
    if seed is not None:
        attrs["seed"] = seed
    if thinking is not None:
        attrs["thinking"] = thinking
    if prompt is not None:
        attrs["prompt"] = prompt
    out = strip_legacy_header(src)
    out = upsert_meta(out, attrs)
    if out == src:
        return False
    if not dry_run:
        path.write_text(out)
    return True


def collect_mog_files(root: Path):
    if not root.exists():
        print(f"warn: {root} does not exist", file=sys.stderr)
        return
    if root.is_file():
        if root.suffix == ".mog":
            yield root
        return
    for dirpath, _, filenames in os.walk(root):
        for fn in filenames:
            if fn.endswith(".mog"):
                yield Path(dirpath) / fn


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("paths", nargs="*", help="files or directories (default: examples + assetpacks)")
    ap.add_argument("--dry-run", "-n", action="store_true",
                    help="report which files would change without writing them")
    args = ap.parse_args()

    roots = [Path(p) for p in args.paths] or DEFAULT_PATHS
    scanned = migrated = 0
    for root in roots:
        for path in collect_mog_files(root):
            scanned += 1
            try:
                if migrate_file(path, args.dry_run):
                    migrated += 1
                    verb = "would migrate" if args.dry_run else "migrated"
                    print(f"{verb} {path}")
            except OSError as e:
                print(f"error: {path}: {e}", file=sys.stderr)
    verb = "would migrate" if args.dry_run else "migrated"
    print(f"\nscanned {scanned} .mog file(s), {verb} {migrated}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
