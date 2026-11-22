"""Object model, value encoding and compile entry points (re-exported by ``mogen``)."""

from __future__ import annotations

import contextvars
import keyword
import numbers
import os
from collections.abc import Iterable, Iterator, MutableMapping, MutableSequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Optional, Union, overload

from . import _native

__version__: str = _native.__version__



# --- errors -----------------------------------------------------------------


class MogenError(Exception):
    """Base class for MoGen errors."""


class ParseError(MogenError, ValueError):
    """Source text is not valid ``.mog``."""


class BuildError(MogenError):
    """Compilation failed. ``diagnostics`` holds every message, errors first."""

    def __init__(self, diagnostics: list["Diagnostic"]):
        self.diagnostics = diagnostics
        errors = [d for d in diagnostics if d.severity == "error"]
        lines = [str(d) for d in errors or diagnostics]
        super().__init__("build failed:\n  " + "\n  ".join(lines))


# --- values -----------------------------------------------------------------


class Expr:
    """A value written as raw ``.mog`` source: an expression (``$h * 2``),
    a unit literal (``18in``), a gradient or a ``face(...)`` entry.

    Supports arithmetic, so ``P.h * 2 + 0.1`` builds ``($h * 2) + 0.1``.
    Validated when assigned to an attribute.
    """

    __slots__ = ("text",)

    def __init__(self, text: str):
        if not isinstance(text, str) or not text.strip():
            raise ValueError("Expr needs non-empty source text")
        self.text = text.strip()

    def _bin(self, op: str, other: Any, flip: bool = False) -> "Expr":
        a, b = _operand(self), _operand(other)
        return Expr(f"{b} {op} {a}" if flip else f"{a} {op} {b}")

    def __add__(self, o): return self._bin("+", o)
    def __radd__(self, o): return self._bin("+", o, True)
    def __sub__(self, o): return self._bin("-", o)
    def __rsub__(self, o): return self._bin("-", o, True)
    def __mul__(self, o): return self._bin("*", o)
    def __rmul__(self, o): return self._bin("*", o, True)
    def __truediv__(self, o): return self._bin("/", o)
    def __rtruediv__(self, o): return self._bin("/", o, True)
    def __neg__(self): return Expr(f"0 - {_operand(self)}")

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Expr) and other.text == self.text

    def __hash__(self) -> int:
        return hash(("Expr", self.text))

    def __str__(self) -> str:
        return self.text

    def __repr__(self) -> str:
        return f"Expr({self.text!r})"


def _operand(v: Any) -> str:
    if isinstance(v, Expr):
        t = v.text
        simple = _native.is_ident(t.lstrip("$")) or not any(c in t for c in " +-*/<>=!")
        return t if simple else f"({t})"
    if isinstance(v, bool) or not isinstance(v, numbers.Real):
        raise TypeError(f"can't use {v!r} in an expression")
    return _native.format_number(float(v))  # `-0.5` is a valid factor


class Ident(str):
    """A bare identifier value, e.g. ``axis=Ident("y")`` writes ``axis=y``.

    Decoded attribute values come back as ``Ident`` when they were written
    bare, so they round-trip unchanged.
    """

    def __new__(cls, s: str):
        if not _native.is_ident(s):
            raise ValueError(f"{s!r} is not a valid identifier")
        return super().__new__(cls, s)

    def __repr__(self) -> str:
        return f"Ident({str(self)!r})"


def expr(text: str) -> Expr:
    """Raw ``.mog`` value source, e.g. ``expr("$w / 2")`` or ``expr("18in")``."""
    return Expr(text)


def ident(s: str) -> Ident:
    return Ident(s)


def ref(name: str) -> Expr:
    """Reference a module parameter: ``ref("h")`` → ``$h``."""
    if not _native.is_ident(name):
        raise ValueError(f"{name!r} is not a valid parameter name")
    return Expr(f"${name}")


