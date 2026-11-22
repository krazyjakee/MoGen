#!/usr/bin/env bash
# Build the `mogen` Python package into a throwaway venv and run its tests.
# Needs `uv` (https://docs.astral.sh/uv/). Usage: ./scripts/run-python-tests.sh [pytest args]
set -euo pipefail
cd "$(dirname "$0")/.."
venv="${MOGEN_PY_VENV:-target/python-venv}"
[ -x "$venv/bin/python" ] || uv venv -q "$venv"
uv pip install -q --python "$venv/bin/python" pytest
(cd crates/mogen-py && VIRTUAL_ENV="$PWD/../../$venv" uvx maturin develop --uv -q)
"$venv/bin/python" -m pytest crates/mogen-py/tests "$@"
