#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Install the `mogen` CLI and `mogen-studio` GUI from GitHub Releases.

Usage:
  curl -fsSL https://raw.githubusercontent.com/krazyjakee/model-gen/master/scripts/install.sh | bash

Options:
  --version VERSION      Version to install (e.g. "v0.1.0"). Default: latest.
  --repo OWNER/REPO      GitHub repo to install from. Default: krazyjakee/model-gen.
  --prefix PATH          Install into PATH/bin (default prefix: $HOME/.local).
  --bin-dir PATH         Install directory for the binaries (overrides --prefix/bin).
  --cli-only             Install only the `mogen` CLI (skip `mogen-studio`).
  --studio-only          Install only the `mogen-studio` GUI (skip `mogen`).
  --force                Overwrite existing binaries, if present.
  --no-verify            Skip checksum verification (not recommended).
  -h, --help             Show this help.

Examples:
  curl -fsSL https://raw.githubusercontent.com/krazyjakee/model-gen/master/scripts/install.sh | bash
  curl -fsSL https://raw.githubusercontent.com/krazyjakee/model-gen/master/scripts/install.sh | bash -s -- --version v0.1.0
  curl -fsSL https://raw.githubusercontent.com/krazyjakee/model-gen/master/scripts/install.sh | bash -s -- --bin-dir "$HOME/.local/bin" --force
  curl -fsSL https://raw.githubusercontent.com/krazyjakee/model-gen/master/scripts/install.sh | bash -s -- --cli-only
EOF
}

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

normalize_tag() {
  local v="$1"
  if [[ "$v" == v* ]]; then
    echo "$v"
  else
    echo "v$v"
  fi
}

http_get() {
  curl -fsSL "$1"
}

resolve_latest_tag() {
  local repo="$1"

  local json
  if json="$(http_get "https://api.github.com/repos/${repo}/releases/latest" 2>/dev/null)"; then
    local tag
    tag="$(
      printf '%s' "$json" |
        sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*$/\1/p' |
        head -n 1
    )"
    if [[ -n "${tag:-}" ]]; then
      echo "$tag"
      return 0
    fi
  fi

  local final_url
  final_url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${repo}/releases/latest")"
  if [[ "$final_url" =~ /tag/([^/]+)$ ]]; then
    echo "${BASH_REMATCH[1]}"
    return 0
  fi

  echo "Could not resolve latest release tag for ${repo}" >&2
  exit 1
}

detect_target() {
  local os arch
  os="$(uname -s 2>/dev/null || true)"
  arch="$(uname -m 2>/dev/null || true)"

  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64) echo "aarch64-apple-darwin" ;;
        x86_64) echo "x86_64-apple-darwin" ;;
        *) echo "Unsupported macOS arch: $arch" >&2; exit 1 ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64) echo "x86_64-unknown-linux-gnu" ;;
        *) echo "Unsupported Linux arch: $arch (release builds are x86_64-only)" >&2; exit 1 ;;
      esac
      ;;
    MINGW*|MSYS*|CYGWIN*)
      case "$arch" in
        x86_64|amd64) echo "x86_64-pc-windows-msvc" ;;
        *) echo "Unsupported Windows arch: $arch" >&2; exit 1 ;;
      esac
      ;;
    *)
      echo "Unsupported OS: $os" >&2
      exit 1
      ;;
  esac
}

is_windows_target() {
  [[ "$1" == *"-pc-windows-"* ]]
}

verify_sha256() {
  local file="$1"
  local expected="$2"

  if command -v shasum >/dev/null 2>&1; then
    local actual
    actual="$(shasum -a 256 "$file" | awk '{print $1}')"
    [[ "$actual" == "$expected" ]]
    return
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    local actual
    actual="$(sha256sum "$file" | awk '{print $1}')"
    [[ "$actual" == "$expected" ]]
    return
  fi

  echo "No sha256 tool found (need shasum or sha256sum)" >&2
  exit 1
}

unpack_archive() {
  # Usage: unpack_archive <archive_path> <out_dir> <target>
  local archive="$1"
  local out_dir="$2"
  local target="$3"

  mkdir -p "$out_dir"
  if is_windows_target "$target"; then
    need_cmd unzip
    unzip -q "$archive" -d "$out_dir"
  else
    need_cmd tar
    tar -xzf "$archive" -C "$out_dir"
  fi
}