class _Params:
    """``P.h`` is shorthand for ``ref("h")``."""

    def __getattr__(self, name: str) -> Expr:
        if name.startswith("__"):
            raise AttributeError(name)
        return ref(name)


P = _Params()


class _Units:
    """Unit literals, e.g. ``units.cm(90)`` → ``90cm``. Values are kept as
    written in the source and normalised (to metres / kilograms) by the
    compiler."""

    _LENGTH = ("mm", "cm", "m", "km", "ft", "yd")
    _MASS = ("kg", "g", "t", "lb", "oz", "st")

    def __getattr__(self, unit: str) -> Callable[[float], Expr]:
        suffix = {"inch": "in", "inches": "in"}.get(unit, unit)
        if suffix not in self._LENGTH + self._MASS + ("in",):
            raise AttributeError(f"unknown unit {unit!r}")
        return lambda v: Expr(_native.format_number(float(v)) + suffix)

    def density(self, kg_per_m3: float) -> Expr:
        """``units.density(700)`` → ``700kg/m3``."""
        return Expr(_native.format_number(float(kg_per_m3)) + "kg/m3")


units = _Units()


def _call(fn: str, args: list[str], kwargs: dict[str, Any]) -> Expr:
    parts = list(args) + [f"{_key(k)}={encode(v)}" for k, v in kwargs.items()]
    return Expr(f"{fn}({', '.join(parts)})")


def gradient(kind: str, **kwargs: Any) -> Expr:
    """Surface gradient: ``gradient("linear", from_=[1,0,0], to=[0,0,1], axis=Ident("y"))``."""
    if kind not in ("linear", "vertical", "radial", "stops"):
        raise ValueError(f"unknown gradient kind {kind!r}")
    return _call(kind, [], kwargs)


def face(material: Union[str, "Node"], **kwargs: Any) -> Expr:
    """One ``box (faces=[…])`` entry with an authored UV transform:
    ``face("wood", uv_scale=[2, 1])``."""
    return _call("face", [encode(material)], kwargs)


def _key(k: str) -> str:
    """Python kwarg → attribute key: ``from_`` → ``from``."""
    key = k[:-1] if k.endswith("_") and keyword.iskeyword(k[:-1]) else k
    if not _native.is_ident(key):
        raise ValueError(f"{k!r} is not a valid attribute name")
    return key


def encode(value: Any) -> str:
    """Python value → ``.mog`` value source.

    ``bool`` → ``true``/``false``; numbers → literals; ``str`` → quoted
    string; ``Ident`` → bare word; ``Expr`` → its text; a ``Node`` → its
    quoted name (so ``mat=oak_material`` works); any other iterable →
    ``[…]`` list (tuples, numpy arrays, glam-style vectors).
    """
    if isinstance(value, Expr):
        return value.text
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, Ident):
        return str(value)
    if isinstance(value, str):
        return _native.quote(value)
    if isinstance(value, numbers.Integral):
        return str(int(value))
    if isinstance(value, numbers.Real):
        return _native.format_number(float(value))
    if isinstance(value, Node):
        if value.name is None:
            raise ValueError(f"can't reference unnamed {value.kind} node by name")
        return _native.quote(value.name)
    if isinstance(value, Iterable):
        return "[" + ", ".join(encode(v) for v in value) + "]"
    raise TypeError(f"can't write {type(value).__name__} value {value!r} to .mog")


def decode(key: str, raw: str) -> Any:
    """``.mog`` value source → Python value (inverse of :func:`encode`)."""
    tag, payload = _native.decode_value(key, raw)
    if tag == "ident":
        if payload in ("true", "false"):
            return payload == "true"
        return Ident(payload)
    if tag == "raw":
        return Expr(payload)
    return payload


# --- diagnostics ------------------------------------------------------------


