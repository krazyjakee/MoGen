"""Every Python example in the docs must run: they're what people and LLMs
copy. Covers docs/python.md, the PyPI README and the `help(mogen)` text."""

import re
import shutil
import textwrap
from pathlib import Path

import pytest

import mogen as mg

ROOT = Path(__file__).resolve().parents[3]
MARKDOWN = [ROOT / "docs" / "python.md", ROOT / "crates" / "mogen-py" / "README.md"]


def markdown_blocks(path: Path) -> list[str]:
    return re.findall(r"```python\n(.*?)```", path.read_text(), re.S)


def docstring_blocks(doc: str) -> list[str]:
    """reST literal blocks (``Title::`` followed by an indented block)."""
    return [textwrap.dedent(b) for b in re.findall(r"::\n\n((?:    .*\n|\n)+)", doc)]


def run_blocks(blocks: list[str], label: str, tmp_path: Path, monkeypatch) -> None:
    assert blocks, f"no examples found in {label}"
    shutil.copy(ROOT / "examples" / "furniture" / "chair.mog", tmp_path)
    monkeypatch.chdir(tmp_path)
    ns: dict = {}
    for i, src in enumerate(blocks):
        exec(compile(src, f"{label} block {i + 1}", "exec"), ns)


@pytest.mark.parametrize("path", MARKDOWN, ids=lambda p: p.name)
def test_markdown_examples_run(path, tmp_path, monkeypatch):
    run_blocks(markdown_blocks(path), path.name, tmp_path, monkeypatch)


def test_package_docstring_examples_run(tmp_path, monkeypatch):
    run_blocks(docstring_blocks(mg.__doc__), "mogen.__doc__", tmp_path, monkeypatch)


def test_quick_start_has_no_diagnostics(tmp_path, monkeypatch):
    run_blocks(markdown_blocks(MARKDOWN[0])[:1], "quick start", tmp_path, monkeypatch)
    assert mg.load(tmp_path / "side_table.mog").check() == []
