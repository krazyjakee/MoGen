# mogen

Create, edit and compile 3D models from Python. `mogen` is the Python API
for [MoGen](https://github.com/krazyjakee/MoGen). You build a model as
`.mog` source, MoGen's declarative 3D language, and compile it to glTF 2.0
`.glb` for Godot, Unity, Unreal, Blender or three.js. The compiler is
native: it's the same pipeline as the `mogen` CLI, so output is
byte-identical and deterministic.

```sh
pip install mogen
```

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

for d in doc.check():              # errors and warnings, each with line, column and node
    print(d)
doc.save("side_table.mog")         # readable .mog source
doc.build("side_table.glb")        # raises mg.BuildError if anything is wrong
```

## Rules for models that build

- **Metres, +Y up, −Z forward.** Use real sizes: a person is about 1.7 m,
  a chair 0.9 m, a door 2.0 × 0.9 m.
- **Build each part at the origin and join parts with `attach`**
  (connector to connector) instead of working out `pos=` by hand. Primitives
  have default face connectors such as `top` and `bottom`.
- **Every part must touch another**, or the build fails with `E1101`. Tag
  deliberate gaps with `tags="floating"`.
- **Declare a material for every `mat=`.** Pass the material node itself:
  `mat=oak`.
- **`cylinder`/`cone`/`capsule` run their `height` along Y;
  `box` `size` is `[width, height, depth]`.**
- **For people, animals and trees, use the standard library:**
  `mg.use("humanoid_full", height=1.7)` plus `mg.use("humanoid_walk")`, and
  `branch` for trees.
- **Run `doc.check()` and fix every error before building.** Each
  `Diagnostic` has a `.node` you can edit directly.

## Edit existing files

```python
doc = mg.load("side_table.mog")
doc.find("top")["size"] = [1.0, 0.04, 0.6]
for leg in doc.find_all(kind="cylinder"):
    leg["radius"] = 0.025
doc.save()                         # only the changed lines are rewritten; comments survive
```

## The language from Python

`kind "name" (key=value) { children }` in `.mog` is
`mg.kind("name", key=value)` in Python, with `with` for children. Python
keywords take a trailing underscore (`from_=`, `mg.if_`); bare words are
`mg.Ident("y")`; units are `mg.units.cm(90)`; module parameters are
`mg.P.h * 2`. `help(mg.box)` lists the attributes of each node kind.

- Python API guide: https://github.com/krazyjakee/MoGen/blob/master/docs/python.md
- Language reference: https://github.com/krazyjakee/MoGen/blob/master/docs/dsl.md
- Standard library (humanoids, animals, foliage, doors, windows): https://github.com/krazyjakee/MoGen/blob/master/docs/modules.md
- Example models: https://github.com/krazyjakee/MoGen/tree/master/examples