@dataclass
class Diagnostic:
    severity: str
    code: str
    message: str
    file: Optional[str] = None
    line: Optional[int] = None
    column: Optional[int] = None
    span: Optional[tuple[int, int]] = None
    node: Optional["Node"] = field(default=None, repr=False, compare=False)
    """The innermost node the diagnostic points into, when it's in this document."""

    def __str__(self) -> str:
        where = self.file or "<document>"
        if self.line is not None:
            where += f":{self.line}:{self.column}"
        return f"{where}: {self.severity}[{self.code}]: {self.message}"


@dataclass
class BuildResult:
    data: bytes
    diagnostics: list[Diagnostic]
    path: Optional[Path] = None

    def __bytes__(self) -> bytes:
        return self.data

    @property
    def warnings(self) -> list[Diagnostic]:
        return [d for d in self.diagnostics if d.severity == "warning"]


# --- tree -------------------------------------------------------------------

# Where module-level builders (`mg.box(...)`) attach new nodes: the innermost
# `with node:` / `with doc:` block.
_target: contextvars.ContextVar[Union["Node", "Document", None]] = contextvars.ContextVar(
    "mogen_target", default=None
)

# Top-level kinds that read as declarations; `Document.add` keeps them above
# the scene so files stay in the conventional order.
_DECLARATIONS = {"meta", "material", "physics", "shader", "module", "import", "joint", "clip",
                 "skeleton", "lod_scale"}


class _Attr:
    __slots__ = ("key", "value", "leading", "trailing", "orig")

    def __init__(self, key: str, value: str, leading=None, trailing=None, orig=None):
        self.key, self.value = key, value
        self.leading, self.trailing, self.orig = leading, trailing, orig

    def native(self) -> dict:
        return {"key": self.key, "value": self.value, "leading": self.leading,
                "trailing": self.trailing, "orig": self.orig}


class Attrs(MutableMapping):
    """A node's attributes as a mapping, in source order.

    Values decode to Python types (``float``, ``list``, ``str``, ``bool``,
    :class:`Ident`, :class:`Expr`); assigned values are encoded with
    :func:`encode` and checked against the grammar.
    """

    __slots__ = ("_node",)

    def __init__(self, node: "Node"):
        self._node = node

    def _find(self, key: str) -> Optional[_Attr]:
        return next((a for a in self._node._attrs if a.key == key), None)

    def __getitem__(self, key: str) -> Any:
        a = self._find(key)
        if a is None:
            raise KeyError(key)
        return decode(key, a.value)

    def __setitem__(self, key: str, value: Any) -> None:
        key = _key(key)
        raw = _native.check_value(key, encode(value))
        a = self._find(key)
        if a is None:
            self._node._attrs.append(_Attr(key, raw))
        else:
            a.value = raw

    def __delitem__(self, key: str) -> None:
        a = self._find(key)
        if a is None:
            raise KeyError(key)
        self._node._attrs.remove(a)

    def __iter__(self) -> Iterator[str]:
        return iter([a.key for a in self._node._attrs])

    def __len__(self) -> int:
        return len(self._node._attrs)

    def source(self, key: str) -> str:
        """The value exactly as written, e.g. ``"18in"`` or ``"$h * 2"``."""
        a = self._find(key)
        if a is None:
            raise KeyError(key)
        return a.value

    def __repr__(self) -> str:
        return "{" + ", ".join(f"{a.key}={a.value}" for a in self._node._attrs) + "}"


