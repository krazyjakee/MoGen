"""MoGen Python API: create, load, edit and compile 3D models.

A model is ``.mog`` source (MoGen's declarative 3D language) compiled to
glTF 2.0 ``.glb`` by the same native pipeline as the ``mogen`` CLI.
``kind "name" (key=value) { children }`` in ``.mog`` is
``mg.kind("name", key=value)`` here, with ``with`` blocks for children.

Create::

    import mogen as mg

    doc = mg.Document()
    oak = doc.material("table_oak_wood", color=[0.55, 0.38, 0.2], roughness=0.7)
    corners = {"fl": (-0.35, -0.2), "fr": (0.35, -0.2), "bl": (-0.35, 0.2), "br": (0.35, 0.2)}
    with doc.scene():
        with mg.box("top", size=[0.8, 0.04, 0.5], mat=oak):
            for name, (x, z) in corners.items():
                mg.connector(f"c_{name}", at=[x, -0.02, z], dir=[0, -1, 0])
        for name in corners:
            mg.cylinder(f"leg_{name}", radius=0.02, height=0.7, mat=oak)
            mg.attach(parent="top", child=f"leg_{name}", socket=f"c_{name}", plug="top")
    for d in doc.check():          # fix every error, then build
        print(d)
    doc.save("side_table.mog")
    doc.build("side_table.glb")

Edit (lossless: comments, formatting and units survive)::

    doc = mg.load("side_table.mog")
    doc.find("top")["size"] = [1.0, 0.04, 0.6]
    doc.save()

Rules for models that build:

- Metres; +Y up, -Z forward. Use real sizes (person 1.7 m, chair 0.9 m).
- Build parts at the origin and join them with ``attach`` (connector to
  connector; primitives have default ``top``/``bottom`` face connectors).
  Don't hand-compute ``pos=``.
- Every part must touch another or the build fails with E1101; mark
  deliberate gaps with ``tags="floating"``.
- Declare a material for every ``mat=``; pass the material node itself.
- ``cylinder``/``cone``/``capsule`` height runs along Y; ``box`` ``size`` is
  ``[width, height, depth]``.
- People, animals, trees: ``mg.use("humanoid_full", height=1.7)`` plus
  ``mg.use("humanoid_walk")``, ``quadruped_*`` modules, ``branch``.
- Run ``doc.check()``; each Diagnostic has ``code``, ``message``, ``line``
  and ``node`` (the Node to fix).

``help(mg.<kind>)`` lists a kind's attributes; ``mg.KINDS`` maps every kind.
Guide: https://github.com/krazyjakee/MoGen/blob/master/docs/python.md
Language: https://github.com/krazyjakee/MoGen/blob/master/docs/dsl.md
"""

from ._core import (
    KINDS,
    P,
    Attrs,
    BuildError,
    BuildResult,
    Children,
    Diagnostic,
    Document,
    Expr,
    Ident,
    MogenError,
    Node,
    ParseError,
    __version__,
    decode,
    encode,
    expr,
    face,
    gradient,
    ident,
    load,
    loads,
    node,
    ref,
    units,
)
from ._kinds import *  # noqa: F401,F403  (one builder per node kind)
from ._kinds import __all__ as _kind_builders

__all__ = [
    "KINDS", "P", "Attrs", "BuildError", "BuildResult", "Children", "Diagnostic", "Document",
    "Expr", "Ident", "MogenError", "Node", "ParseError", "__version__", "decode", "encode",
    "expr", "face", "gradient", "ident", "load", "loads", "node", "ref", "units",
    *_kind_builders,
]
