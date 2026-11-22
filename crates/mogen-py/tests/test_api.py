"""Tests for the `mogen` Python package. Run from the repo root:

    ./scripts/run-python-tests.sh
"""

import math
from pathlib import Path

import pytest

import mogen as mg

ROOT = Path(__file__).resolve().parents[3]
EXAMPLES = ROOT / "examples"


def stool() -> mg.Document:
    doc = mg.Document()
    oak = doc.material("oak", color=[0.55, 0.38, 0.2], roughness=0.7)
    with doc.scene():
        mg.cylinder("seat", radius=0.2, height=mg.units.cm(3), y=0.45, mat=oak)
        with mg.group("legs"):
            for i in range(3):
                a = math.radians(i * 120)
                mg.cylinder(f"leg{i}", radius=0.015, height=0.45, mat=oak,
                            pos=[0.14 * math.cos(a), 0.225, 0.14 * math.sin(a)])
    return doc


# --- building ---------------------------------------------------------------


def test_build_from_scratch_prints_house_style():
    assert stool().source == (
        'material "oak" (color=[0.55, 0.38, 0.2], roughness=0.7)\n'
        "\n"
        "scene {\n"
        '  cylinder "seat" (radius=0.2, height=3cm, y=0.45, mat="oak")\n'
        '  group "legs" {\n'
        '    cylinder "leg0" (radius=0.015, height=0.45, mat="oak", pos=[0.14, 0.225, 0])\n'
        '    cylinder "leg1" (radius=0.015, height=0.45, mat="oak", pos=[-0.07, 0.225, 0.12124356])\n'
        '    cylinder "leg2" (radius=0.015, height=0.45, mat="oak", pos=[-0.07, 0.225, -0.12124356])\n'
        "  }\n"
        "}\n"
    )


def test_build_writes_glb(tmp_path):
    out = tmp_path / "stool.glb"
    result = stool().build(out)
    assert out.read_bytes() == result.data
    assert result.data[:4] == b"glTF"
    assert not [d for d in result.diagnostics if d.severity == "error"]


def test_build_is_deterministic_and_matches_saved_file(tmp_path):
    doc = stool()
    first = doc.build().data
    path = doc.save(tmp_path / "stool.mog")
    assert mg.load(path).build().data == first


def test_fbx_export(tmp_path):
    result = stool().build(tmp_path / "stool.fbx")
    assert result.path.suffix == ".fbx"
    assert result.data[:4] != b"glTF"


def test_explicit_methods_and_detached_nodes():
    doc = mg.Document()
    scene = doc.scene()
    seat = scene.box("seat", size=[1, 0.1, 1])
    leg = mg.cylinder("leg", radius=0.05)  # no `with` block → detached
    assert leg.parent is None
    seat.add(leg)
    assert leg.parent is seat and leg.document is doc
    assert doc.find("leg") is leg


def test_declarations_go_above_the_scene():
    doc = mg.Document()
    doc.scene().box(size=1)
    doc.material("red", color=[1, 0, 0])
    assert [n.kind for n in doc.nodes] == ["material", "scene"]


def test_modules_and_params():
    doc = mg.Document()
    with doc.module("leg", h=mg.units.inch(18)):
        mg.box(size=[0.05, mg.P.h, 0.05], y=mg.P.h / 2)
    with doc.scene():
        mg.use("leg", h=0.5)
    assert 'module "leg" (h=18in) {\n  box (size=[0.05, $h, 0.05], y=$h / 2)\n}' in doc.source
    assert 'use "leg" (h=0.5)' in doc.source
    doc.build()


def test_expr_arithmetic():
    assert str(mg.P.w * 2 + 0.1) == "($w * 2) + 0.1"
    assert str(1 - mg.P.w) == "1 - $w"
    assert str(-mg.P.w) == "0 - $w"
    assert str(mg.P.w * -0.5) == "$w * -0.5"


def test_keyword_kinds_and_attrs():
    n = mg.node("box", from_=[0, 0, 0], to=[1, 1, 1])
    assert list(n.attrs) == ["from", "to"]
    assert mg.if_ is not None and mg.for_ is not None and mg.import_ is not None


def test_values_encode():
    n = mg.box(axis=mg.Ident("y"), flag=True, label='say "hi"', n=3,
               pts=[(0, 0, 0), (1, 2, 3)], g=mg.gradient("linear", from_=[1, 0, 0], to=[0, 0, 1]))
    assert n.attrs.source("axis") == "y"
    assert n.attrs.source("flag") == "true"
    assert n.attrs.source("label") == '"say \\"hi\\""'
    assert n.attrs.source("pts") == "[[0, 0, 0], [1, 2, 3]]"
    assert n.attrs.source("g") == "linear(from=[1, 0, 0], to=[0, 0, 1])"
    assert mg.face("wood", uv_scale=[2, 1]).text == 'face("wood", uv_scale=[2, 1])'


def test_values_decode():
    doc = mg.loads('scene { box (size=[1, 2, 3], h=18in, axis=y, on=true, mat="oak", '
                   'tags=["a", "b"], y=$h * 2, pts=[[0, 0, 0], [1, 1, 1]]) }')
    b = doc.find(kind="box")
    assert b["size"] == [1, 2, 3]
    assert b["h"] == pytest.approx(0.4572)
    assert b["axis"] == "y" and isinstance(b["axis"], mg.Ident)
    assert b["on"] is True
    assert b["mat"] == "oak" and not isinstance(b["mat"], mg.Ident)
    assert b["tags"] == ["a", "b"]
    assert b["y"] == mg.Expr("$h * 2")
    assert b["pts"] == [[0, 0, 0], [1, 1, 1]]
    assert b["size"] == [1.0, 2.0, 3.0] and repr(doc.find(kind="box")["h"]) == "0.4572"