class Children(MutableSequence):
    """A node's (or document's) child list. Inserting a node that already
    has a parent moves it."""

    __slots__ = ("_owner",)

    def __init__(self, owner: Union["Node", "Document"]):
        self._owner = owner

    def _list(self, create: bool = False) -> list["Node"]:
        o = self._owner
        if isinstance(o, Document):
            return o._nodes
        if o._children is None:
            if not create:
                return []
            o._children = []
        return o._children

    def __len__(self) -> int:
        return len(self._list())

    @overload
    def __getitem__(self, i: int) -> "Node": ...
    @overload
    def __getitem__(self, i: slice) -> list["Node"]: ...
    def __getitem__(self, i):
        return self._list()[i]

    def _adopt(self, n: "Node") -> "Node":
        if not isinstance(n, Node):
            raise TypeError(f"expected a Node, got {type(n).__name__}")
        o = self._owner
        if isinstance(o, Node) and (o is n or n in o._ancestors_and_self()):
            raise ValueError("can't insert a node into its own subtree")
        if n._parent is not None:
            n.remove()
            if n._leading is not None and "//" not in n._leading:
                n._leading = None  # re-indent for the new position
        n._parent = o
        return n

    def __setitem__(self, i, n) -> None:
        if isinstance(i, slice):
            raise TypeError("slice assignment isn't supported; use insert/remove")
        lst = self._list()
        old = lst[i]
        if n is old:
            return
        self._adopt(n)
        lst[lst.index(old)] = n
        old._parent = None

    def __delitem__(self, i) -> None:
        lst = self._list()
        for n in (lst[i] if isinstance(i, slice) else [lst[i]]):
            n._parent = None
        del lst[i]

    def insert(self, i: int, n: "Node") -> None:
        self._adopt(n)
        self._list(create=True).insert(i, n)

    def __repr__(self) -> str:
        return repr(list(self))


def _walk(nodes: Iterable["Node"]) -> Iterator["Node"]:
    for n in nodes:
        yield n
        if n._children:
            yield from _walk(n._children)


def _match(n: "Node", name: Optional[str], kind: Optional[str], pred) -> bool:
    return ((name is None or n.name == name) and (kind is None or n.kind == kind)
            and (pred is None or pred(n)))


class _Container:
    """Shared tree queries and builder-context behaviour."""

    def walk(self) -> Iterator["Node"]:
        """Every node below this one, depth-first in source order."""
        return _walk(self.children)

    def find(self, name: Optional[str] = None, kind: Optional[str] = None,
             where: Optional[Callable[["Node"], bool]] = None) -> Optional["Node"]:
        """First descendant matching all given filters, or ``None``."""
        return next((n for n in self.walk() if _match(n, name, kind, where)), None)

    def find_all(self, name: Optional[str] = None, kind: Optional[str] = None,
                 where: Optional[Callable[["Node"], bool]] = None) -> list["Node"]:
        return [n for n in self.walk() if _match(n, name, kind, where)]

    def __enter__(self):
        self.__dict__.setdefault("_tokens", []).append(_target.set(self))
        return self

    def __exit__(self, *exc) -> None:
        _target.reset(self.__dict__["_tokens"].pop())