install_one() {
  # Usage: install_one <src_path> <dst_path> <force>
  local src="$1"
  local dst="$2"
  local force="$3"

  if [[ ! -f "$src" ]]; then
    echo "Archive did not contain $(basename "$src")" >&2
    exit 1
  fi
  if [[ -e "$dst" && "$force" -ne 1 ]]; then
    echo "Already exists: $dst" >&2
    echo "Re-run with --force to overwrite." >&2
    exit 1
  fi
  cp -f "$src" "$dst"
  chmod +x "$dst"
  echo "Installed: $dst"
}

version="latest"
repo="krazyjakee/model-gen"
prefix=""
bin_dir=""
force=0
verify=1
install_cli=1
install_studio=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      version="${2:-}"
      shift 2
      ;;
    --repo)
      repo="${2:-}"
      shift 2
      ;;
    --prefix)
      prefix="${2:-}"
      shift 2
      ;;
    --bin-dir)
      bin_dir="${2:-}"
      shift 2
      ;;
    --cli-only)
      install_cli=1
      install_studio=0
      shift
      ;;
    --studio-only)
      install_cli=0
      install_studio=1
      shift
      ;;
    --force)
      force=1
      shift
      ;;
    --no-verify)
      verify=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      echo >&2
      usage >&2
      exit 2
      ;;
  esac
done

need_cmd curl

if [[ -z "$bin_dir" ]]; then
  if [[ -z "$prefix" ]]; then
    prefix="$HOME/.local"
  fi
  bin_dir="$prefix/bin"
fi

tag="$version"
if [[ "$version" == "latest" ]]; then
  tag="$(resolve_latest_tag "$repo")"
else
  tag="$(normalize_tag "$version")"
fi

target="$(detect_target)"

archive_ext="tar.gz"
if is_windows_target "$target"; then
  archive_ext="zip"
fi

archive_name="mogen-${tag}-${target}.${archive_ext}"
base_url="https://github.com/${repo}/releases/download/${tag}"
archive_url="${base_url}/${archive_name}"

tmpdir="$(mktemp -d 2>/dev/null || mktemp -d -t mogen-install)"
cleanup() { rm -rf "$tmpdir"; }
trap cleanup EXIT

echo "+ downloading ${archive_name}"
archive_path="$tmpdir/$archive_name"
curl -fL "$archive_url" -o "$archive_path"

if [[ "$verify" -eq 1 ]]; then
  sums_url="${base_url}/SHA256SUMS"
  sums_path="$tmpdir/SHA256SUMS"
  if curl -fL "$sums_url" -o "$sums_path" >/dev/null 2>&1; then
    expected="$(
      awk -v f="$archive_name" '($2==f){print $1}' "$sums_path" | head -n 1
    )"
    if [[ -z "${expected:-}" ]]; then
      echo "SHA256SUMS found, but no entry for ${archive_name}" >&2
      echo "Re-run with --no-verify to bypass, or fix release assets." >&2
      exit 1
    fi
    echo "+ verifying sha256"
    if ! verify_sha256 "$archive_path" "$expected"; then
      echo "Checksum mismatch for ${archive_name}" >&2
      exit 1
    fi
  else
    echo "Warning: SHA256SUMS not found for ${tag}; skipping verification." >&2
  fi
fi

mkdir -p "$bin_dir"
exe_suffix=""
if is_windows_target "$target"; then
  exe_suffix=".exe"
fi

unpack_dir="$tmpdir/unpack"
unpack_archive "$archive_path" "$unpack_dir" "$target"

if [[ "$install_cli" -eq 1 ]]; then
  install_one "$unpack_dir/mogen${exe_suffix}" "$bin_dir/mogen${exe_suffix}" "$force"
fi
if [[ "$install_studio" -eq 1 ]]; then
  install_one "$unpack_dir/mogen-studio${exe_suffix}" "$bin_dir/mogen-studio${exe_suffix}" "$force"
fi

if [[ "$install_cli" -eq 1 ]] && ! command -v mogen >/dev/null 2>&1; then
  echo "Note: 'mogen' is not on PATH. Add this to your shell profile:" >&2
  echo "  export PATH=\"$bin_dir:\$PATH\"" >&2
fi