@pytest.mark.parametrize("bad", ["1) box (", "1, b=2", "1 // c", "[1, 2"])
def test_injection_is_rejected(bad):
    with pytest.raises(ValueError):
        mg.box(size=mg.Expr(bad))


def test_weight_units_only_on_weight():
    mg.box(weight=mg.units.kg(5))
    with pytest.raises(ValueError):
        mg.box(size=mg.units.kg(5))


def test_unencodable_values():
    with pytest.raises(TypeError):
        mg.box(size=object())
    with pytest.raises(ValueError):
        mg.box(size=float("nan"))
    with pytest.raises(ValueError):
        mg.Node("not a kind")


# --- editing ----------------------------------------------------------------

EDIT_SRC = """\
// A small test file.
material "oak" (color=[0.5, 0.3, 0.1]) // warm

scene {
  // the seat
  box "seat" (size=[1, 0.1, 1], mat="oak")
  cylinder "leg" (radius = 18in, mat="walnut")
}
"""


def test_load_save_is_lossless(tmp_path):
    for src in sorted(EXAMPLES.rglob("*.mog")):
        try:
            doc = mg.load(src)
        except mg.ParseError:
            continue  # deliberately-invalid fixtures
        out = tmp_path / src.name
        doc.save(out, stamp=False)
        assert out.read_bytes() == src.read_bytes(), src


def test_edits_only_touch_what_changed():
    doc = mg.loads(EDIT_SRC)
    doc.find("seat")["size"] = [2, 0.1, 2]
    assert doc.source == EDIT_SRC.replace("size=[1, 0.1, 1]", "size=[2, 0.1, 2]")


def test_rename_delete_and_add():
    doc = mg.loads(EDIT_SRC)
    leg = doc.find("leg")
    del leg["mat"]
    leg.name = "leg_fl"
    doc.find("seat").add("connector", "c_fl", at=[-0.4, -0.05, -0.4], dir=[0, -1, 0])
    assert doc.source == (
        "// A small test file.\n"
        'material "oak" (color=[0.5, 0.3, 0.1]) // warm\n'
        "\n"
        "scene {\n"
        "  // the seat\n"
        '  box "seat" (size=[1, 0.1, 1], mat="oak") {\n'
        '    connector "c_fl" (at=[-0.4, -0.05, -0.4], dir=[0, -1, 0])\n'
        "  }\n"
        '  cylinder "leg_fl" (radius = 18in)\n'
        "}\n"
    )


def test_move_node_between_parents():
    doc = mg.loads("scene {\n  group \"a\" {\n    box \"x\"\n  }\n  group \"b\" {\n    sphere\n  }\n}\n")
    doc.find("b").children.append(doc.find("x"))
    assert doc.source == ("scene {\n  group \"a\" {\n  }\n  group \"b\" {\n    sphere\n"
                          "    box \"x\"\n  }\n}\n")
    with pytest.raises(ValueError):
        doc.find("b").children.append(doc.find("b"))
    kids = doc.find("b").children
    kids[0] = kids[0]
    kids[0] = mg.cone("c")
    assert [n.kind for n in kids] == ["cone", "box"]


def test_copy_is_detached_and_deep():
    doc = mg.loads(EDIT_SRC)
    seat2 = doc.find("seat").copy()
    seat2.name = "seat2"
    seat2["y"] = 1
    doc.scene().children.append(seat2)
    assert doc.find("seat").get("y") is None
    assert 'box "seat2" (size=[1, 0.1, 1], mat="oak", y=1)' in doc.source


def test_save_stamps_version(tmp_path):
    doc = mg.loads(EDIT_SRC)
    doc.save(tmp_path / "x.mog")
    text = (tmp_path / "x.mog").read_text()
    assert text.startswith(f'// A small test file.\nmeta (mogen_version="{mg.__version__}")\n\nmaterial')
    assert doc.meta["mogen_version"] == mg.__version__


def test_mogb_round_trip(tmp_path):
    doc = stool()
    doc.save(tmp_path / "stool.mogb")
    assert (tmp_path / "stool.mogb").read_bytes()[:4] != b"mate"
    assert mg.load(tmp_path / "stool.mogb").build().data == doc.build().data


def test_parse_error():
    with pytest.raises(mg.ParseError):
        mg.loads("scene { box (size=) }")


# --- diagnostics ------------------------------------------------------------


def test_build_errors_point_at_nodes():
    doc = mg.loads(EDIT_SRC)
    with pytest.raises(mg.BuildError) as exc:
        doc.build()
    errors = [d for d in exc.value.diagnostics if d.severity == "error"]
    assert errors
    located = [d for d in errors if d.node is not None]
    assert located, errors
    d = located[0]
    assert d.node is doc.find("leg")
    assert d.line == 7 and d.column is not None
    assert "7:" in str(d)


def test_check_returns_diagnostics_without_raising():
    diags = mg.loads(EDIT_SRC).check()
    assert any(d.severity == "error" for d in diags)
    assert all(d.severity != "error" for d in stool().check())


def test_imports_resolve_against_the_file(tmp_path):
    (tmp_path / "parts.mog").write_text('module "peg" () {\n  cylinder (radius=0.02, height=0.1)\n}\n')
    doc = mg.Document()
    doc.add("import", "parts.mog")
    with doc.scene():
        mg.use("peg")
    doc.path = tmp_path / "main.mog"
    doc.build()


def test_example_edit_then_build():
    doc = mg.load(EXAMPLES / "furniture" / "chair.mog")
    for leg in doc.find_all(kind="cylinder"):
        leg["radius"] = 0.025
    assert doc.build().data[:4] == b"glTF"