class Node(_Container):
    """One ``kind "name" (attrs) { children }`` statement.

    ``node["size"]`` reads/writes attributes (see :class:`Attrs`);
    ``node.children`` is the child list; ``node.box("lid", size=1)`` (any
    known kind) appends a child. ``with node:`` makes it the parent for
    module-level builders such as ``mogen.box(...)``.
    """

    def __init__(self, kind: str, name: Optional[str] = None, /, **attrs: Any):
        if not _native.is_ident(kind):
            raise ValueError(f"{kind!r} is not a valid node kind")
        self._kind = kind
        self._name = name
        self._attrs: list[_Attr] = []
        self._children: Optional[list[Node]] = None
        self._leading: Optional[str] = None
        self._orig = None
        self._parent: Union[Node, Document, None] = None
        for k, v in attrs.items():
            self.attrs[k] = v

    # -- native conversion
    @classmethod
    def _from_native(cls, d: dict, parent) -> "Node":
        n = cls.__new__(cls)
        n._kind, n._name = d["kind"], d["name"]
        n._attrs = [_Attr(a["key"], a["value"], a["leading"], a["trailing"], a["orig"])
                    for a in d["attrs"]]
        n._leading, n._orig, n._parent = d["leading"], d["orig"], parent
        kids = d["children"]
        n._children = None if kids is None else [Node._from_native(k, n) for k in kids]
        return n

    def _native(self) -> dict:
        return {"kind": self._kind, "name": self._name,
                "attrs": [a.native() for a in self._attrs],
                "children": None if self._children is None
                else [c._native() for c in self._children],
                "leading": self._leading, "orig": self._orig}

    # -- identity
    @property
    def kind(self) -> str:
        return self._kind

    @kind.setter
    def kind(self, k: str) -> None:
        if not _native.is_ident(k):
            raise ValueError(f"{k!r} is not a valid node kind")
        self._kind = k

    @property
    def name(self) -> Optional[str]:
        return self._name

    @name.setter
    def name(self, n: Optional[str]) -> None:
        if n is not None and not isinstance(n, str):
            raise TypeError("name must be a str or None")
        self._name = n

    # -- attributes
    @property
    def attrs(self) -> Attrs:
        return Attrs(self)

    def __getitem__(self, key: str) -> Any:
        return self.attrs[key]

    def __setitem__(self, key: str, value: Any) -> None:
        self.attrs[key] = value

    def __delitem__(self, key: str) -> None:
        del self.attrs[key]

    def __contains__(self, key: str) -> bool:
        return key in self.attrs

    def get(self, key: str, default: Any = None) -> Any:
        return self.attrs.get(key, default)

    def set(self, **attrs: Any) -> "Node":
        """Set several attributes; returns the node for chaining."""
        for k, v in attrs.items():
            self.attrs[k] = v
        return self

    # -- structure
    @property
    def children(self) -> Children:
        return Children(self)

    @property
    def has_block(self) -> bool:
        """Whether the node has a ``{ … }`` block (possibly empty)."""
        return self._children is not None

    @has_block.setter
    def has_block(self, v: bool) -> None:
        if v and self._children is None:
            self._children = []
        elif not v:
            if self._children:
                raise ValueError("remove the children before dropping the block")
            self._children = None

    @property
    def parent(self) -> Optional["Node"]:
        return self._parent if isinstance(self._parent, Node) else None

    @property
    def document(self) -> Optional["Document"]:
        p = self._parent
        while isinstance(p, Node):
            p = p._parent
        return p

    def _ancestors_and_self(self) -> list["Node"]:
        out, n = [], self
        while isinstance(n, Node):
            out.append(n)
            n = n._parent
        return out

    def add(self, kind: Union[str, "Node"], name: Optional[str] = None, /, **attrs: Any) -> "Node":
        """Append a child (a new node of ``kind``, or an existing ``Node``)."""
        child = kind if isinstance(kind, Node) else Node(kind, name, **attrs)
        self.children.append(child)
        return child

    def remove(self) -> "Node":
        """Detach from the parent; returns self."""
        p = self._parent
        if p is not None:
            lst = p._nodes if isinstance(p, Document) else p._children
            lst.remove(self)
            self._parent = None
        return self

    def copy(self) -> "Node":
        """Deep, detached copy. Formatting of the copied subtree is kept."""
        n = Node._from_native(self._native(), None)
        if n._leading is not None and "//" not in n._leading:
            n._leading = None
        return n

    @property
    def source(self) -> str:
        """This node's ``.mog`` text."""
        d = self._native()
        d["leading"] = ""
        doc = self.document
        text, _ = _native.print([d], "", doc._indent if doc else "  ")
        return text

    def __getattr__(self, kind: str):
        # `node.box(...)` → append a child of that kind.
        k = _KIND_ALIASES.get(kind, kind)
        if k in _KIND_SET:
            return lambda name=None, /, **attrs: self.add(k, name, **attrs)
        raise AttributeError(f"{type(self).__name__!r} has no attribute {kind!r}")

    def __repr__(self) -> str:
        head = self._kind + (f" {self._name!r}" if self._name is not None else "")
        bits = []
        if self._attrs:
            bits.append(", ".join(f"{a.key}={a.value}" for a in self._attrs[:4])
                        + (", …" if len(self._attrs) > 4 else ""))
        if self._children:
            bits.append(f"{len(self._children)} children")
        return f"<Node {head}" + (f" ({'; '.join(bits)})" if bits else "") + ">"

    def __bool__(self) -> bool:
        return True  # don't fall back to __len__-like truthiness


