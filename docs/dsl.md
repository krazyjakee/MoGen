# `mgen` DSL reference

`mgen` reads `.mg` source files, lowers them to an intermediate scene graph,
and exports glTF 2.0 GLB. This document is the authoritative reference for
the surface language: every node kind, every attribute, and the little bits of
grammar that sit between them. For a conceptual overview of the whole
pipeline, see [`ROADMAP.md`](./ROADMAP.md); for a worked catalog of reusable
modules, see [`modules.md`](./modules.md).

- [Grammar at a glance](#grammar-at-a-glance)
- [Values and expressions](#values-and-expressions)
- [Common attributes](#common-attributes)
- [Scene structure](#scene-structure-scene-group)
- [Primitives](#primitives)
- [Materials](#materials)
- [Connectors](#connectors)
- [Replicators: `mirror` and `array`](#replicators-mirror-and-array)
- [CSG: `union` / `difference` / `intersect`](#csg-union--difference--intersect)
- [Modules: `module` and `use`](#modules-module-and-use)
- [Animation: `joint`, `clip`, templates](#animation-joint-clip-templates)
- [Full example](#full-example)

---

## Grammar at a glance

A `.mg` file is a sequence of nodes. Every node shares the same shape:

```
kind ["optional name"] [(attr=value, ...)] [{ child_nodes... }]
```

- `kind` is an identifier like `box`, `cylinder`, `group`, `scene`, `material`, `joint`, …
- `name` is a quoted string. Some kinds require it (`material`, `module`, `joint`, `clip`, `connector`); most geometry kinds treat it as optional — when omitted the node's name defaults to its kind.
- `attr_list` is a comma-separated `key=value` sequence in parentheses.
- `block` is a brace-delimited list of child nodes.

Comments run from `//` to the end of the line. Whitespace between tokens is
insignificant. The top of the file may contain `material`, `module`, `joint`,
and `clip` declarations; `scene { ... }` holds the geometry itself.

---

## Values and expressions

Every `value` on the right side of an attribute is one of:

| form | example | notes |
|---|---|---|
| number | `0.5`, `-90`, `1` | parsed as `f32` |
| vec3 | `[1.0, 0.5, 0.0]` | three expressions, comma-separated |
| list | `[0, 90]`, `[1, 2, 3, 4]` | arbitrary arity |
| string | `"wood"` | used for names and references |
| ident | `wood`, `y` | no quotes; used for axes and enum-like values |
| expression | `$height * 0.5`, `$r + 0.1`, `($a - $b) / 2` | arithmetic over `$param` refs |

Inside `module` bodies, any expression may reference a declared parameter as
`$name`. Expressions support `+ - * /` with conventional precedence and
parentheses. An expression is evaluated at module-expansion time — by the
time the scene graph is built, every `$name` has been replaced with a
concrete number.

---

## Common attributes

These apply to every geometry node (`box`, `cylinder`, …) and to `group`:

| attribute | value | effect |
|---|---|---|
| `pos` | `vec3` | translation in the parent's frame; default `[0, 0, 0]` |
| `rot` | `vec3` (Euler XYZ in **degrees**) or a list | rotation applied after translation; default identity |
| `scale` | scalar or `vec3` | uniform/per-axis scale; default `1` |
| `mat` | string or ident | references a declared `material` by name |
| `role` | string or ident | semantic label; written into the GLB `extras` block |
| `tags` | comma-separated string | free-form labels; also in `extras` |

Transforms compose from child → parent along the scene hierarchy, exactly as
in glTF.

---

## Scene structure: `scene`, `group`

```
scene {
  group "chair" (pos=[0, 0, 0]) {
    box "seat" (pos=[0, 0.5, 0], size=[1.0, 0.1, 1.0])
  }
}
```

- `scene` is the root container. Exactly one per file is expected in practice; top-level nodes outside any `scene` are also lowered and become extra roots.
- `group` is a transform-only container — no geometry of its own, used to compose children and receive `pos`/`rot`/`scale`.

---

## Primitives

All primitives accept the common attributes above (`pos`, `rot`, `scale`,
`mat`, `role`, `tags`) plus the kind-specific attributes below.

| kind | required attrs | other attrs |
|---|---|---|
| `box` | `size=[x,y,z]` | — |
| `plane` | `size=[x,_,z]` (Y ignored) | — |
| `quad` | `size=[w,h]` or `vec3` (w,h,_) | — |
| `cylinder` | `radius`, `height` | `segments` (default 24) |
| `cone` | `radius`, `height` | `segments` (default 24) |
| `sphere` | `radius` | `rings` (16), `segments` (24) |
| `capsule` | `radius`, `height` | `rings` (8), `segments` (24) |
| `torus` | `major`, `minor` | `major_segments` (24), `minor_segments` (12) |
| `prism` | `size=[x,y,z]` | triangular prism along +Z |
| `pyramid` | `radius`, `height`, `sides` | N-sided pyramid base |
| `disc` | `radius` | `segments` (24) |
| `icosphere` | `radius` | `subdivisions` (2) |
| `rounded_box` | `size=[x,y,z]`, `radius` | `segments` per corner (4) |
| `ellipsoid` | `size=[x,y,z]` | `rings` (16), `segments` (24); independent radii per axis |
| `superellipsoid` | `size=[x,y,z]` | `ew`, `ns` (1 = sphere, > 1 boxy, < 1 pinched), `rings` (16), `segments` (24) |
| `curved_plane` | `size=[x,z]` or `vec3` | `bend_u`, `bend_v` (degrees; arc angle along X/Z), `segments_u`/`segments_v` (12) |
| `lathe` | `profile=[[r,y], …]` | `segments` (24), `cap_ends` (1 = capped); profile authored bottom-to-top in `(radius, y)` pairs |
| `spline_tube` | `points=[[x,y,z], …]` | `radius` (scalar) or `radii=[…]` (per-point), `segments` (12), `samples` (8), `cap_ends` (1) |

`plane` and `quad` are both flat single-quad meshes; `plane` is XZ-aligned,
`quad` is XY-aligned (useful for UI-style panels).

`superellipsoid` is the workhorse for smooth organic bodies (eggs, pears,
bullet shapes) and stylised soft boxes — pick `ew`/`ns` together for a
symmetric shape, or split them for asymmetric profiles like an apple
(`ew=1.2`, `ns=0.8`).

`curved_plane`, `lathe`, and `spline_tube` accept nested list literals:
`points=[[0, 0, 0], [1, 0.5, 0]]`, `profile=[[0.2, 0], [0.5, 0.4]]`. Inner
lists must be constant (no `$param`) — parameterise the whole node via a
module wrapper instead. `spline_tube` runs a Catmull–Rom curve through its
control points and uses a parallel-transport frame so the cross-section
doesn't flip at inflection points.

Default values mean that `cylinder "leg"` with no attrs is a 1 m unit-radius
cylinder centered on the origin. Every primitive is authored in its local
frame and then positioned via `pos`/`rot`/`scale`.

---

## Materials

```
material "wood"  (color=[0.55, 0.35, 0.18], metallic=0.0, roughness=0.8)
material "glass" (color=[0.9, 0.95, 1.0], alpha=0.3, roughness=0.05, transmission=0.9)
material "neon"  (color=[1, 0.2, 1], emissive=[1, 0.2, 1], emissive_strength=8.0)
material "leaf"  (color=[0.25, 0.6, 0.2], alpha_mode="mask", alpha_cutoff=0.5)
```

Declared at the top of the file or inside `scene { ... }`. Attributes:

- `color` — vec3 `[r, g, b]` in linear space; alpha defaults to `1.0`.
- `alpha` — optional alpha override for transparency. Setting `alpha < 1`
  without an explicit `alpha_mode` auto-selects `"blend"`.
- `metallic` — `0.0`–`1.0`, default `0.0`.
- `roughness` — `0.0`–`1.0`, default `0.9`.
- `alpha_mode` — `"opaque"` (default), `"blend"` (translucent), or `"mask"`
  (1-bit cutout, e.g. foliage).
- `alpha_cutoff` — threshold for `alpha_mode="mask"`, default `0.5`.
- `emissive` — vec3 glow colour added on top of PBR shading. Use this for
  screens, embers, lava. Default `[0, 0, 0]`.
- `emissive_strength` — HDR multiplier on `emissive`
  (`KHR_materials_emissive_strength`). Values `> 1.0` drive bloom and produce
  the saturated, "fluorescent paint" look. Default `1.0`.
- `transmission` — `0.0`–`1.0` fraction of light that passes through the
  surface (`KHR_materials_transmission`). `0` is opaque PBR, `1` is perfectly
  clear glass. Orthogonal to `alpha_mode` — use this for glass and water,
  `alpha`/`alpha_mode` for gels, tints, and smoke.
- `double_sided` — `0` (default) or `1`. When `1`, the renderer draws both
  faces of the triangle (glTF `doubleSided`). Use for leaves, fins, flags,
  cloth, and any thin `curved_plane`/`plane`/`disc`/`quad` whose underside
  can be seen. This is the correct fix for tilted or bent single-sided
  geometry — mirroring a bent `curved_plane` along its bend axis does **not**
  produce a double-sided surface; it produces two sheets curling away from
  each other.

Reference a material on any geometry or group via `mat="wood"`. The lookup is
by exact string match; unknown names are a hard error at lowering.

---

## Connectors

Connectors are oriented frames that a node exposes so other nodes can attach
to it. They do not produce geometry.

```
box "seat" (pos=[0, 0.5, 0], size=[1.0, 0.1, 1.0]) {
  connector "top"    (at=[0,  0.05, 0], dir=[0,  1, 0], tag=seat_top)
  connector "bottom" (at=[0, -0.05, 0], dir=[0, -1, 0], tag=seat_bottom)
}
```

Attributes:

| attribute | value | default |
|---|---|---|
| `at` | `vec3` | `[0, 0, 0]` |
| `dir` | `vec3` (any nonzero) | `[0, 1, 0]` |
| `tag` | string or ident | empty |
| `radius` | number | — (unset) |

Internally a connector is stored as a position plus a quaternion that rotates
canonical `+Y` onto `dir`. `tag` groups compatible attach points (e.g. every
leg top shares `tag=leg_top`) so downstream fitting logic can pair them.

---

## Replicators: `mirror` and `array`

Both are "wrapper" nodes: they create one parent group and replicate their
children under it.

### `mirror`

```
mirror "pair" (axis=x) {
  sphere "ball" (pos=[0.5, 0.5, 0], radius=0.25)
}
```

`axis` is `x`, `y`, or `z` (ident or string). The body is emitted twice —
once unchanged and once with the named axis negated. Use it for left/right
symmetry where only one side is authored by hand.

### `array`

```
array "legs" (count=4, around=y) {
  group "offset" (pos=[0.45, 0, 0.45]) {
    cylinder "leg" (radius=0.05, height=0.5)
  }
}
```

Attributes:

- `count` — number of copies (integer); default `1`.
- `around` — `x` / `y` / `z` ident; the rotation axis. Default `y`.
- `start_angle` — degrees offset of the first copy; default `0`.

The children are cloned `count` times; the i-th copy is rotated by
`start_angle + 360° * i / count` around `around`. Combine with an offset
`group` (as above) to place the first copy off the rotation axis; the array
then fans it into a ring.

---

## CSG: `union` / `difference` / `intersect`

CSG ops fold their children into a single mesh that hangs off the op node
itself — the operand children do not become separate scene nodes.

```
difference "wall_with_door" (mat="concrete") {
  box "wall"    (size=[4.0, 3.0, 0.2])
  box "doorway" (pos=[0, -0.5, 0], size=[0.9, 2.0, 0.5])
}
```

- `union` — N ≥ 1 operands; the union of all.
- `difference` — the first operand minus every subsequent operand.
- `intersect` — N ≥ 2 operands; the shared volume.

Operand transforms are baked into the vertices at evaluation time, so each
operand lives in the parent's frame regardless of its local `pos`/`rot`.
Connectors and `material` children declared directly on the CSG node still
apply; any on operand children are ignored.

The output is cleaned (vertex welding, degenerate-tri cull, normal recompute)
to give the exporter a watertight mesh.

---

## Modules: `module` and `use`

Modules are parametric sub-graphs. A declaration lives at the top level of
the file:

```
module "leg" (height=0.5, radius=0.05) {
  cylinder "leg" (pos=[0, $height * 0.5, 0],
                  radius=$radius, height=$height, mat="wood") {
    connector "top" (at=[0, $height * 0.5, 0], dir=[0, 1, 0], tag=leg_top)
  }
}
```

Parameters:

- Each parameter has a **scalar default** (number or expression). `vec3`,
  `list`, string, or ident defaults are rejected — `$param` substitution is
  numeric.
- Parameters are referenced inside the body as `$name`. They participate in
  `pos`, `rot`, `scale`, `radius`, `height`, etc., and inside nested `vec3`
  expressions like `[0, $h * 0.5, 0]`.

Invoke a module with `use`:

```
scene {
  group "chair" {
    use "leg" (height=0.6, radius=0.04)
    array "legs" (count=4, around=y) {
      group "offset" (pos=[0.45, 0, 0.45]) {
        use "leg" (height=0.5, radius=0.05)
      }
    }
  }
}
```

Rules:

- `use` takes the module's **declared** name. Unknown names fail with a clear error.
- Omitted arguments fall back to declared defaults. Unknown argument names are a hard error (catches typos).
- Modules may call other modules. Recursion is detected and rejected.
- Expansion is lexically scoped — `$param` references outside a module body are rejected.

---

## Animation: `joint`, `clip`, templates

All animation in v1 lowers to glTF node-transform animation tracks. There is
no skeleton / skinning (see M10 for that).

### Joints

A `joint` names an articulation, picks a DOF type, and points at the scene
node that rotates/translates when the joint moves.

```
joint "door_hinge" (type=hinge, axis=[0, 1, 0], limits=[0, 100], pivot="door")
```

| attribute | value | notes |
|---|---|---|
| `type` | `hinge`, `slider`, `ball`, `rotor` | DOF kind |
| `pivot` | string — a node name | required |
| `axis` | vec3 | default `[0, 1, 0]` |
| `limits` | `[lo, hi]` list | optional; degrees (rotary) or meters (slider) |

### Authored clips

```
clip "open" (seconds=1.0) {
  track "door_hinge" (from=0, to=90)
}
```

- `clip` holds a single duration and an ordered list of `track` children.
- `track` targets a joint (by name) or a scene node directly. When targeting
  a node, add `prop="translation"|"rotation"|"scale"` to pick the channel.
- `from` / `to` are scalars. For rotation they're degrees around the joint's
  `axis`; for translation they're distance along the axis; for scale they're
  the uniform factor.
- Two keyframes are emitted at `0` and `seconds`; the exporter linearly
  interpolates between them.

### Procedural templates

One-line declarations that expand into a full clip. They all take a
`target="name"` pointing at a joint or a scene node.

| template | extra attrs | effect |
|---|---|---|
| `spin` | `axis`, `rpm` (60) | continuous rotation |
| `open_close` | `axis`, `angle` (90), `seconds` (1.0) | 0° → angle → 0° swing |
| `wave` | `axis`, `amplitude` (15°), `hz` (1.0) | sinusoidal wobble |
| `flap` | `axis`, `amplitude` (30°), `hz` (2.0) | faster wobble, bigger amplitude |
| `idle` | `amplitude` (0.02 m), `hz` (0.5) | tiny translation breathe |

When the target is a joint, its `axis` is used by default; when it's a node,
pass `axis` explicitly.

```
spin "rotor_spin" (target="rotor", axis=[0, 0, 1], rpm=30)
open_close "door_swing" (target="door_hinge", angle=90, seconds=1.2)
```

---

## Full example

A door in a wall, with a swinging-open animation, built end-to-end:

```
material "wood"     (color=[0.55, 0.35, 0.18], roughness=0.8)
material "concrete" (color=[0.78, 0.78, 0.78], roughness=0.85)

scene {
  difference "wall_with_door" (mat="concrete", role="wall") {
    box "wall"       (size=[4.0, 3.0, 0.2])
    box "door_gap"   (pos=[0, -0.5, 0], size=[0.9, 2.0, 0.5])
  }

  // Hinge at the left edge: offset the panel by half-width inside the group.
  group "door" (pos=[-0.45, 1.0, 0]) {
    box "panel" (pos=[0.45, 0, 0], size=[0.9, 2.0, 0.04], mat="wood")
  }
}

joint "door_hinge" (type=hinge, axis=[0, 1, 0], limits=[0, 100], pivot="door")
clip "open" (seconds=1.2) {
  track "door_hinge" (from=0, to=90)
}
```

Compile with `mgen build examples/<file>.mg -o out.glb` and open in Godot
4.x or any glTF-2.0 viewer.

---

## Diagnostics and tooling

- `mgen check <file>.mg` validates without building. Pass `--json` for machine-readable diagnostics (the format the LLM repair loop consumes).
- `mgen dump-scene <file>.mg --json` prints the lowered graph for debugging.
- `mgen inspect <file>.glb` reads back a GLB and prints its top-level structure.

See [`ROADMAP.md`](./ROADMAP.md) §8 for the full diagnostic catalog.
