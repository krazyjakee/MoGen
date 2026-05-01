# MoGen DSL reference

`mogen` reads `.mog` source files, lowers them to an intermediate scene graph,
and exports glTF 2.0 GLB. This document is the authoritative reference for
the surface language: every node kind, every attribute, and the little bits of
grammar that sit between them. For a conceptual overview of the whole
pipeline, see [`ROADMAP.md`](./ROADMAP.md); for a worked catalog of reusable
modules, see [`modules.md`](./modules.md).

- [Grammar at a glance](#grammar-at-a-glance)
- [Values and expressions](#values-and-expressions)
- [Common attributes](#common-attributes)
- [Placement shortcuts](#placement-shortcuts)
- [Scene structure](#scene-structure-scene-group)
- [Primitives](#primitives)
- [Materials](#materials)
- [Connectors](#connectors)
- [Replicators: `mirror`, `array`, `stack`, `grid`](#replicators-mirror-array-stack-grid)
- [CSG: `union` / `difference` / `intersect`](#csg-union--difference--intersect)
- [Modules: `module` and `use`](#modules-module-and-use)
- [Imports: `import`](#imports-import)
- [Animation: `joint`, `clip`, templates](#animation-joint-clip-templates)
- [Lights: `light`](#lights-light)
- [Full example](#full-example)

---

## Grammar at a glance

A `.mog` file is a sequence of nodes. Every node shares the same shape:

```
kind ["optional name"] [(attr=value, ...)] [{ child_nodes... }]
```

- `kind` is an identifier like `box`, `cylinder`, `group`, `scene`, `material`, `joint`, …
- `name` is a quoted string. Some kinds require it (`material`, `module`, `joint`, `clip`, `connector`); most geometry kinds treat it as optional — when omitted the node's name defaults to its kind.
- `attr_list` is a comma-separated `key=value` sequence in parentheses.
- `block` is a brace-delimited list of child nodes.

Comments run from `//` to the end of the line. Whitespace between tokens is
insignificant. The top of the file may contain `import`, `material`,
`module`, `joint`, and `clip` declarations; `scene { ... }` holds the
geometry itself.

---

## Global settings

Top-level directives that tune the build itself rather than describing
geometry. They sit at the file level (alongside `material` / `module`) and are
consumed during lowering.

| directive | value | effect |
|---|---|---|
| `lod_scale (value=N)` | number, default `1.0` | multiplies primitive default `segments` / `rings` / `samples`. `0.5` halves them, `2.0` doubles them. `icosphere` `subdivisions` step by `round(log2(N))` instead, since each step quadruples its triangle count. Per-primitive values (`segments=24`, `rings=16`, …) are absolute and are **not** scaled. |

```
lod_scale (value=0.5)

scene {
  sphere "head" (radius=0.5)         // 12 rings, 12 segments (default 16/24 halved)
  sphere "lod0" (radius=0.5, segments=48, rings=32)  // explicit values keep 48/32
}
```

The studio's "LOD scale" slider (under the build summary) edits this directive
in place — drag it down to iterate quickly on big scenes, then drag back to
`1.0` for export. The slider clears the directive when it returns to `1.0` so
saved files stay clean by default.

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

## Placement shortcuts

Every node accepts a family of ergonomic shortcuts on top of the classic
`pos`/`rot`/`size` vec3s. They exist for one reason: an LLM should never need
to do arithmetic that the DSL can do for it. Mix and match freely.

### Per-component shortcuts

| shortcut | replaces / overrides | notes |
|---|---|---|
| `x=`, `y=`, `z=` | individual components of `pos` | missing axes default to `pos`'s value, or `0` |
| `rx=`, `ry=`, `rz=` | individual components of `rot` (degrees) | same fallback; great for single-axis spins |
| `w=`, `h=`, `d=` | individual components of `size` (X, Y, Z) | for 2D primitives, `w`/`d` are used on `plane`/`curved_plane` (XZ) and `w`/`h` on `quad` (XY) |

```
box (y=1.5, size=1)            // equivalent to pos=[0, 1.5, 0], size=[1,1,1]
box (size=[2, 2, 0.1], h=3)    // h overrides the middle component — width=2, height=3, depth=0.1
cylinder (rx=90, radius=0.2, height=1)   // lay a cylinder on its side
```

### Scalar `size` (cube shorthand)

Any primitive that takes `size=[…]` also accepts `size=<number>`, which
expands to a uniform vec3. `box (size=0.5)` is a half-metre cube.

### `from` / `to` — axis-aligned box by corners

On any primitive that uses `size`, `from=[x1,y1,z1]` + `to=[x2,y2,z2]` sets
`size` to `|to − from|` and `pos` to their midpoint. No "shift by half" math:

```
box (from=[-2, 0, -1.5], to=[2, 2.8, -1.4], mat="wall")
// equivalent to: box (pos=[0, 1.4, -1.45], size=[4.0, 2.8, 0.1])
```

### `anchor` — place by face, not centre

Every primitive's `pos` controls where its **anchor point** lands, not where
its centre lands. The default anchor is `center`; `anchor=bottom` puts the
primitive's bottom face at `pos`, which is usually what "sit on the ground"
means. Values are underscore-joined tokens drawn from
`center`, `top`, `bottom`, `left`, `right`, `front`, `back`:

```
box (y=0,  size=[1, 2, 1], anchor=bottom)           // bottom face on y=0
box (xyz, size=2,           anchor=bottom_left_front) // corner at the origin
```

Internally the anchor shifts the mesh vertices so the chosen point is at the
local origin. The six default face connectors (`top`, `bottom`, `left`, …)
move with the shift, so attach/connector math stays correct.

### Relative placement: `above`, `below`, `left_of`, `right_of`, `in_front_of`, `behind`

Set one of these to the name of a **prior sibling** in the same parent; the
node is translated so its matching face is flush against the sibling's
opposite face, optionally plus `gap`. At most one may be set per node.

```
group "chests" {
  box "chest_lo" (size=[0.8, 0.6, 0.5])
  box "chest_hi" (above="chest_lo", gap=0.02, size=[0.8, 0.6, 0.5])
}
```

Resolution happens after the target's subtree is fully lowered, so nested
geometry is included in the AABB. Lookup is scoped to siblings in the same
parent, so replicated subtrees (`array`, `grid`) don't collide with
identically named nodes elsewhere.

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
- `solid` behaves like `group` in the scene tree, but its same-material leaf children are CSG-unioned into a single mesh at export time. See [Solid groups](#solid-groups-solid) below.

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
| `slab` | `size=[x,y,z]` | box alias; default anchor `bottom` (sits on ground) |
| `post` | `size=[x,y,z]` | box alias; default anchor `bottom` (pillar/leg) |
| `panel` | `size=[x,y,z]` | box alias; default anchor `back` (flat panel flush to a surface) |
| `wall` | `size=[x,y,z]` | `holes=[[x,y,w,h], …]` — rectangular cutouts through the Z axis |

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

`slab`, `post`, and `panel` are box aliases that exist only to change the
**default anchor** — their geometry is identical to `box`. Use them to make
"this sits on the ground" or "this is a wall-hung panel" the one-line thing
it should be, without `anchor=…` on every row. You can still override
`anchor=` explicitly if you need something different.

`wall` is a box with rectangular cutouts along Z. Each hole is a 4-element
sublist `[cx, cy, w, h]` in the wall's local frame (X/Y are the face plane;
the Z thickness axis is cut all the way through). Cutouts are applied via
CSG `difference` at lowering time and the result is welded/cleaned, so a
single `wall` node becomes one watertight mesh — no nested `difference`
idiom needed:

```
wall "barracks" (size=[3, 3, 0.1], holes=[
  [-0.75, -0.4, 0.9, 2.0],   // door
  [ 0.9,  0.3, 0.8, 0.8],    // window
])
```

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
- `normal_strength` — slope multiplier baked into the *derived* normal map
  by `mogen textures`. Larger = more pronounced bumps. Range `~0..8`, default
  `1.5`. Has no effect if `normal_texture` is authored directly.
- `occlusion_strength` — `0.0`–`1.0` ceiling on how dark the *derived* AO
  map can get. `0` emits flat white (no darkening), `1` lets cavities reach
  black. Default `0.7`. Has no effect if `occlusion_texture` is authored
  directly.
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
- `uv_mode` — `"tile"` (default) or `"fit"`. Controls how textures map onto
  the geometry. `"tile"` emits world-space UVs so 1 world unit = 1 texture
  tile (scaled by `uv_scale`). Texel density is identical across every
  primitive that uses the material — the right choice for repeating surfaces
  like stone walls, wood planks, fabric, ground, and roof shingles. `"fit"`
  falls back to per-face `[0, 1]²` UVs so every face of the primitive shows
  the full image once — the right choice when the texture *is* the picture:
  signs, paintings, decals, stained-glass panes, anything whose image must
  land at a specific place on a specific face. Pick `"fit"` for
  image-as-texture; leave the default for material-as-texture.
- `uv_scale` — `1.0` (default), a scalar (`uv_scale=2`), or a vec2
  (`uv_scale=[2, 1]`). In `tile` mode this is "tiles per world unit": `2`
  doubles the tiling density (smaller bricks), `0.5` halves it (bigger
  bricks). In `fit` mode it multiplies the `[0, 1]` coords — `> 1` repeats
  the image inside a face, `< 1` zooms into a sub-region. Per-axis vec2
  form lets you stretch a texture asymmetrically (planks on a floor, bands
  on a column).
- `base_color_texture` — string path to an `.png`/`.jpg` file on disk,
  resolved relative to the `.mog` file. Multiplied against `color`. sRGB.
- `metallic_roughness_texture` — packed metal/rough map (glTF convention:
  green = roughness, blue = metallic). Linear.
- `normal_texture` — tangent-space normal map. Linear.
- `occlusion_texture` — ambient occlusion (red channel). Linear.
- `emissive_texture` — emissive colour map, multiplied against `emissive`.
  sRGB.

Example:

```
material "oak" (
  color=[1, 1, 1],
  roughness=0.8,
  base_color_texture="textures/oak_albedo.png",
  normal_texture="textures/oak_normal.png"
)
```

Texture files are embedded in the output GLB, so the resulting `.glb` is
self-contained and can be moved without the source images. Missing files
are a hard error at export.

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

When a node is the `child` of an `attach`, its `pos` / `rot` are still honoured
as a local offset on top of the alignment — `pos` shifts the anchor in the
parent's frame and `rot` rotates the aligned node around its anchor — so a
Studio gizmo drag persists across rebuilds.

---

## Replicators: `mirror`, `array`, `stack`, `grid`

Wrapper nodes that create one parent group and either replicate or lay out
their children. All four accept the usual transform attributes so the whole
cluster can be positioned as a unit.

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

### `stack`

Lay children out along one axis, using each child's computed AABB as its
"slot". No half-size math, no accumulated offsets to maintain by hand.

```
stack "cake" (axis=y, gap=0.02) {
  slab "tier_a" (size=[1.4, 0.25, 1.4])
  slab "tier_b" (size=[1.0, 0.20, 1.0])
  slab "tier_c" (size=[0.6, 0.15, 0.6])
}
```

Attributes:

| attribute | value | default | effect |
|---|---|---|---|
| `axis` | `x`, `y`, `z` | `y` | stacking direction |
| `gap` | number | `0` | spacing between consecutive children |
| `align` | `center`, `start`, `end` | `center` | alignment on the two perpendicular axes |
| `pack` | `start`, `center`, `end` | `start` | where the whole stack sits along `axis`: `start` keeps the first child at origin; `center` centres the stack; `end` puts the last child's far face at origin |

Each child keeps its own declared `pos`/`x`/`y`/`z` as an **additive**
offset inside its slot — `stack` computes the slot position, your `pos`
nudges within it.

### `grid`

N-dimensional replicator. Creates `count[0] × count[1] × count[2]` copies of
the body, each offset by `step[0..3] * [i, j, k]`:

```
grid "tiles" (count=[5, 1, 3], step=[0.6, 0, 0.6], center=1) {
  slab "tile" (size=[0.55, 0.05, 0.55])
}
```

Attributes:

| attribute | value | default |
|---|---|---|
| `count` | vec3, list, or scalar | `[1, 1, 1]` |
| `step` | vec3, list, or scalar | `[0, 0, 0]` |
| `center` | `0` / `1` | `0` — when `1`, the grid is centred on the wrapper origin |

A scalar `count`/`step` applies to X only (useful for 1D rows); a 2-element
list applies to X/Z (floor patterns). For 3D, pass a vec3.

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

## Solid groups: `solid`

`solid { … }` is a group-like container that defers CSG union to export time.
Its same-material, non-skinned leaf children are merged into a single mesh,
so overlapping or touching primitives of the same material read as one hollow
shape — interior faces where pieces meet get eliminated.

```
solid "shell" (mat="stone", cleanup="coplanar") {
  box "floor"   (pos=[0, 0.1, 0],   size=[6.2, 0.2, 4.2])
  box "north"   (pos=[0, 1.7, 2.0], size=[6.0, 3.0, 0.2])
  box "south"   (pos=[0, 1.7,-2.0], size=[6.0, 3.0, 0.2])
  box "east"    (pos=[ 3.0, 1.7, 0], size=[0.2, 3.0, 4.0])
  box "west"    (pos=[-3.0, 1.7, 0], size=[0.2, 3.0, 4.0])
}
```

- Children lower as normal scene nodes — you can still `attach` to them, put
  modules inside, author connectors, and so on. The merge is *export-time*,
  scoped to that subtree. The in-memory scene graph the editor sees keeps
  every child as a distinct, editable node.
- Only same-material leaf siblings merge together. Different-material
  children (`mat="glass"` next to `mat="stone"`) stay as separate nodes so
  textures and PBR factors are preserved.
- Skinned meshes, joint-referenced nodes, and groups are never merged; they
  pass through unchanged.

### `cleanup="coplanar"`

When set, the merged output gets one extra pass that drops triangle pairs
which share a plane and have opposite-facing normals. This catches the case
CSG union can't resolve on its own: two boxes that *touch* along a face
without overlapping — e.g. perpendicular walls meeting at a corner. Without
the cleanup, both sides of the seam survive; with it, they cancel.

Values: `"coplanar"` (enable) or `"none"` (default).

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

## Imports: `import`

Pull `module` declarations, `material` declarations, and the entire `scene { … }`
of another `.mog` file into the current file. Two use cases:

**Module libraries** — share parameterised modules across files:

```
import "shared/legs.mog"

scene {
  use "leg" (h=0.6)
}
```

**Scene composition** — assemble a scene out of object `.mog` files. Each
imported file's top-level `scene { … }` becomes an implicit module named
after the file stem, so `import "chair.mog"` lets you `use "chair" ()`:

```
import "objects/chair.mog"
import "objects/table.mog"

scene {
  group (pos=[ 1, 0, 0]) { use "chair" () }
  group (pos=[ 0, 0, 0]) { use "table" () }
  group (pos=[-1, 0, 0]) { use "chair" (), rot=[0, 180, 0] }
}
```

`import` is a top-level directive — declare it alongside `material` and
`module`, before or after them. It takes a quoted file path and an optional
`(as=<ident>)` to override the synthesised module name (handy when two files
share a stem, since stem collisions are a hard error).

Path resolution:

- **Relative paths** are joined onto the importing file's directory. So
  `import "shared/legs.mog"` from `/proj/scenes/chair.mog` reads
  `/proj/scenes/shared/legs.mog`.
- **Absolute paths** are used verbatim.
- Paths are canonicalised before deduplication, so `import "lib.mog"` and
  `import "./lib.mog"` resolve to the same file and load only once.

What gets lifted from an imported file:

- **`module` declarations** — added to the importer's module registry.
- **`material` declarations** (top-level or inside the imported `scene { … }`)
  — added to the importer's material registry. **Texture paths are rooted at
  the defining file's directory**: `material "wood" (base_color_texture =
  "textures/wood.png")` inside `objects/chair.mog` resolves to
  `objects/textures/wood.png` regardless of where the composing scene lives.
- **Top-level `scene { … }`** — synthesised as `module "<stem>" () { … }`.
  Use `(as=<ident>)` on the import to give it a different name.

Rules:

- Imports are transitive: an imported file can `import` another file, and
  every transitively-imported module / material / synthesised scene is
  visible to the original importer.
- Cycles (`A imports B imports A`) are detected and rejected with the
  full chain in the error message.
- Importing the same file twice — directly or via a chain — is a no-op.
- **Module name shadowing** follows precedence: **stdlib < imports < user
  declarations**. A user `module "leg" { … }` in the importing file
  overrides any `leg` pulled in by `import`; an imported `leg` overrides
  the stdlib's `leg`.
- **Synthesised scene-as-module collisions are a hard error.** If two
  imports both default to `chair` (one in `a/chair.mog`, one in
  `b/chair.mog`), rename one with `(as=chair_a)`.
- **Material name collisions across imports are a hard error.** Re-declare
  the material in the importing file to shadow it.
- An imported file may contain only `import`, `module`, `material`, and a
  single top-level `scene { … }`. Other top-level forms (joints, clips,
  skeletons) aren't composable yet and are rejected.

Failures surface as errors at `mogen check` / `mogen build` time, pointing
at the offending `import`.

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

## Lights: `light`

`mogen` exports lights via the standard glTF `KHR_lights_punctual` extension.
A `light` is a transform-only scene node — it carries `pos` / `rot` like any
other node, has no mesh, and never accepts children. Direction is implicit:
the light points along its local `-Z` axis.

```
light "sun"  (kind=directional, dir=[-0.4, -1, -0.3], color=[1, 0.95, 0.85], intensity=3)
light "lamp" (kind=point, pos=[0, 2, 0], color=[1, 0.9, 0.7], intensity=10, range=8)
light "spot" (kind=spot,  pos=[0, 3, 0], dir=[0, -1, 0], intensity=20,
              range=10, inner_cone=20, outer_cone=35)
```

| attribute | value | notes |
|---|---|---|
| `kind` | `directional`, `point`, `spot` | required |
| `color` | vec3 | linear-space RGB; default `[1, 1, 1]` |
| `intensity` | number | **candela** for point/spot, **lux** for directional; default `1.0` |
| `range` | number | distance cutoff for point/spot; rejected on directional |
| `dir` | vec3 | optional shortcut: rotates the node so `-Z` points along `dir` (overrides `rot=`) |
| `inner_cone` | number | spot only; degrees, default `0` |
| `outer_cone` | number | spot only; degrees, default `45` |

Lights ignore `mat`, `anchor`, `from`/`to`, and the relative-placement
shortcuts (`above`/`below`/…) — only transforms (`pos`, `rot`, `scale`,
`x`/`y`/`z`, `rx`/`ry`/`rz`), `role`, and `tags` apply.

`mogen` does not emit an ambient term: the glTF core spec has no ambient
light, and Godot derives ambient from a `WorldEnvironment` node downstream.
For low-intensity fill, use a dim directional light (e.g.
`intensity=0.5, dir=[0, -1, 0]`) or set up an environment in your engine.

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

Compile with `mogen build examples/<file>.mog -o out.glb` and open in any
glTF-2.0 viewer or game engine.

---

## Diagnostics and tooling

- `mogen check <file>.mog` validates without building. Pass `--json` for machine-readable diagnostics (the format the LLM repair loop consumes).
- `mogen dump-scene <file>.mog --json` prints the lowered graph for debugging.
- `mogen inspect <file>.glb` reads back a GLB and prints its top-level structure.

See [`ROADMAP.md`](./ROADMAP.md) §8 for the full diagnostic catalog.
