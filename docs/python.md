# Python API

`pip install mogen` gives a Python module for creating, loading, editing and
compiling MoGen models (`.mog` files) into glTF 2.0 `.glb`, the format Godot,
Unity, Unreal, Blender and three.js import. It's the PyPI-wheel counterpart
of Blender's `bpy`. The compiler inside it is the same native pipeline
`mogen build` runs, so a script and the CLI produce byte-identical files.

Every document is plain `.mog` source, MoGen's declarative 3D language
([`dsl.md`](./dsl.md)). The Python API maps it one-to-one, so anything the
language reference shows can be written from Python (see
[Mapping `.mog` to Python](#mapping-mog-to-python)).

**LLM agents:** read [Creating models](#creating-models) first. It covers
the check-and-fix loop and the modelling rules that decide whether a model
builds on the first try.

- [Quick start](#quick-start)
- [Creating models](#creating-models)
- [Mapping `.mog` to Python](#mapping-mog-to-python)
- [Building API](#building-api)
- [Editing existing files](#editing-existing-files)
- [Values](#values)
- [Compiling and diagnostics](#compiling-and-diagnostics)
- [Reference](#reference)
- [Developing](#developing)

## Quick start

```python
import mogen as mg

doc = mg.Document()
doc.set_meta(name="side_table", tags=["furniture", "table"])
oak = doc.material("table_oak_wood", color=[0.55, 0.38, 0.2], roughness=0.7)

corners = {"fl": (-0.35, -0.2), "fr": (0.35, -0.2), "bl": (-0.35, 0.2), "br": (0.35, 0.2)}
with doc.scene():
    with mg.box("top", size=[0.8, 0.04, 0.5], mat=oak):        # 80 x 4 x 50 cm tabletop
        for name, (x, z) in corners.items():                    # one socket under each corner
            mg.connector(f"c_{name}", at=[x, -0.02, z], dir=[0, -1, 0])
    for name in corners:
        mg.cylinder(f"leg_{name}", radius=0.02, height=0.7, mat=oak)
        mg.attach(parent="top", child=f"leg_{name}", socket=f"c_{name}", plug="top")

for d in doc.check():          # fix every error before building
    print(d)
doc.save("side_table.mog")
doc.build("side_table.glb")
```

`doc.source` (or the saved file) is ordinary `.mog`:

```
meta (name="side_table", tags=["furniture", "table"])

material "table_oak_wood" (color=[0.55, 0.38, 0.2], roughness=0.7)

scene {
  box "top" (size=[0.8, 0.04, 0.5], mat="table_oak_wood") {
    connector "c_fl" (at=[-0.35, -0.02, -0.2], dir=[0, -1, 0])
    ...
  }
  cylinder "leg_fl" (radius=0.02, height=0.7, mat="table_oak_wood")
  attach (parent="top", child="leg_fl", socket="c_fl", plug="top")
  ...
}
```

## Creating models

### The loop

1. Build the document.
2. Call `doc.check()`. It runs every compiler stage except export and
   returns a list of `Diagnostic`s without raising.
3. Fix every `error`. Each diagnostic has a `code`, a `message`, a
   `line`/`column` in `doc.source`, and `node`: the `Node` it points into,
   which you can edit directly. Read the warnings too.
4. Repeat until there are no errors, then `doc.build("out.glb")`.

```python
for d in doc.check():
    print(d.severity, d.code, d.message)
    if d.node is not None:
        print("  in", d.node, "at line", d.line)
```

### Modelling rules

1. **Units are metres; +Y is up, −Z is forward, +X is right.** Use real
   sizes: a person is about 1.7 m tall, a chair 0.9 m with a 0.45 m seat, a
   table 0.75 m, a door 2.0 × 0.9 m, a car 4 m long, a tree 3–8 m.
2. **Build each part at the origin and join parts with `attach`.** Don't
   work out positions by hand: it's the main cause of floating heads and
   disconnected legs. Give the parent a `connector` (a socket) and attach the
   child's connector (a plug) to it, as the quick start does. Primitives
   also come with default face connectors (`top`, `bottom`, …), so a
   cylinder's `top` is the centre of its +Y end.
   Use `pos=`/`rot=` only for parts that are deliberately apart.
3. **Concentric parts don't need `attach`.** A pane inside a frame, liquid in
   a glass or a core in a shell all share the default origin. `attach` joins
   surfaces end to end, so it would stack them instead.
4. **Every part must touch another (within 2 mm)**, or the build fails with
   `E1101` (disconnected clusters). If a gap is intentional (a hovering orb,
   a drone rotor), tag the part or its group with `tags="floating"`.
5. **Declare a material for every `mat=`.** Pass the material `Node` itself
   (`mat=oak`) rather than retyping its name. Name materials
   `<object>_<region>_<surface>` (`table_oak_wood`, `tiger_back_fur`); the
   texture generator uses those names as hints.
6. **Know each primitive's orientation.** `cylinder`, `cone`, `capsule`,
   `tube` and `pyramid` run their `height` along Y. `box`/`rounded_box`
   `size` is `[width, height, depth]`. `plane`/`disc` lie flat facing +Y
   and `quad` stands facing +Z; all three are single-sided, so set
   `double_sided=1` on the material if the back can be seen. The full list is
   under Primitives in [`dsl.md`](./dsl.md#primitives).
7. **Use the standard library for organic subjects.** For people,
   `mg.use("humanoid_full", height=1.7, ...)` gives a rigged figure with
   hands, feet and a face. Pair it with one animation module
   (`humanoid_idle`, `humanoid_walk`, `humanoid_run`, `humanoid_jump`), and
   attach clothing and props to its `slot_*` connectors. Use `branch` for
   trees, `blob` for single organic masses (skulls, fruit), and the
   `quadruped_*` modules for animals. See [`modules.md`](./modules.md).
8. **Animate with templates first:** `spin`, `open_close`, `wave`, `flap`
   and `idle`. Only build `joint` + `clip` + `track` by hand when you need
   specific keyframes.
9. **Keep part names unique.** `attach` finds parts by name, and a duplicate
   part name isn't reported, so the join can land on the wrong part.
10. **Let Python do the arithmetic, and keep the structure in the model.**
    Loops, math and data-driven layouts are what Python adds; the replicators
    `array`, `mirror`, `grid` and `stack` keep repeated parts compact in the
    output.

An intentionally floating part:

```python
lamp = mg.Document()
brass = lamp.material("lamp_brass", color=[0.8, 0.6, 0.2], metallic=1, roughness=0.3)
with lamp.scene():
    mg.box("base", size=[0.3, 0.05, 0.3], mat=brass)
    mg.sphere("orb", radius=0.1, y=0.5, mat=brass, tags="floating")   # without the tag: E1101
assert not [d for d in lamp.check() if d.severity == "error"]
```

A walking character:

```python
person = mg.Document()
with person.scene():
    mg.use("humanoid_full", height=1.7, shirt=[0.6, 0.1, 0.1], pants=[0.2, 0.2, 0.3])
    mg.use("humanoid_walk")
person.build("person.glb")
```

## Mapping `.mog` to Python

Every `.mog` statement has the form `kind "name" (key=value, ...) { children }`,
and the API follows it directly:

| `.mog` | Python |
|---|---|
| `box "lid" (size=[1, 0.1, 1])` | `mg.box("lid", size=[1, 0.1, 1])` |
| `group "legs" { ... }` | `with mg.group("legs"): ...` |
| `scene { ... }` | `with doc.scene(): ...` |
| `material "oak" (color=[...])` | `doc.material("oak", color=[...])` |
| `meta (name="chair")` | `doc.set_meta(name="chair")` |
| `module "leg" (h=1) { box (size=[0.1, $h, 0.1]) }` | `with doc.module("leg", h=1): mg.box(size=[0.1, mg.P.h, 0.1])` |
| `use "leg" (h=2)` | `mg.use("leg", h=2)` |
| `import "parts.mog"` | `doc.add("import", "parts.mog")` |
| `from=[0, 0, 0]` (a Python keyword) | `from_=[0, 0, 0]` |
| `axis=y` (bare word) | `axis=mg.Ident("y")` |
| `18in`, `90cm`, `5kg` | `mg.units.inch(18)`, `mg.units.cm(90)`, `mg.units.kg(5)` |
| `$h * 2` | `mg.P.h * 2` |
| `if`, `for`, `else` nodes | `mg.if_`, `mg.for_`, `mg.else_` |

Most string attributes accept either a quoted string or a bare word. Use
`mg.Ident` where the reference shows a bare word for an enum-like value.

## Building API

- Every node kind has a builder: `mg.box`, `mg.group`, `mg.terrain`, … They
  are generated from the compiler's schema. Any kind also works through
  `mg.node("kind", "name", **attrs)`.
- Inside `with node:` (or `with doc:`), a builder appends to that node.
  Outside any `with` block it returns a detached `Node`.
- Without builders: `scene.box("lid", size=1)`, `node.add("box", "lid", size=1)`,
  or `node.children.append(other_node)`.
- The first positional argument is the node name: `mg.use("door_simple", width=0.9)`
  writes `use "door_simple" (width=0.9)`.
- Keyword arguments become attributes in the order given.
- `doc.add(...)` puts declarations (`material`, `module`, `import`, …) above
  the `scene` and everything else at the end. `doc.material(name, **attrs)`
  creates the material, or updates it if it already exists.

Modules are reusable parts with parameters, which you reference with `mg.P`:

```python
kit = mg.Document()
with kit.module("leg", h=mg.units.inch(18)):
    mg.box(size=[0.05, mg.P.h, 0.05], y=mg.P.h / 2)   # size=[0.05, $h, 0.05], y=$h / 2
with kit.scene():
    mg.use("leg", h=0.5)
kit.build()
```

## Editing existing files

```python
doc = mg.load("chair.mog")                 # .mog or .mogb
seat = doc.find("seat")                    # by name; also kind=, where=lambda n: …
seat["size"] = [0.5, 0.04, 0.5]
for leg in doc.find_all(kind="cylinder"):
    leg["radius"] = 0.025
doc.save()                                 # back to chair.mog
```

Saving is lossless. Comments, blank lines, alignment, unit literals and
expressions survive untouched, and only the nodes and attributes you changed
are rewritten. New nodes copy the indentation of their siblings.
`doc.save(stamp=False)` skips the `meta.mogen_version` stamp that every MoGen
tool writes on save.

Tree API: `node.kind`, `node.name` (both settable), `node.attrs` (a mutable
mapping; `node[key]` is shorthand, `del node[key]` removes),
`node.attrs.source(key)` (the value exactly as written), `node.children`,
`node.parent`, `node.document`, `node.walk()`, `node.copy()`, `node.remove()`,
`node.source`. On a document: `doc.nodes`, `doc.scene()`, `doc.meta`,
`doc.materials`, `doc.source`. Appending a node that already has a parent
moves it.

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
doc = mg.load("side_table.mog")
result = doc.build("side_table.glb")  # or .fbx / format="fbx"; omit the path for bytes only
result.data, result.warnings
try:
    doc.build()
except mg.BuildError as e:
    for d in e.diagnostics:
        print(d)                      # side_table.mog:7:3: error[E0104]: unknown material "walnut"
        d.node                        # the Node it points into, when it has a span
diags = doc.check()                   # every stage except export; never raises
```

`import` paths and textures resolve against `doc.base_dir`. That defaults to
the file's directory, or to the working directory for a document that was
never saved.

## Reference

`help(mg.box)` lists the attributes specific to each kind, and `mg.KINDS`
maps every kind to them. The full language is in [`dsl.md`](./dsl.md), the
standard-library modules in [`modules.md`](./modules.md), and worked models
in [`examples/`](https://github.com/krazyjakee/MoGen/tree/master/examples).
Each example is a `.mog` file `mg.load` can open.

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
`pypi` job and environment in `release.yml`). The Python code blocks in this
file and in the package README are executed by `tests/test_docs.py`.