class Document(_Container):
    """A ``.mog`` file. Build one up, or :func:`load` an existing file."""

    def __init__(self, source: Optional[str] = None, *, path: Union[str, os.PathLike, None] = None):
        self.path: Optional[Path] = Path(path) if path is not None else None
        self.base_dir: Optional[Path] = None
        """Directory for resolving ``import``s and textures; defaults to the
        file's directory, else the current directory."""
        self._nodes: list[Node] = []
        self._tail = "\n"
        self._indent = "  "
        if source is not None:
            try:
                nodes, self._tail, self._indent = _native.parse(source)
            except ValueError as e:
                raise ParseError(str(e)) from None
            self._nodes = [Node._from_native(d, self) for d in nodes]

    # -- structure
    @property
    def nodes(self) -> Children:
        """Top-level nodes."""
        return Children(self)

    children = nodes

    def add(self, kind: Union[str, Node], name: Optional[str] = None, /, **attrs: Any) -> Node:
        """Add a top-level node. Declarations (``material``, ``module``, …)
        go before the first ``scene``; anything else goes at the end."""
        n = kind if isinstance(kind, Node) else Node(kind, name, **attrs)
        scene = next((i for i, x in enumerate(self._nodes) if x.kind == "scene"), None)
        if n.kind in _DECLARATIONS and scene is not None:
            self.nodes.insert(scene, n)
        else:
            self.nodes.append(n)
        return n

    def scene(self) -> Node:
        """The top-level ``scene`` node, created if missing. Use as
        ``with doc.scene():`` to build into it."""
        s = next((n for n in self._nodes if n.kind == "scene"), None)
        if s is None:
            s = self.add("scene")
            s.has_block = True
        return s

    def material(self, name: str, /, **attrs: Any) -> Node:
        """Declare a material, or update it if ``name`` already exists."""
        m = next((n for n in self._nodes if n.kind == "material" and n.name == name), None)
        if m is None:
            return self.add("material", name, **attrs)
        return m.set(**attrs)

    def module(self, name: str, /, **params: Any) -> Node:
        """Declare a module; build its body with ``with doc.module(...) as m:``
        and reference parameters with :data:`P` (``P.h`` → ``$h``)."""
        m = self.add("module", name, **params)
        m.has_block = True
        return m

    @property
    def meta(self) -> Optional[Node]:
        """The ``meta (...)`` node, if the file has one."""
        return next((n for n in self._nodes if n.kind == "meta"), None)

    def set_meta(self, **attrs: Any) -> Node:
        """Set ``meta`` fields (``name``, ``description``, ``tags``, …),
        creating the block at the top of the file if needed."""
        m = self.meta
        if m is None:
            m = Node("meta")
            self.nodes.insert(0, m)
            if len(self._nodes) > 1:
                # Keep a file-header comment above the new meta block.
                first = self._nodes[1]
                m._leading, first._leading = first._leading, "\n\n"
        return m.set(**attrs)

    @property
    def materials(self) -> dict[str, Node]:
        return {n.name: n for n in self._nodes if n.kind == "material" and n.name}

    def __getattr__(self, kind: str):
        k = _KIND_ALIASES.get(kind, kind)
        if k in _KIND_SET:
            return lambda name=None, /, **attrs: self.add(k, name, **attrs)
        raise AttributeError(f"'Document' has no attribute {kind!r}")

    # -- text
    def _print(self) -> tuple[str, list[tuple[int, int]]]:
        return _native.print([n._native() for n in self._nodes], self._tail, self._indent)

    @property
    def source(self) -> str:
        return self._print()[0]

    def __str__(self) -> str:
        return self.source

    def __repr__(self) -> str:
        where = f" {str(self.path)!r}" if self.path else ""
        return f"<Document{where} ({len(self._nodes)} top-level nodes)>"

    def save(self, path: Union[str, os.PathLike, None] = None, *, stamp: bool = True) -> Path:
        """Write the document (``.mogb`` paths are packed as MOGB). Stamps
        ``meta.mogen_version`` like every MoGen tool does, unless
        ``stamp=False``."""
        target = Path(path) if path is not None else self.path
        if target is None:
            raise ValueError("no path: pass one to save() or set doc.path")
        meta = self.meta
        if stamp and (meta is None or meta.get("mogen_version") != __version__):
            self.set_meta(mogen_version=__version__)
        _native.write_source(target, self.source)
        self.path = target
        return target

    # -- compile
    def _base(self) -> str:
        if self.base_dir is not None:
            return str(self.base_dir)
        if self.path is not None:
            return str(self.path.resolve().parent)
        return os.getcwd()

    def _diagnostics(self, raw: list[dict], text: str, spans) -> list[Diagnostic]:
        order = list(self.walk())
        out = []
        for d in raw:
            diag = Diagnostic(d["severity"], d["code"], d["message"], file=d["file"],
                              span=tuple(d["span"]) if d["span"] else None)
            if diag.file is None:
                diag.file = str(self.path) if self.path else None
                if diag.span is not None:
                    s, e = diag.span
                    before = text[:s]
                    diag.line = before.count("\n") + 1
                    diag.column = s - (before.rfind("\n") + 1) + 1
                    # Innermost node containing the start (last match in
                    # pre-order). Compiler spans can run on over trailing
                    # whitespace, so the end isn't compared.
                    for node, (ns, ne) in zip(order, spans):
                        if ns <= s < ne:
                            diag.node = node
            out.append(diag)
        out.sort(key=lambda d: d.severity != "error")
        return out

    def check(self) -> list[Diagnostic]:
        """Validate and lower without exporting; returns all diagnostics."""
        text, spans = self._print()
        return self._diagnostics(_native.check(text, self._base()), text, spans)

    def build(self, out: Union[str, os.PathLike, None] = None, *, format: Optional[str] = None) -> BuildResult:
        """Compile to GLB (or FBX: ``format="fbx"`` or an ``.fbx`` path).
        Writes ``out`` when given. Raises :class:`BuildError` on errors."""
        out_path = Path(out) if out is not None else None
        fmt = format or ("fbx" if out_path and out_path.suffix.lower() == ".fbx" else "glb")
        text, spans = self._print()
        data, raw = _native.compile(text, self._base(), fmt)
        diags = self._diagnostics(raw, text, spans)
        if data is None:
            raise BuildError(diags)
        if out_path is not None:
            out_path.write_bytes(data)
        return BuildResult(data, diags, out_path)


# --- entry points -----------------------------------------------------------


def load(path: Union[str, os.PathLike]) -> Document:
    """Open a ``.mog`` or ``.mogb`` file for editing."""
    return Document(_native.read_source(Path(path)), path=path)


def loads(source: str) -> Document:
    """Parse ``.mog`` source text."""
    return Document(source)


def node(kind: str, name: Optional[str] = None, /, **attrs: Any) -> Node:
    """Create a node of any kind. Inside a ``with`` block it's appended to
    that node (or document); otherwise it's returned detached."""
    target = _target.get()
    if target is None:
        return Node(kind, name, **attrs)
    return target.add(kind, name, **attrs)


# Kind schema straight from the compiler; `_kinds.py` (generated) holds the
# static builder functions, and `Node`/`Document` attribute builders use this.
KINDS: dict[str, tuple[str, ...]] = {k: tuple(attrs) for k, attrs in _native.kinds()}
_KIND_SET = frozenset(KINDS)
_KIND_ALIASES = {f"{k}_": k for k in KINDS if keyword.iskeyword(k)}
