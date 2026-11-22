"""MoGen Python API: build, load, edit and compile ``.mog`` files.

The compiler is native, the same pipeline ``mogen build`` runs, so a script
and the CLI produce byte-identical GLBs for the same source. Every document
is a real ``.mog`` file: loading and saving keeps comments, formatting, unit
literals (``18in``) and ``$param`` expressions intact, and an edit only
rewrites what it touched.

Build from scratch::

    import mogen as mg

    doc = mg.Document()
    oak = doc.material("oak", color=[0.55, 0.38, 0.2], roughness=0.7)
    with doc.scene():
        mg.cylinder("seat", radius=0.2, height=mg.units.cm(3), y=0.45, mat=oak)
        with mg.group("legs"):
            for i in range(3):
                mg.cylinder(f"leg{i}", radius=0.015, height=0.45, ry=i * 120)
    doc.save("stool.mog")
    doc.build("stool.glb")

Edit an existing file::

    doc = mg.load("chair.mog")
    doc.find("seat")["size"] = [0.5, 0.04, 0.5]
    for leg in doc.find_all(kind="cylinder"):
        leg["radius"] = 0.025
    doc.save()

See ``docs/python.md`` for the full reference.
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
