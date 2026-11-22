# Python API

`pip install mogen` gives a Python module for building, loading, editing and
compiling `.mog` files. It's the PyPI-wheel counterpart of Blender's `bpy`.
The compiler inside it is the native pipeline `mogen build` runs, so a script
and the CLI produce byte-identical GLBs. Source lives in `crates/mogen-py`.

- [Building](#building)
- [Editing existing files](#editing-existing-files)
- [Values](#values)
- [Compiling and diagnostics](#compiling-and-diagnostics)
- [Reference](#reference)
- [Developing](#developing)

## Building

```python
import math
import mogen as mg

doc = mg.Document()
doc.set_meta(name="stool", tags=["furniture"])
oak = doc.material("oak", color=[0.55, 0.38, 0.2], roughness=0.7)

with doc.scene():
    mg.cylinder("seat", radius=0.2, height=mg.units.cm(3), y=0.45, mat=oak)
    with mg.group("legs"):
        for i in range(3):
            a = math.radians(i * 120)
            mg.cylinder(f"leg{i}", radius=0.015, height=0.45, mat=oak,
                        pos=[0.14 * math.cos(a), 0.225, 0.14 * math.sin(a)])

doc.save("stool.mog")
doc.build("stool.glb")
```

- Every node kind has a builder: `mg.box`, `mg.group`, `mg.terrain`, … They
  are generated from the compiler's schema. Python keywords get a trailing
  underscore (`mg.if_`, `mg.for_`, `mg.import_`). Any kind also works through
  `mg.node("kind", "name", **attrs)`.
- Inside `with node:` (or `with doc:`), a builder appends to that node.
  Outside any `with` block it returns a detached `Node`.
- Without builders: `scene.box("lid", size=1)`, `node.add("box", "lid", size=1)`,
  or `node.children.append(other_node)`.
- The first positional argument is the node name: `mg.use("door_simple", width=0.9)`
  writes `use "door_simple" (width=0.9)`.
- Keyword arguments become attributes in the order given. For keys that are
  Python keywords, add an underscore: `from_=[0, 0, 0]` writes `from=[0, 0, 0]`.
- `doc.add(...)` puts declarations (`material`, `module`, `import`, …) above
  the `scene` and everything else at the end. `doc.material(name, **attrs)`
  creates the material, or updates it if it already exists.

Modules take parameters, which you reference with `mg.P`:

```python
with doc.module("leg", h=mg.units.inch(18)):
    mg.box(size=[0.05, mg.P.h, 0.05], y=mg.P.h / 2)   # size=[0.05, $h, 0.05], y=$h / 2
with doc.scene():
    mg.use("leg", h=0.5)
```

## Editing existing files

```python
doc = mg.load("chair.mog")                 # .mog or .mogb
seat = doc.find("seat")                    # by name; also kind=, where=lambda n: …
seat["size"] = [0.5, 0.04, 0.5]
for leg in doc.find_all(kind="cylinder"):
    leg["radius"] = 0.025
del seat["radius"]
doc.find("backrest").remove()
doc.save()                                 # back to chair.mog
```

Saving is lossless. Comments, blank lines, alignment, unit literals and
expressions survive untouched, and only the nodes and attributes you changed
are rewritten. New nodes copy the indentation of their siblings.
`doc.save(stamp=False)` skips the `meta.mogen_version` stamp that every MoGen
tool writes on save.

Tree API: `node.kind`, `node.name` (both settable), `node.attrs` (a mutable
mapping; `node[key]` is shorthand), `node.attrs.source(key)` (the value
exactly as written), `node.children`, `node.parent`, `node.document`,
`node.walk()`, `node.copy()`, `node.remove()`, `node.source`. On a document:
`doc.nodes`, `doc.scene()`, `doc.meta`, `doc.materials`, `doc.source`.
Appending a node that already has a parent moves it.

## Values

| Python | `.mog` | read back as |
|---|---|---|
| `1`, `0.25` | `1`, `0.25` | `float` |
| `"oak"` | `"oak"` | `str` |
| `mg.Ident("y")` | `y` | `Ident` (a `str`) |
| `True` / `False` | `true` / `false` | `bool` |
| `[1, 2, 3]`, tuples, numpy arrays | `[1, 2, 3]` | `list` |
| a `Node` | its quoted name (`mat=oak`) | `str` |
| `mg.units.cm(90)`, `.inch`, `.ft`, `.kg`, `.lb`, `.density(700)` | `90cm`, … | `float` (metres / kg) |
| `mg.P.h * 2`, `mg.ref("h")`, `mg.expr("$h / 2")` | `$h * 2`, … | `Expr` |
| `mg.gradient("linear", from_=[1,0,0], to=[0,0,1])` | `linear(from=…, to=…)` | `Expr` |
| `mg.face("wood", uv_scale=[2, 1])` | `face("wood", uv_scale=[2, 1])` | `Expr` (in a list) |

Numbers are written at the compiler's `f32` precision, so
`0.14 * cos(120°)` is written as `-0.07`. Every value is checked against the
grammar when you assign it; a bad value raises `ValueError` and the document
is left unchanged.

## Compiling and diagnostics

```python
result = doc.build("out.glb")       # or .fbx / format="fbx"; omit the path for bytes only
result.data, result.warnings
try:
    doc.build()
except mg.BuildError as e:
    for d in e.diagnostics:
        print(d)                    # stool.mog:7:3: error[E0104]: unknown material "walnut"
        d.node                      # the Node it points into, when it has a span
diags = doc.check()                 # every stage except export; never raises
```

`import` paths and textures resolve against `doc.base_dir`. That defaults to
the file's directory, or to the working directory for a document that was
never saved.

## Reference

`help(mg.box)` lists the attributes specific to each kind, and `mg.KINDS`
maps every kind to them. The full language is in [`dsl.md`](./dsl.md) and the
stdlib modules in [`modules.md`](./modules.md).

## Developing

```sh
./scripts/run-python-tests.sh                         # build into target/python-venv + pytest (needs uv)
MOGEN_GOLDENS_UPDATE=1 cargo test -p mogen-py         # regenerate python/mogen/_kinds.py after schema changes
```

The editing model is `mogen_dsl::syntax`, a lossless parse/print that Rust
tools can use too. PyO3 is behind the `python` feature, so
`cargo test --workspace` doesn't need Python. `python-wheels.yml` builds
and tests abi3 wheels for every platform on PRs that touch the package;
tagged releases reuse it and publish to PyPI through trusted publishing (the
`pypi` job and environment in `release.yml`).
