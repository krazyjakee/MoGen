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
- [Physics](#physics)
- [Decals](#decals)
- [Connectors](#connectors)
- [Attach: rigid alignment of two connector frames](#attach-rigid-alignment-of-two-connector-frames)
- [Conform: moulding a primitive onto a target surface](#conform-moulding-a-primitive-onto-a-target-surface)
- [Replicators: `mirror`, `array`, `stack`, `grid`](#replicators-mirror-array-stack-grid)
- [CSG: `union` / `difference` / `intersect`](#csg-union--difference--intersect)
- [Implicit-field blob: `blob`](#implicit-field-blob-blob)
- [Mesh subdivision: `subdivide`](#mesh-subdivision-subdivide)
- [Solid groups: `solid`](#solid-groups-solid)
- [Modules: `module` and `use`](#modules-module-and-use)
- [Imports: `import`](#imports-import)
- [Animation: `joint`, `clip`, templates](#animation-joint-clip-templates)
- [Skeletons and skinning: `skeleton`, `bone`, `skin=`, `bind=`](#skeletons-and-skinning-skeleton-bone-skin-bind)
- [Lights: `light`](#lights-light)

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

## File metadata: `meta`

An optional top-of-file block recording author-facing metadata about the
asset. Place at most once, before `material` / `scene` / `module`.

```
meta (
  name = "wooden_chair",
  version = "1.2.0",
  description = "A simple four-legged dining chair.",
  tags = ["furniture", "chair", "wood"],
)
```

| attr | type | source | notes |
|---|---|---|---|
| `name` | string | author | human-readable asset name |
| `version` | string | author | semver-style is conventional but not enforced |
| `mogen_version` | string | **toolchain** | auto-stamped on every save. Don't write it yourself. |
| `description` | string | author | one-line summary |
| `tags` | list of string | author | free-form labels |
| `style` | string | toolchain | visual-style hint from `mogen generate --style …`. Suggested values: `ps1`, `n64`, `low_poly`, `high_detail`, `arcade`, `voxel`, `cel_shaded`, `stylized_fantasy`, `cyberpunk`, `pixel_art`. Validator accepts any string; survives `modify`/`animate`/`repair` |
| `seed` | string | toolchain | random seed from `mogen generate`/`modify`; round-trips through edits |
| `thinking` | string | toolchain | per-file Gemini thinking budget stamped with `seed`; survives `modify` |
| `prompt` | string | toolchain | original NL prompt; round-trips through `modify` so the LLM can revise against original intent |
| `moghub_model_id` | string | toolchain | stamped by Studio's Publish after a successful MoGHub upload |
| `moghub_slug` | string | toolchain | MoGHub slug, paired with `moghub_model_id` |
| `moghub_version` | string | toolchain | last-published MoGHub version, paired with `moghub_model_id` |

The block is informational — not consumed by the geometry pipeline. It
survives lowering on `SceneGraph::meta` for downstream tooling. Old files
without `meta` keep building; the toolchain inserts a fresh
`meta (mogen_version = "...")` line on next save.

Diagnostics: `E0310` (no body block), `E0311` (no quoted name — use
`name=`), `E0312` (duplicate), `E0313` (must be top level), `W0107`
(`mogen_version` mismatch).

---

## Global settings

Top-level directives that tune the build itself.

| directive | value | effect |
|---|---|---|
| `lod_scale (value=N)` | number, default `1.0` | multiplies every primitive `segments`/`rings`/`samples` (implicit defaults *and* explicit values like `segments_u=64`) and `blob` `resolution`. `icosphere` `subdivisions` and any `subdivide=` post-pass step by `round(log2(N))` instead. Counts clamp to a per-primitive minimum |

```
lod_scale (value=0.5)

scene {
  sphere "head" (radius=0.5)         // 8 rings, 12 segments (default 16/24 halved)
  sphere "lod0" (radius=0.5, segments=48, rings=32)  // 16 rings, 24 segments (48/32 halved)
}
```

Studio's "LOD scale" slider edits this directive in place; it clears the
directive at `1.0` so saved files stay clean.

### Per-node `lod=` overrides

Any geometry/group node accepts `lod=N`, which **multiplies** the active
LOD scale for that node's subtree. RAII-scoped (doesn't leak into siblings)
and compounds with the global setting. Per-primitive minimums still apply
(`lod=0.1` won't collapse a cylinder below three sides).

```
scene {
  group "hero" (lod=2) {                  // boosted detail
    sphere "face" (radius=0.12)
  }
  sphere "background_rock" (radius=0.4, lod=0.5)  // halved detail
}
```

---

## Values and expressions

Every `value` on the right side of an attribute is one of:

| form | example | notes |
|---|---|---|
| number | `0.5`, `-90`, `1` | parsed as `f32` |
| length | `1.5m`, `90cm`, `18in`, `2ft` | a number with a unit suffix; normalised to metres (see [Length units](#length-units)) |
| vec3 | `[1.0, 0.5, 0.0]` | three expressions, comma-separated |
| list | `[0, 90]`, `[1, 2, 3, 4]` | arbitrary arity |
| string | `"wood"` | used for names and references |
| ident | `wood`, `y` | no quotes; used for axes and enum-like values |
| expression | `$height * 0.5`, `$r + 0.1`, `($a - $b) / 2` | arithmetic over `$param` refs |
| comparison | `$h > 0`, `$count == 4`, `$a != $b` | `<`, `<=`, `>`, `>=`, `==`, `!=` — used in `if` conditions and `for` ranges |

Inside `module` bodies, expressions reference parameters as `$name`.
Operators: `+ - * /` (conventional precedence + parentheses) and the six
comparisons. Evaluated at module-expansion time; the scene graph never
sees `$name`.

### Length units

mogen's base unit is the **metre** — the glTF/Godot convention — so a bare
number is a length in metres (`size=[0.45, 0.04, 0.45]` is a 45 cm seat).
To write real-world dimensions directly — handy for accurate blueprints —
append a unit suffix to any number:

| suffix | unit | metres |
|---|---|---|
| `mm` | millimetre | `0.001` |
| `cm` | centimetre | `0.01` |
| `m` | metre | `1.0` |
| `km` | kilometre | `1000.0` |
| `in` | inch | `0.0254` |
| `ft` | foot | `0.3048` |
| `yd` | yard | `0.9144` |

```
box "wall"  (size=[2m, 100cm, 8in], pos=[3ft, 0, 0])
box "joist" (height=5ft + 6in)        // units compose through arithmetic
```

The suffix is resolved to metres at **parse time**, so units flow through
every numeric position: scalars, `vec3` components, list items, and
`$param` arithmetic. Each component carries its own unit, so metric and
imperial can be mixed freely within one literal or expression.

Length units are for **lengths only** — sizes, positions, radii, heights,
offsets. Rotations stay in degrees, `scale` stays a unit-less multiplier, and
counts / colours stay plain numbers; don't put a length suffix on those.
Because everything normalises to metres, a unit literal and its metre
equivalent lower to byte-identical geometry.

### Weight units

Weights follow the same scheme, normalising to the **kilogram**. A bare number
is kilograms. Used by [`physics`](#physics) (`weight=`) and the per-node
`weight=` override.

| suffix | unit | metres → kg |
|---|---|---|
| `g` | gram | 0.001 |
| `kg` | kilogram | 1.0 |
| `t` | tonne | 1000 |
| `lb` | pound | 0.4536 |
| `oz` | ounce | 0.02835 |
| `st` | stone | 6.350 |

A `physics` block's `weight=` is a weight **per cubic metre** — append `/m3`
(or `/m³`): `700kg/m3`, `0.7t/m3`. A bare `weight=` on a geometry node is a flat
mass (`5kg`). Mass and length suffixes are disjoint, so a literal's dimension is
never ambiguous.

---

## Common attributes

These apply to every geometry node (`box`, `cylinder`, …) and to `group`:

| attribute | value | effect |
|---|---|---|
| `pos` | `vec3` | translation in the parent's frame; default `[0, 0, 0]` |
| `rot` | `vec3` (Euler XYZ in **degrees**) or a list | rotation applied after translation; default identity |
| `scale` | scalar or `vec3` | uniform/per-axis scale; default `1` |
| `mat` | string or ident | references a declared `material` by name |
| `phys` | string or ident | references a declared `physics` substance by name; drives auto-computed weight + centre of gravity (see [Physics](#physics)) |
| `weight` | mass (`5kg`) | optional flat per-node weight override (a mass), for props whose weight shouldn't follow from their volume |
| `role` | string or ident | semantic label; written into the GLB `extras` block |
| `tags` | comma-separated string | free-form labels; also in `extras` |

Transforms compose from child → parent along the scene hierarchy, exactly as
in glTF.

---

## Placement shortcuts

Every node accepts ergonomic shortcuts on top of `pos`/`rot`/`size`.
Mix and match freely — the DSL does the arithmetic.

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

The anchor shifts mesh vertices so the chosen point lands at the local
origin; default face connectors (`top`, `bottom`, …) move with it.

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

Resolution happens after the target's subtree is fully lowered (nested
geometry is in the AABB). Lookup is scoped to siblings in the same parent.

---

## Deformation modifiers

Every primitive accepts a small set of common modifier attrs that perturb the
generated mesh between primitive construction and anchor placement. The point
is variety without authoring extra geometry — bent beams, weathered rocks,
melted candles, jelly blobs — using one or two extra attrs.

| attribute | value | effect |
|---|---|---|
| `bend_x`, `bend_y`, `bend_z` | degrees | arc-length-preserving bend around the named axis. Length axis is the perpendicular one (Y for `bend_x`/`bend_z`, X for `bend_y`). |
| `twist_y` | degrees | helical twist around Y, from 0 at `y_min` to the full angle at `y_max`. |
| `taper` | ratio (1.0 = unchanged, 0.5 = half-width at top) | linear shrink along Y. |
| `droop` | amount (0..1 of length) | quadratic gravity-style sag along -Y; the base stays put, the top sinks by `amount * height`. |
| `noise` | 0..1 | coherent value-noise displacement along the vertex normal. Yields blobby "rock" texture. |
| `jitter` | 0..1 | per-vertex random displacement along the normal. Higher-frequency than `noise`, looks "jagged". |
| `faceted` | 0/1 | rebuild the mesh with three unique vertices per triangle and face-flat normals; reads as low-poly. |
| `seed` | integer | RNG seed for the stochastic modifiers (`noise`, `jitter`); same seed always reproduces the same shape. |
| `wave` | peak displacement (m) | sinusoidal displacement along the vertex normal — periodic ripples for water, jelly, ribbed metal, fabric. Pair with `wave_frequency`, `wave_axis`, `wave_phase`, `wave_range`. |
| `wave_frequency` | cycles/unit, default `1.0` | spatial frequency along `wave_axis`. `0.5` puts a crest every 2 m. |
| `wave_axis` | `"x"`/`"y"`/`"z"`, default `"x"` | axis the wave propagates along. |
| `wave_phase` | radians, default `0.0` | phase offset; lets sibling waves desync without animation. |
| `wave_range` | `[a, b]` | gates the wave to a normalised slice along `wave_axis` via smoothstep, matching `*_range` convention. |

```
icosphere "rock"   (radius=0.4, noise=0.30, jitter=0.15, faceted=1, seed=7)
cylinder  "candle" (radius=0.18, height=0.7, droop=0.4, noise=0.05)
box       "beam"   (size=[0.2, 0.2, 3], twist_y=20, taper=0.7)
```

Stochastic modifiers are deterministic for a given `seed`. Two unnamed
primitives with `noise=0.3` share the default seed (1) and produce
identical surfaces — set distinct seeds for sibling variety.

Each modifier accepts `*_range=[a, b]` (`bend_x_range`, `bend_y_range`,
`bend_z_range`, `twist_y_range`, `taper_range`, `droop_range`,
`noise_range`, `jitter_range`) that gates the deformation along its length
axis: vertices below `a` are unchanged, `[a, b]` ramps in via smoothstep,
above `b` is full effect. Endpoints auto-normalise (`[1.0, 0.5]` →
`[0.5, 1.0]`).

```
box "blade" (size=[0.06, 1.4, 0.01], bend_z=18, bend_z_range=[0.55, 1.0])
cylinder "spire" (radius=0.4, height=4.0, twist_y=70, twist_y_range=[0.6, 1.0])
```

Default tessellation auto-bumps (×2 segments / +1 icosphere subdivision)
when a smooth deformer is present; explicit `segments=`/`rings=`/
`subdivisions=` always override. Modifiers don't apply to `mesh` (loaded
glb) primitives. See `examples/nature/asteroid_field.mog`.

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
| `box` | `size=[x,y,z]` | `faces=[…]` — six entries in `+X,-X,+Y,-Y,+Z,-Z` order for [per-face materials](#per-face-box-materials-faces); each is a material name or a `face("mat", uv_scale=…, uv_offset=…, uv_swap=…)` struct with an [authored UV transform](#authored-per-face-uv-transforms-face) |
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
| `chamfered_box` | `size=[x,y,z]` | `radius` (bevel offset, 0.1) — sharp 45° bevels on all 12 edges + 8 corner triangles |
| `inset_box` | `size=[x,y,z]` | `face` (`"+y"`/`"-y"`/`"+x"`/`"-x"`/`"+z"`/`"-z"` or `"top"`/`"bottom"`/`"left"`/`"right"`/`"front"`/`"back"`, default `"+y"`), `amount` (inset, 0.1), `depth` (sink, 0.05) — five plain faces + one sunken panel |
| `wedge` | `size=[x,y,z]` | right-triangle prism — flat bottom on -Y, hypotenuse climbing toward +Y/+Z |
| `frustum` | `bottom=[w,d]`, `top=[w,d]`, `height` | truncated rectangular pyramid (defaults `bottom=[1,1]`, `top=[0.5,0.5]`, `height=1`) |
| `tube` | `outer`, `inner`, `height` | hollow cylinder (pipe / ring); `segments` (24) |
| `hemisphere` | `radius` | half-sphere, flat side on -Y; `rings` (8), `segments` (24) |
| `half_cylinder` | `radius`, `height` | D-profile half-cylinder, flat side facing -Z; `segments` (24) |
| `torus_arc` | `major`, `minor` | partial torus; `arc` (degrees, default 90) sweeps around +Y; `major_segments` (24), `minor_segments` (12) |
| `ellipsoid` | `size=[x,y,z]` | `rings` (16), `segments` (24); independent radii per axis |
| `superellipsoid` | `size=[x,y,z]` | `ew`, `ns` (1 = sphere, > 1 boxy, < 1 pinched), `rings` (16), `segments` (24) |
| `curved_plane` | `size=[x,z]` or `vec3` | `bend_u`, `bend_v` (degrees; arc angle along X/Z), `segments_u`/`segments_v` (12) |
| `lathe` | `profile=[[r,y], …]` | `segments` (24), `cap_ends` (1 = capped); profile authored bottom-to-top in `(radius, y)` pairs |
| `spline_tube` | `points=[[x,y,z], …]` | `radius` (scalar) or `radii=[…]` (per-point), `segments` (12), `samples` (8), `cap_ends` (1) |
| `spline_ribbon` | `points=[[x,y,z], …]` | `width` (scalar) or `widths=[…]` (per-point), `samples` (8), `twist` (degrees, default `0`); flat strip along a Catmull–Rom curve |
| `coil` | — | helical sweep. `radius` (0.5), `height` (1.0), `turns` (3), `profile_radius` (0.05), `segments` (12), `samples` per turn (16), `cap_ends` (1), `handedness` (`"right"`/`"left"`) |
| `heightfield` | — | XZ grid with fBm displacement. `size=[w,d]` (`[1, 1]`), `segments_u`/`segments_v` (32), `amplitude` (0.5), `octaves` (1..=8, default 3), `frequency` (1.0), `persistence` (0.5), `seed` (1). Layer `wave` on top for water |
| `bezier_patch` | `points=[…]` (exactly 16 control points, row-major u × v) | `segments_u`/`segments_v` (12). Bicubic Bézier surface — corners are `[0]`/`[3]`/`[12]`/`[15]`, inner four `[5]/[6]/[9]/[10]` shape the bulge |
| `metaball` | `points=[…]` (≥1) plus `radius=` or `radii=[…]` | `blend` (smooth-union radius, 0), `rings` (12), `segments` (16). Sugar over `union (smooth=k) { sphere … }` |
| `blob` | container with implicit-field children | true SDF + surface-nets mesh of any mix of `sphere` / `ellipsoid` / `box` / `rounded_box` / `capsule` / `cylinder` / `torus` children, smoothly blended. Each child may carry `op="subtract"` to carve a smooth cavity. See [Implicit-field blob](#implicit-field-blob-blob) |
| `extrude` | `points=[[x,z], …]` (closed CCW) | `hole=[[x,z], …]` (one CW inner contour), `height` (1.0), `taper` (1.0), `twist` (0), `caps` (1). Multi-hole: chain `extrude` + `difference` |
| `sweep` | `profile=[[x,y], …]` (closed CCW), `path=[[x,y,z], …]` | `samples` per segment (8), `twist` (uniform deg), `roll=[deg, …]`, `scale_along=[s, …]`, `caps` (1). Generalises `spline_tube`/`spline_ribbon` |
| `loft` | `points=[[x,z], …]`, `heights=[y, …]` | sections flat-packed in `points` (vertex counts must match); `samples` (4), `caps` (1) |
| `hull` | `points=[[x,y,z], …]` (≥4) | convex hull of the point cloud — the tightest closed manifold enclosing them. Interior points are ignored. Lossless sink for any convex polyhedron (sheared boxes, wedges, ramps) when no parametric primitive fits |
| `poly` | `points=[[x,y,z], …]`, `indices=[i0,i1,i2, …]` (triangles), `uvs=[[u,v], …]` (optional, one per point) | raw triangle mesh with **author-supplied per-vertex UVs** — the escape hatch for geometry whose texture mapping must be carried through verbatim (atlas sub-rectangles, engine-native faces a converter re-emits). Ignores the inherited UV mode; the `uvs=` list is the UV channel. Flat vs. smooth shading is decided by how you share vertices (own corners per face → flat; shared corners → smooth). One `mat=` per node |
| `leaf_card` | `size=[w,h]` | `cards` (2). Alpha-cutout foliage cluster — one quad + `cards-1` rotated copies sharing an XY plane. Pair with `alpha_mode="mask"` + `double_sided=1` |
| `mesh` | `src="path.glb"` | embed an external glTF binary as a single mesh. Path relative to the calling `.mog`. Source materials/skinning/animations are dropped — set them in the DSL |
| `branch` | — | procedural tree / vine / antler. See [Branch](#branch) below |
| `slab` | `size=[x,y,z]` | box alias; default anchor `bottom` (sits on ground) |
| `post` | `size=[x,y,z]` | box alias; default anchor `bottom` (pillar/leg) |
| `panel` | `size=[x,y,z]` | box alias; default anchor `back` (flat panel flush to a surface) |
| `wall` | `size=[x,y,z]` | `holes=[[x,y,w,h], …]` — rectangular cutouts through the Z axis |

`plane` is XZ-aligned, `quad` is XY-aligned (UI-style panels).

`curved_plane`, `lathe`, `spline_tube`, and `spline_ribbon` accept nested
list literals: `points=[[0, 0, 0], [1, 0.5, 0]]`. Inner lists must be
constant (no `$param`) — parameterise the whole node via a module wrapper.
`spline_tube`/`spline_ribbon` run Catmull–Rom with a parallel-transport
frame; `spline_ribbon` adds a `twist` that ramps roll around the path
tangent.

`slab`, `post`, and `panel` are `box` aliases that only change the default
anchor (`bottom`/`bottom`/`back`). Override with `anchor=` if needed.

`wall` is a box with rectangular cutouts along Z. Each hole is
`[cx, cy, w, h]` in the wall's local frame (X/Y are the face plane; Z is
cut through). Cutouts are CSG-differenced and welded at lowering, so a
single `wall` becomes one watertight mesh:

```
wall "barracks" (size=[3, 3, 0.1], holes=[
  [-0.75, -0.4, 0.9, 2.0],   // door
  [ 0.9,  0.3, 0.8, 0.8],    // window
])
```

### Per-face box materials (`faces=`)

A `box` can carry a different material on each face via `faces=[…]` — exactly
six material names in the fixed order **`+X, -X, +Y, -Y, +Z, -Z`**:

```
material "wood"  (color=[0.6, 0.4, 0.2])
material "metal" (color=[0.7, 0.7, 0.75], metallic=0.9)

box "crate" (size=[2, 1, 1], faces=[
  "wood", "wood",   // +X, -X
  "metal",          // +Y (lid)
  "wood", "wood", "wood"  // -Y, +Z, -Z
])
```

The box lowers to an editable wrapper (keeping its transform, anchor, and
connectors) plus one **frozen** child per distinct material — each a quad group
of just that material's faces. Faces sharing a material collapse into one child,
so the crate above becomes two children (`wood` ×5, `metal` ×1), not six. The
children have no AST of their own, so the Studio inspector redirects clicks to
the wrapper.

An empty entry `""` falls back to the box's own `mat=` (or inherited material),
so you can override just the faces you care about:

```
box "panel" (mat="body", faces=["", "", "label", "", "", ""])  // only +Y differs
```

This is the migration sink for engine map blocks whose six faces each reference
a different tile texture.

#### Authored per-face UV transforms (`face(...)`)

By default each face's texture coordinates follow the material's UV mode
(`fit` = one copy per face, `tile` = 1 unit per metre). When a converter carries
its own per-face UV mapping (atlas sub-rectangles, engine-native tile offsets),
any entry in the `faces=` list may be a `face(...)` struct instead of a bare
string. It sets the material **and** an explicit UV transform baked verbatim
onto that face, bypassing the procedural projection:

```
box "wall_seg" (size=[4, 3, 1], faces=[
  "wall", "wall", "wall", "wall",
  face("panel", uv_scale=[0.25, 0.333], uv_offset=[0, 0], uv_swap=false),  // +Z
  "wall"
])
```

- **First argument** — the material name (same rules as the bare string; `""`
  falls back to the box's own material).
- `uv_scale=[sx, sy]` — per-axis scale; may be **negative** to mirror. Default `[1, 1]`.
- `uv_offset=[ox, oy]` — per-axis constant added after scaling. Default `[0, 0]`.
- `uv_swap=<bool>` — swap the two in-plane axes before scale/offset. Default `false`.

All UV fields are optional; **`face("mat")` is identical to the bare `"mat"`**
(no authored transform → keep the default Fit/Tile UVs). Authored and bare
entries may be freely mixed in one list, and faces still group into frozen
children by material as above.

The baked coordinate is face-local. For a face whose outward normal is on axis
*A*, the two in-plane axes are the other two in ascending index order
(`X→(Y,Z)`, `Y→(X,Z)`, `Z→(X,Y)`); the local coordinate on each is the vertex
offset from the box's minimum corner on that axis (`vertex + size/2`). The final
UV is `offset + scale * (swap ? swapped : local)`. Values are emitted **exactly
as computed** — never clamped or wrapped, so repeat/tile behaviour is left to the
sampler.

### Branch

`branch` is a procedural tree builder. One node expands into a recursive
cluster of `spline_tube` segments tapering from trunk to twigs, with
optional `leaf_card` clusters at the tips. Inner segments are
non-editable (deterministic function of `seed=`).

| attribute | default | effect |
|---|---|---|
| `form` | `"decurrent"` | growth habit preset — see below. Sets sensible defaults for all attrs in this table; user attrs still win |
| `length` | `1.0` | trunk length (m) |
| `radius` | `0.05` | trunk base radius (m) |
| `depth` | `4` | recursion depth (number of branching levels) |
| `splits` | `2` | child branches per parent at each split |
| `length_falloff` | `0.7` | per-level length multiplier |
| `radius_falloff` | `0.6` | per-level radius multiplier |
| `branch_angle` | `35` | angle (degrees) child branches lean off the parent tangent |
| `roll` | `137.5` | roll (degrees) between successive children — the golden angle by default, breaks bilateral symmetry |
| `tropism` | `0.0` | bias toward +Y per segment (positive = upright trees, negative = drooping) |
| `bend` | `10` | random bend (degrees) added to each segment frame |
| `leader_bias` | `0.0` | `0.0`–`1.0` strength of central-leader behaviour. At `1.0` child 0 of every fork continues straight up at full length/radius (pine-like silhouette); at `0.0` all forks are equal (default broadleaf habit) |
| `multi_stem` | `1` | number of trunks emerging from the base. Only honoured by `form="shrub"`; ignored otherwise |
| `segments` | `8` | radial segments per spline_tube |
| `samples` | `4` | samples per spline segment |
| `seed` | `1` | RNG seed; same seed = identical tree |
| `jitter` | `0.2` | `0.0`–`1.0` random perturbation amount on lengths/angles |
| `leaves` | `1` | emit `leaf_card` clusters at terminal tips (`0` to disable) |
| `leaf_size` | `0.35` | leaf-card height (m) |
| `leaf_aspect` | `1.0` | leaf width / height ratio. `<1` for needle/willow leaves, `>1` for wide flat leaves |
| `leaf_cards` | `2` | crossed cards per leaf cluster (or fronds in the palm rosette) |
| `leaf_mat` | — | material name for leaves; defaults to inherited `mat=` |

`form` values:

| value | habit | distinctive defaults |
|---|---|---|
| `"decurrent"` | broadleaf tree (default — oak, maple) | equal forks, square leaves |
| `"excurrent"` | conifer (pine, spruce) | strong central leader, near-horizontal side branches, narrow needle leaves |
| `"weeping"` | willow | long branches with strong negative tropism (drooping), long narrow leaves |
| `"shrub"` | bush | several short trunks at the base (`multi_stem=4` by default), no leader |
| `"palm"` | palm | single straight trunk with a fan of frond-shaped cards at the tip; no recursive branching |

Wrap a `branch` in a `group` with `scale=`/`rot=` for forests, antlers,
vines, root systems. Pair with stdlib `branch`/`leaf` for hand-tuned shapes.

### Building

`building` is a procedural building-interior generator. One node expands
into floor/ceiling slabs, perimeter walls with windows and entrance
cutouts, interior walls with door cutouts, and stamped door/window/skylight
modules. The generated subtree is stamped non-editable (pure function of
`seed=` plus the declared attrs). See `docs/building.md` for the full
spec, supported styles/roof shapes, and tranche status.

| attribute | default | effect |
|---|---|---|
| `seed` | `1` | RNG seed; same seed = identical layout + geometry. |
| `style` | `"grid"` | Layout algorithm. T1-T2: `grid`, `apartment-block`. |
| `mat_style` | `""` | Free-text style hint forwarded to material/texture generation. |
| `floor_area` | `120` | Target floorplate area in m² (per floor). |
| `rooms` | `4` | Total rooms across all floors; distributed proportionally. |
| `floors_above` | `1` | Storeys above ground (incl. ground floor). |
| `floors_below` | `0` | Basement storeys. |
| `windows` | `0` | Total above-ground window count; distributed across above-ground storeys. Basements get none. |
| `skylights` | `0` | Top-storey ceiling cutouts. |
| `roof` | `"flat"` | Roof shape: `flat`, `pitched`, `gabled`, `hipped`, `mansard`, `shed`. |
| `roof_overhang` | _unset_ | Eaves projection past the walls (m) for `gabled`/`pitched`. Unset → `gabled` flush, `pitched` modest default. |
| `ceiling_height` | `2.6` | Clear height per storey (m). |
| `door_w`, `door_h` | `0.9, 2.1` | Door opening dimensions (m). |
| `window_w`, `window_h` | `1.2, 1.4` | Window opening dimensions (m, medium class). Small = ×0.6, large = ×1.4. |
| `wall_thickness` | `0.12` | Exterior / interior wall thickness (m). |
| `ceiling_thickness` | `0.2` | Slab thickness (m). |
| `entrances` | `1` | Ground-floor external door count. |
| `external_door` | `"door_simple"` | Stdlib / user module ref. Stamped at each entrance. |
| `internal_door` | `"door_simple"` | Stamped at each interior opening. |
| `window_small`, `window_medium`, `window_large` | `"window_simple"` | Per-class window module refs. |
| `skylight` | `"skylight_simple"` | Skylight module ref; carves the top ceiling slab. |
| `elevators` | `0` | Vertical shafts on the east side, stamped with `elevator_shaft_simple`. |
| `staircases` | `0` | One straight flight per storey transition, stamped with `stair_simple`. `W1113` warns if missing on multi-storey. |

`building` accepts only `room_type` and `adjacency` children.

```
room_type "<name>" (kind=public|private|service|utility|secure|staff_only,
                    density=0..10, mat="<material>",
                    min_area=…, max_area=…)
adjacency "<room_type>" (adjacent_to=["a", "b"], away_from=["c"])
```

`kind` is required on `room_type`; `density` weights sampling (0 disables
the type); `mat` overrides per-room floor/wall material. Adjacency rules
are **soft** — the solver runs 10 sub-seeded layouts and keeps the
highest-scoring (`+1`/m of shared wall for `adjacent_to`, `-1`/m for
`away_from`). Referencing an undeclared room-type is a validation error.

```
building "house" (seed=4, style="apartment-block",
                  floor_area=85, rooms=6, windows=6, mat="wood") {
  room_type "bedroom"  (kind=private, density=4)
  room_type "kitchen"  (kind=service, density=1, mat="tile")
  room_type "living"   (kind=public,  density=2)
  adjacency "kitchen"  (adjacent_to=["living"])
}
```

Beyond geometry, `building` embeds **points of interest** as empty marker
nodes (`role`/`tags` in glTF extras) so an engine can place its own content:
`entrance` / `door` / `window` opening slots, per-room furniture markers when
`furnish=1` (`bed`, `desk`, …), and per-floor **vertical-circulation** anchors
— `stair_access` (one per storey, at each floor's stair entry platform) and
`elevator_stop` (one per served storey, anchored at the shaft centre on the
floor surface where the cab stops). Each circulation marker faces its shaft
door and carries a `floor=<n>` tag, so you can drop your own stairwell /
elevator models knowing exactly where each floor connects. `debug_show_poi=1`
gives every marker a small bright `building_poi_<role>` sphere so the
otherwise geometry-free anchors show in a preview.

### Cave

`cave` is a procedural cave generator. One node expands into a single
watertight rock shell — a bounding block hollowed by a smooth-blended field of
chamber and passage carvers (surface-nets meshed) — plus optional scattered
features. Chambers are organised into `levels` stacked horizontal layers
(separated by `level_gap` rock, joined by `level_links` vertical ramps); within
a layer they mix `spacing`-separated rooms with `overlap`-merged caverns.
Passages steeper than `max_slope` are rebuilt as switchback ramps so no walkable
surface exceeds the angle cap; and
`entrances` punch horizontal mouths out through the nearest side face so the
cave is enterable. The generated subtree is stamped non-editable (pure function
of `seed=` plus the declared attrs). See `docs/caves.md` for the full spec.

| attribute | default | effect |
|---|---|---|
| `seed` | `1` | RNG seed; same seed = identical cave. |
| `size` | `[24, 10, 24]` | Outer rock-block dimensions `[width, height, depth]` (m); base sits on `y=0`. |
| `chambers` | `6` | Number of chambers carved. |
| `levels` | `2` | Stacked horizontal layers (floors). Each is its own chamber+passage network. |
| `level_gap` | `1.5` | Solid rock kept between layers (m). |
| `level_links` | `1` | Vertical ramps carved between each adjacent layer pair (clamped ≥ 1 when `levels > 1`). |
| `chamber_min`, `chamber_max` | `2.5, 5.0` | Chamber radius range (m). Auto-capped to fit a layer's height. |
| `spacing` | `2.0` | Minimum rock gap between same-layer chambers (m); keeps rooms distinct. |
| `overlap` | `0.35` | Probability a chamber merges with a same-layer neighbour into a larger cavern. `0` = all separate, `1` = all clustered. |
| `chamber_flatten` | `0.6` | Vertical squash (height = radius × this); < 1 keeps floors gentle. |
| `passage_radius` | `1.1` | Tunnel radius (m). |
| `loops` | `1` | Extra connections beyond the spanning tree, for looping layouts. |
| `max_slope` | `45` | Maximum walkable slope (degrees); steeper passages switchback. |
| `roughness` | `0.35` | Wall-noise amount `[0, 1]` for a natural stone finish. |
| `blend` | `1.5` | Smooth-union radius blending chambers + passages. |
| `margin` | `2.0` | Rock thickness kept around the void on every face (m). |
| `resolution` | `96` | Voxel-grid resolution (32–224). Higher = finer + slower. |
| `lod_scale` | `1.0` | Mesh-quality scale `[0.1, 1.0]`; scales rock voxel grid + decoration tessellation. Lower = fewer triangles. Layout / counts / POIs unaffected. |
| `entrances` | `1` | Horizontal mouths punched to a side face. A scalar `N` opens `N` on the surface (top) band; an array `[b0, b1, …]` opens them per band from the bottom up (index `0` = lowest), so e.g. `entrances=[1, 0, 1]` connects a chosen storey to an adjacent dungeon. Bands past the array end get none; mouths prefer chambers nearest a wall. |
| `mat` | `cave_rock` | Rock material; defaults to an auto-stamped grey stone. |
| `water_mat` | `cave_water` | Material for pools / lakes. |
| `rock_piles`, `pools`, `lakes`, `stalagmites`, `stalactites`, `columns` | `0` | Decoration counts scattered on chamber floors / ceilings (`columns` span floor→ceiling). |
| `mushrooms` | `0` | Mushroom-spot POI markers on chamber floors (empty nodes, no geometry). |
| `debug_hide_shell` | `0` | Debug: slice the front (+Z) half of the rock away to see the chambers in cross-section (like `building`'s `debug_hide_roof`). |
| `debug_show_poi` | `0` | Debug: give every point-of-interest marker a small bright per-kind `cave_poi_<kind>` sphere (colour-coded by group) so the empty markers are visible in a preview. |

Beyond geometry, `cave` embeds **points of interest** as empty marker nodes
(`role`/`tags` in glTF extras) under a `points_of_interest` group:
`dead_end_chamber` (single-connection rooms), `column_base`, `ladder_anchor`
(passages steeper than `max_slope`) and `mushroom_spot`. See `docs/caves.md`.

Decorations are placed by querying the **real carved floor / ceiling** under
each feature, so drips and boulders sit flush on the surface rather than the
geometric chamber bound. `cave` accepts only `feature` children, which tune one
decoration kind:

```
feature "<name>" (kind=stalagmite|stalactite|column|rock_pile|pool|lake,
                  count=…, min_size=…, max_size=…, mat="<material>")
```

`kind` is required. `count` overrides the matching top-level count knob;
`min_size`/`max_size` set the size range (m); `mat` overrides the material.

```
cave "hollow" (seed=12, size=[26, 12, 26], chambers=7, levels=2,
               max_slope=45, entrances=2, mat="limestone",
               stalagmites=14, stalactites=10, pools=2) {
  feature "spires" (kind=stalagmite, min_size=0.3, max_size=1.1)
}
```

Every primitive is authored in its local frame and positioned via
`pos`/`rot`/`scale`. Defaults make bare `cylinder "leg"` a 1 m unit-radius
cylinder at the origin.

### Terrain

`terrain` is a procedural heightfield generator. One node expands into a grid of
independently cullable **chunk meshes** sampled from a coherent noise field, then
refined by an ordered chain of retouch passes (smoothing → terracing). The field
is normalised `0..1` and scaled by the height component of `size`; adjacent
chunks share boundary grid lines so seams are crack-free. `sea_level` does not
reshape the land — the terrain keeps its real shape underwater and the sea is a
flat plane laid on top. The generated subtree is stamped non-editable (a pure
function of `seed=` plus the declared attrs).

| attribute | default | effect |
|---|---|---|
| `seed` | `1` | RNG seed; same seed = identical terrain. |
| `size` | `[40, 6, 40]` | Patch dimensions `[width, max_height, depth]` (m). `max_height` scales the normalised field; the patch is centred on the origin. |
| `source` | `fbm` | Noise shape: `fbm` (rolling hills), `ridged` (sharp mountain ridges), `billow` (rounded mounds), `island` (central landmass falling off to water at the edges — pair with `sea_level`), `voronoi` (cellular crater rims and cracked basins). |
| `octaves` | `4` | Noise octaves summed; more = finer detail. |
| `frequency` | `0.06` | Base noise frequency; higher = more, smaller features. |
| `persistence` | `0.5` | Per-octave amplitude falloff `[0, 1]`; higher = rougher. |
| `resolution` | `128` | Grid divisions across the patch (4–1024); rounded up to a multiple of `chunks`. |
| `chunks` | `4` | Splits the patch into a `chunks × chunks` grid of separately cullable meshes (1–16). |
| `lod_levels` | `3` | Baked render-LOD meshes per chunk (1–4). Level 0 is full detail; each higher level halves the sampling stride and takes over at a greater camera distance. `1` = full detail only, no distance swapping. |
| `smooth` | `0` | Box-blur passes (0–32) applied to soften the raw noise. |
| `terrace` | `0` | Quantise heights into this many stepped bands (0 = off, 0–64). |
| `sea_level` | `0.0` | Normalised waterline height `[0, 1]`; adds a flat water plane at this height and enables shoreline POIs. Land keeps its real shape underwater (basins are not flattened). |
| `peaks` | `0` | Number of summit POI markers placed on local maxima. |
| `flat_spots` | `0` | Number of buildable-flat-area POI markers (flattest neighbourhoods above sea level). |
| `shore_points` | `0` | Number of shoreline POI markers at the water's edge (requires `sea_level > 0`). |
| `colliders` | `none` | `all` adds a trimesh collider to every chunk; `none` is visual-only. |
| `lod_scale` | `1.0` | Mesh-quality scale `[0.1, 1.0]`; scales effective resolution. Lower = fewer triangles. POIs / sea level unaffected. |
| `mat` | `terrain_ground` | Ground material; defaults to an auto-stamped **white** PBR finish (the surface colour comes from the per-vertex bake below, which multiplies the base colour). A custom `mat=` tints the whole bake. |
| `debug_show_poi` | `0` | Debug: give every POI marker a small bright per-kind `terrain_poi_<kind>` sphere so the empty markers are visible in a preview. |

Every chunk vertex carries a **baked surface colour** in `COLOR_0` (glTF
per-vertex colour, which multiplies the white ground material): a grass→rock
blend driven by slope (steep faces turn to bare rock) plus, when `sea_level > 0`,
sand hugging the waterline that fades to dark mud on the submerged floor. The
look therefore travels as plain glTF — no engine shader is required — and an
engine is still free to layer a runtime slope/height shader on top.

Beyond geometry, `terrain` embeds **points of interest** as empty marker nodes
(`role`/`tags` in glTF extras) under a `points_of_interest` group: `peak`
(summit local maxima), `flat_spot` (buildable flat areas above sea level), and
`shoreline` (cells at the water's edge). Markers spread out so no two land on the
same feature.

#### Carving children: `hole` and `road`

A `terrain` body accepts only `hole` and `road` declarations (any other child is
an error — geometry parented here would be dropped). Both carve the same shared
height field, so chunk seams, skirts and POIs stay consistent with the result.

**`hole`** drops the surface inside a footprint and walls the rim, punching a real
void into the baked mesh that a basement or cave mouth can slot into.

| attribute | default | effect |
|---|---|---|
| `at` | `[0, 0]` | Footprint centre `[x, z]` in patch-local metres. |
| `radius` | — | Circular footprint radius (m). Give **either** `radius` **or** `size`, not both. |
| `size` | — | Rectangular footprint `[w, d]` (m). |
| `depth` | `4` | How far below the rim the floor sits (m). |
| `cap` | `open` | `open` = vertical rim walls drop to the floor depth, bottom left open (plug it with a basement/cave). `floor` = walls plus a flat floor quad = a fully sealed, watertight pit. |

**`road`** flattens a corridor along a polyline to a smoothed longitudinal
profile (staying at the terrain's own height — no trench), and tints the corridor
toward a dirt/gravel colour baked into `COLOR_0`.

| attribute | default | effect |
|---|---|---|
| `path` | — | Required centreline `[[x, z], [x, z], …]` (≥ 2 waypoints), patch-local metres. |
| `width` | `3` | Flattened corridor width (m). |
| `shoulder` | `= width/2` | Extra blend distance (m) outside the flat corridor where the road eases back into the surrounding terrain. |

```
terrain "valley" (seed=2, size=[200, 12, 200], source="island", chunks=6) {
  hole ( at=[-30, -20], radius=14, depth=6, cap="floor" )  // sealed pit
  hole ( at=[40, 30], radius=10, depth=8, cap="open" )     // open shaft
  road ( path=[[-90, -60], [-20, -10], [30, 20], [90, 70]], width=8, shoulder=5 )
}
```

Each chunk emits `lod_levels` overlapping mesh variants named
`chunk_<i>_<j>_lod<n>`, every variant tagged with a camera-distance band in glTF
`extras.lod` (`{ level, min_distance, max_distance }`). The bands partition
`[0, ∞)`, so exactly one variant is the active one at any distance: MoGen Studio's
viewer shows it (and frustum-culls per chunk), and an engine importer can wire the
same ranges into Godot `VisibilityRange`. Only the finest variant (level 0) carries
the trimesh collider and casts shadows; coarser variants are visual stand-ins, ringed
by a downward skirt so a neighbouring chunk at a different LOD can't open a crack in
the surface.

```
terrain "valley" (seed=7, size=[80, 14, 80], source="fbm",
                  octaves=5, frequency=0.04, resolution=192, chunks=6,
                  lod_levels=3, smooth=2, sea_level=0.28,
                  peaks=8, flat_spots=12, shore_points=16,
                  colliders="all", mat="grassland")
```

---

### Dungeon

`dungeon` is a procedural layout generator for dungeon-crawler levels. Where
`cave` carves an organic void from a smooth field, a dungeon is a **tile grid**:
axis-aligned rectangular rooms placed on a `cell`-metre lattice, joined by
straight/L-shaped corridors, stacked into `levels` floors that real staircase
geometry connects. Walls, floors and ceilings are clean **box meshes** and
doorways are gaps in the walls — the whole layout is watertight by construction.
The node takes no body block; the generated subtree is stamped non-editable (a
pure function of `seed=` plus the declared attrs).

| attribute | default | effect |
|---|---|---|
| `seed` | `1` | RNG seed; same seed = identical layout. `0` is coerced to `1`. |
| `size` | `[48, 4, 48]` | Footprint + clearance `[width(x), room_height(y), depth(z)]` (m). `width`/`depth` set the grid footprint (÷ `cell`); `room_height` is one level's floor-to-ceiling clearance. |
| `cell` | `4` | Grid cell edge length (m). Rooms and corridors quantise to this lattice so walls always meet flush. |
| `levels` | `1` | Stacked floors. `1` is single-storey; higher values stack independent room layouts linked by staircases. |
| `stairs` | `1` | Staircases between each pair of adjacent levels. |
| `entrances` | `1` | Exterior doorways through the perimeter wall. A scalar `N` puts `N` on the ground floor; an array `[g, f1, f2, …]` sets each floor independently (index `0` = ground), so e.g. `entrances=[1, 0, 2]` opens a chosen storey onto an adjacent cave mouth or ledge. Floors past the array end get none; doors prefer dead-end rooms and the shortest border stub. |
| `rooms` | `6` | Target room count per level (placement is best-effort within the grid). |
| `room_min` | `2` | Room side length range in cells (inclusive). `room_min > room_max` is swapped. |
| `room_max` | `5` | — |
| `spacing` | `1` | Minimum rock gap kept between rooms of the same level, in cells. |
| `corridor_width` | `1` | Corridor width in cells. |
| `loops` | `1` | Extra corridor connections beyond the spanning tree, per level (creates loops). |
| `wall_thickness` | `0.4` | Wall thickness (m). |
| `floor_thickness` | `0.4` | Floor / ceiling deck thickness (m). |
| `ceilings` | `1` | Emit ceiling decks (each deck doubles as the next level's floor). `0` = open-topped levels. |
| `prop_spots` | `0` | Number of `prop_spot` POI markers scattered on room floors. |
| `colliders` | `all` | `all` adds a trimesh collider to every solid surface (decks, walls, steps); `none` is visual-only. |
| `lod_scale` | `1.0` | Mesh-quality scale `[0.1, 1.0]`; compounds with the file-global `lod_scale`. Layout, counts and POIs are unaffected. |
| `debug_hide_roof` | `0` | Debug: omit the topmost deck (roof) so rooms are visible from above in a preview. |
| `debug_render_floor` | _(unset)_ | Debug: render only this level index (`0` = ground), with no ceiling and only the staircases that touch it, to peek inside one floor. Out-of-range values render every level. |
| `debug_show_poi` | `0` | Debug: give every POI marker a small bright per-kind `dungeon_poi_<kind>` sphere so the empty markers show in a preview. |

Geometry is built from **decks**: deck `k` is simultaneously the floor of level
`k` and the ceiling of level `k-1`, so there are no duplicate slabs. Every
walkable cell — including staircase landings — is covered, so the top level is
fully roofed and stairwells are enclosed. Staircases punch an opening through
the upper deck and fill it with a flight of small (~0.18 m riser) steps sized
for in-game use. Each **exterior doorway** is a one-cell floor stub run out to
the grid border with the perimeter wall there left open, so the dungeon has a
clear way in; `entrances` controls how many per floor (a scalar for the ground
floor, or a per-floor array) so a raised storey can open onto an adjacent cave
mouth or ledge. Walls are extended by `wall_thickness`
at their ends so corners always close — no holes. A single material pair
(`dungeon_floor`, `dungeon_stone`) is auto-stamped if you don't supply `mat=`.

Beyond geometry, `dungeon` embeds **points of interest** as empty marker nodes
(`role`/`tags` in glTF extras) under a `points_of_interest` group: `entrance`
(the exterior doorway, oriented facing out), `spawn` (the room the entrance
leads into), `treasure_room` (other dead-end rooms), `stair_top` /
`stair_bottom` (staircase landings), and `prop_spot` (scattered room-floor
slots). An engine reads these to place the player, loot, transitions and props.

```
dungeon "keep" (seed=7, size=[56, 4, 56], cell=4,
                levels=3, stairs=1,
                rooms=7, room_min=2, room_max=5, spacing=1,
                corridor_width=1, loops=1,
                prop_spots=8, colliders="all")
```

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
- `normal_strength` — slope multiplier baked into the *derived* normal
  map by `mogen textures`. `~0..8`, default `1.5`. No effect when
  `normal_texture` is authored directly.
- `occlusion_strength` — `0.0`–`1.0` ceiling on the *derived* AO darkness
  (`0` flat white, `1` cavities reach black). Default `0.7`. No effect
  when `occlusion_texture` is authored directly.
- `alpha_mode` — `"opaque"` (default), `"blend"` (translucent), or `"mask"`
  (1-bit cutout, e.g. foliage).
- `alpha_cutoff` — threshold for `alpha_mode="mask"`, default `0.5`.
- `emissive` — vec3 glow added on top of PBR. Screens, embers, lava.
  Default `[0, 0, 0]`.
- `emissive_strength` — HDR multiplier on `emissive`
  (`KHR_materials_emissive_strength`); `> 1.0` drives bloom. Default `1.0`.
- `transmission` — `0.0`–`1.0` light through the surface
  (`KHR_materials_transmission`); `1` is clear glass. Orthogonal to
  `alpha_mode` — glass/water use this, gels/tints/smoke use `alpha`.
- `double_sided` — `0` (default) or `1`. When `1`, draws both triangle
  faces (glTF `doubleSided`). Use for leaves, fins, flags, cloth, and thin
  `curved_plane`/`plane`/`disc`/`quad` whose underside is visible.
- `uv_mode` — `"tile"` (default) or `"fit"`. `"tile"` emits world-space
  UVs (1 unit = 1 tile, scaled by `uv_scale`) — repeating surfaces like
  walls, planks, fabric. `"fit"` uses per-face `[0, 1]²` UVs so each face
  shows the full image once — image-as-texture (signs, paintings, decals).
- `uv_scale` — scalar or vec2; default `1.0`. In `tile` mode: tiles per
  world unit (`2` = denser, `0.5` = bigger). In `fit` mode: multiplies
  `[0, 1]` coords. Vec2 stretches asymmetrically.
- `base_color_texture` — string path to an `.png`/`.jpg` file on disk,
  resolved relative to the `.mog` file. Multiplied against `color`. sRGB.
- `metallic_roughness_texture` — packed metal/rough map (glTF convention:
  green = roughness, blue = metallic). Linear.
- `normal_texture` — tangent-space normal map. Linear.
- `occlusion_texture` — ambient occlusion (red channel). Linear.
- `emissive_texture` — emissive colour map, multiplied against `emissive`.
  sRGB.
- `prompt` — free-form subject hint for `mogen textures` when generating
  albedo (e.g. `prompt="navy nylon ripstop weave"`). Steers Gemini away
  from the default "material name + colour" framing; auto-rephrased on
  recitation rejection.
- `shader` — `"standard"`/`"pbr"` (default, the regular PBR path), the
  built-in `"water"` preset, or the name of a user-declared `shader "<name>"`
  (see [Shaders](#shaders) below). Studio-preview only — the exported `.glb`
  always uses standard PBR scalars; the shader name + `shader_params` ride
  along as `node.extras.shader` metadata for downstream consumers. `"water"`
  reinterprets the standard knobs as a procedural water shader: `color` is
  the body tint, `uv_scale` is ripple density, `roughness` controls
  chop/foam (`0.05` glassy → `1.0` whitecaps), `metallic` lerps toward
  liquid-metal Fresnel, `transmission` + `alpha_mode="blend"` lets the floor
  show through, and `normal_texture`/`base_color_texture` blend into the
  procedural waves.
- `gradient` — optional ramp baked into per-vertex `COLOR_0` at export
  time. Four surface forms, all colours are vec3 in sRGB `[0..1]`:
  - `linear(from=[…], to=[…], axis=x|y|z)` — interpolates between two
    colours along the named local axis (default `y`).
  - `vertical(from=[…], to=[…])` — sugar for `linear(axis=y)`. No `axis=`.
  - `radial(center=[…], edge=[…])` — centre-to-corner falloff in the
    mesh's local AABB. No `axis=`.
  - `stops(colors=[[…], …], positions=[…], axis=…, kind=linear|radial)` —
    multi-stop ramp. `positions` defaults to even spacing across `[0, 1]`
    if omitted; `kind` defaults to `linear`.

  `COLOR_0` multiplies `baseColorFactor` per the glTF spec, so pair the
  gradient with `color=[1, 1, 1]` to get the ramp unaltered, or with a
  tinted `color=` to tint the whole ramp. Vertex colours need vertices to
  interpolate between — bump primitive `segments`/`rings`/`segments_u/v`
  for smoother ramps; a default 6-vert box reads as a hard step, not a
  gradient.

Example:

```
material "oak" (
  color=[1, 1, 1],
  roughness=0.8,
  base_color_texture="textures/oak_albedo.png",
  normal_texture="textures/oak_normal.png"
)

material "lake" (color=[0.05, 0.32, 0.45], shader="water")
```

Texture files are embedded in the output GLB (self-contained, movable
without sources); missing files are a hard error at export. Reference a
material via `mat="wood"`; unknown names are a hard error at lowering.

---

## Shaders

A `shader "<name>" { ... }` declaration names an on-disk GLSL fragment
snippet and the parameters materials may feed it. mogen never compiles or
runs the GLSL — it only carries the path + resolved parameter values as
`node.extras.shader` metadata on the glTF nodes bound to a referencing
material. MoGen Studio is the one component that actually compiles a
declared shader, for live preview. The built-in `water` preset (see
`shader=` above) is just the first client of this system — declaring your
own `shader "water" { ... }` shadows it.

```
shader "ripple" (source="shaders/ripple.glsl") {
  param "speed" (type=float, default=2.0)
  param "tint"  (type=color, default=[0.0, 0.3, 0.5])
}

material "pond" (color=[0.1, 0.2, 0.3], shader="ripple") {
  shader_params (speed=3.5, tint=[0.0, 0.4, 0.6])
}
```

`shader` attributes:

- `source` — path to the GLSL fragment snippet, resolved relative to the
  `.mog` file the same way texture paths are.

Each `param "<name>" { ... }` child declares one uniform the snippet may
use:

- `type` — `float`, `vec2`, `vec3`, `vec4`, or `color` (an RGB `vec3`
  authored as `[r, g, b]`).
- `default` — value used when a referencing material supplies no override
  via `shader_params`. Must match the declared `type`.

A material opts in with `shader="<name>"` (built-in `water`, or a declared
name) and overrides individual parameter values with a `shader_params (...)`
child block — any attribute name matching a declared `param` name; names
that don't match warn (`W0108`) but don't fail the build. Referencing an
undeclared, non-built-in shader name is a hard error (`E0106`) at
validation time. Shaders declared in an imported file aren't currently
visible to the importing file's materials — declare shaders in the file
that uses them.

---

## Physics

A `physics` substance is the *feel* of an object, the counterpart to
`material`'s *look*. Declared once and referenced with `phys=`, it lets an
engine reconstruct a rigid body — and because mogen builds real watertight
geometry, it **computes the weight and centre of gravity for you** from the
mesh. mogen runs no simulation; this is metadata written to `node.extras`.

```
material "wood" (color=[0.55, 0.35, 0.18])            // look
physics  "oak"  (weight=700kg/m3, friction=0.55, bounce=0.15)  // feel

scene {
  box "crate" (size=[1, 1, 1], mat="wood", phys="oak")   // → weighs 700 kg
}
```

Attributes: `weight` (a weight **per cubic metre**, e.g. `700kg/m3` — density in
human words, not the jargon "density"), `friction` (`0` ice … `1` rubber), and
`bounce` (`0` dead thud … `1` superball). A geometry node references a substance
with `phys="oak"`; the object's weight is `weight_per_m3 × volume` and its
centre of gravity is the mesh centroid, both auto-computed and written into
`node.extras.physics`. A flat per-node `weight=5kg` overrides the computed value
for props whose weight shouldn't follow from their size.

`phys=` **inherits** down the hierarchy like `mat=`: set it on a `group` and
every child without its own `phys=` inherits it and weighs itself. A group that
carries a substance but no mesh of its own reports the **compound** weight and
mass-weighted centre of gravity of its whole subtree, so an engine can treat the
assembly as one rigid body. Weight units carry a dimension — a mass literal
(`5kg`) is only valid on `weight=`; using one elsewhere (`size=[5kg,1,1]`) is a
parse error.

See [`physics.md`](./physics.md) for the full spec: weight units, the `extras`
shape engines read, validation codes, and current limitations.

---

## Decals

A `decal` is a transparent image (logo, label, sticker, seal) projected
onto a surface. It lowers to a thin double-sided quad floating slightly
off the parent, with an auto-synthesised `alpha_mode="blend"` material
whose albedo is an RGBA PNG.

```
decal "logo" (
  pos = [0, 0.1, 0.101],
  size = [0.25, 0.12],
  prompt = "embroidered MoGen logo, white thread on dark fabric"
)
```

Attributes:

- `size` — `[w, h]` in local units. Default `[0.5, 0.5]`. The decal is a
  flat XY quad whose normal points along its local +Z; rotate the decal
  with `rot=` / `rx`/`ry`/`rz` to point its face wherever you need.
- `prompt` — image description handed to Gemini when running
  `mogen textures`. Asks for an RGBA PNG with a fully transparent
  background; the resolved file path is spliced back into the source as
  `image="…"` for reproducibility.
- `image` — explicit path to an existing RGBA PNG (relative to the `.mog`
  file). Wins over `prompt=`; skips the LLM call entirely.
- `tint` — vec3 `[r, g, b]` multiplied against the decal's albedo.
  Default `[1, 1, 1]` (no tint).
- `roughness` — `0.0`–`1.0`. Default `0.6`.
- `offset` — `+Z` gap from the surface (avoids z-fighting); default
  `0.001`. Raise on coarse geometry.

### Curved surfaces: `on=` / `at=`

For flat surfaces, parent the decal under the surface and use `pos=`. For
curved surfaces, `on=`/`at=` synthesise a `conform` patch so the decal
hugs the curvature.

```
ellipsoid "bag" (size=[1.0, 0.5, 0.5], mat="leather") {
  connector "front_spot" (at=[0.0, 0.0, 0.25], dir=[0, 0, 1])
}
decal "bag_logo" (
  size = [0.18, 0.10],
  on   = "bag",
  at   = "front_spot",
  prompt = "embroidered MoGen wordmark, cream thread on dark leather"
)
```

- `on` — name of the target node to conform onto. Triggers the shortcut.
- `at` — required when `on=` is set; names the connector on the target that
  acts as the patch anchor.
- `up` — optional `x|y|z`; which local axis points along the surface normal.
  Defaults to `z` (the decal quad's face direction).
- `lift` — optional outward offset along the surface normal, applied during
  conform. Layered on top of the per-mesh `offset=` value, so use `lift=`
  on coarse target geometry where you need extra separation. Defaults to 0.

When `on=` is set the decal is reparented under the target. Its `pos=` is
dropped (positioning comes from `at=`), but `rot=` and `scale=` are baked
into the artwork before projection — so `rz=90` spins the logo in the
tangent plane, `scale=[2, 1, 1]` widens it. Off-plane rotations work but
rarely produce what authors want; reach for `rz=` for "spin the logo."

If neither `prompt=` nor `image=` is set, the decal's name is used as the
prompt. Rules:

- `mat=` is rejected — decals own their material. Use `tint`/`roughness`.
- Each decal gets its own auto-named material (`__decal_<name>`) and is
  never merged into same-material siblings by the export merge pass.
- `mogen textures` asks Gemini for transparent-background RGBA directly;
  there is no chroma-key step.
- `at=`, `up=`, `lift=` are validation errors without `on=`.

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

Stored as a position + quaternion (rotates `+Y` onto `dir`). `tag` groups
compatible attach points (e.g. every leg top shares `tag=leg_top`) so
fitting logic can pair them. When a node is the `child` of an `attach`,
its `pos`/`rot` apply as a local offset on top of the alignment (so
Studio gizmo drags persist).

---

## Attach: rigid alignment of two connector frames

`attach` aligns a child's `plug` connector with a parent's `socket`,
then reparents the child under the parent. Rigid alignment with optional
offset + roll.

```
scene {
  cylinder "trunk"  (radius=0.2, height=1.0) {
    connector "top" (at=[0, 0.5, 0], dir=[0, 1, 0], tag=trunk_top)
  }
  sphere   "canopy" (radius=0.5) {
    connector "stem" (at=[0, -0.5, 0], dir=[0, -1, 0], tag=canopy_stem)
  }
  attach (parent="trunk", child="canopy", socket="top", plug="stem")
}
```

| attribute | required | default | effect |
|---|---|---|---|
| `parent` | yes | — | name of the node carrying the `socket` connector |
| `child`  | yes | — | name of the node carrying the `plug` connector |
| `socket` | no  | `"top"` | connector name on `parent` |
| `plug`   | no  | `"bottom"` | connector name on `child` |
| `offset` | no  | `0.0` | gap (m) along the socket's outward direction; positive lifts the child away from the parent |
| `twist`  | no  | `0.0` | roll (degrees) around the socket's axis after alignment |

The child is reparented under the parent and its local TRS recomputed so
the two connector frames coincide (`+offset` along the socket normal,
`twist` around it). Author `pos=`/`rot=` on the child remain additive
offsets on top of the alignment (so Studio gizmo drags survive rebuilds).

`attach` runs per-instance inside `array`/`mirror` — one declaration glues
every replicated copy.

---

## Conform: moulding a primitive onto a target surface

`conform` deforms a child primitive's *vertex positions* so it lies on a
target mesh's surface. It has two modes:

- **Path mode (`from=`/`to=`)** — stretches a strip or tube between two
  connectors. Zips, labels, trim, hoses, ribbons, seams.
- **Patch mode (`at=`)** — bends a flat/disc child onto the local surface
  curvature at one anchor connector. Pockets, decals, plates, eye spots.

Conform deforms vertices where `attach` would only align rigidly. The mode
is picked by the attrs supplied; mixing or omitting both is an error.

```
// Path mode — strip stretched along a curve.
conform (target="bag", child="zip", from="zip_a", to="zip_b",
         along=x, lift=0.005)

// Patch mode — disc anchored at a single point.
conform (target="bag", child="pocket", at="pocket_spot", lift=0.002)
```

### Shared attributes

| attribute | required | default | effect |
|---|---|---|---|
| `target` | yes | — | name of the surface mesh node to mould onto |
| `child` | yes | — | name of the primitive whose vertices get deformed |
| `lift` | no | `0.0` | outward offset along the surface normal (m) — typically a fraction of a millimetre to avoid z-fighting |
| `reparent` | no | `1` | reparent child under target after conform; pass `0` to keep its original parent |

### Path mode — `from=` / `to=`

Each child vertex's coordinate on the `along` axis becomes a position along
the surface path; the perpendicular axes lie tangent / normal to the surface
at each sample.

| attribute | required | default | effect |
|---|---|---|---|
| `from` | yes | — | connector name on `target` — start of the path |
| `to` | yes | — | connector name on `target` — end of the path |
| `along` | no | `x` (flat strips) / `y` (tubes) | which child-local axis is the path axis |
| `width` | no | inferred from `along` | child-local axis perpendicular to path, tangent to surface |
| `height` | no | inferred from `along` | child-local "thickness" axis (along surface normal) |
| `samples` | no | `64` | path subdivisions (clamped to ≥ 2). Increase for high-curvature surfaces |
| `twist` | no | `0` | total roll (degrees) around the path tangent across the strip |

**Compatible primitives** (path mode):

- **Flat strips**: `box`, `plane`, `quad`, `curved_plane`, `slab`, `post`, `panel`, `wall`, `spline_ribbon`
- **Tubes**: `cylinder`, `capsule`, `tube`, `spline_tube` (cross-section ring rotates with the surface frame)
- **Imported meshes** via `mesh "..." (src="...")`: accepted, but `along=` is required

### Patch mode — `at=`

Each child vertex is independently snapped to its closest point on the target
surface; the child's `up` axis becomes the surface-outward direction at every
vertex. This makes flat decals (a disc, a quad) bend to follow curvature
locally without forcing the author to pick a path or supply two endpoints.

| attribute | required | default | effect |
|---|---|---|---|
| `at` | yes | — | connector name on `target` — patch centre |
| `up` | no | `y` (most flat primitives) / `z` (`quad`, `leaf_card`) | which child-local axis aligns with the surface outward normal |

**Compatible primitives** (patch mode):

- **Flat decals**: `disc`, `plane`, `quad`, `curved_plane`, `leaf_card`
- **Box-likes used as thin patches**: `box`, `slab`, `panel`, `wall` — give them a small extent on the `up` axis
- **Round primitives with a flat side**: `cylinder`, `hemisphere`, `half_cylinder` (a thin cylinder makes a perfect round disc)
- **Imported meshes**: accepted, but `up=` is required

### Rejected primitives

Closed shapes with no canonical surface axis (`sphere`, `ellipsoid`,
`icosphere`, `torus`, `torus_arc`, `superellipsoid`, `pyramid`, `cone`,
`frustum`, `lathe`, `prism`, `rounded_box`, `wedge`) and CSG result nodes
are rejected in both modes. The error message suggests the other mode if
it would have worked.

### Tessellation

The deformation reads each child vertex's `along`-axis coordinate as its
path position, so a bare `box` (only two distinct values per axis) can't
bend. `conform` auto-inserts planar cuts perpendicular to `along` when the
child is coarser than `samples / 4` segments (clamped 8–64). Author
subdivision (`curved_plane (segments_u=48)`, etc.) overrides.

### Examples

```
// Path mode — zip stretched between two connectors on a curved bag.
scene {
  ellipsoid "bag" (size=[1.0, 0.5, 0.5], mat="leather") {
    connector "zip_a" (at=[-0.4, 0.20, 0.22], dir=[0, 0, 1])
    connector "zip_b" (at=[ 0.4, 0.20, 0.22], dir=[0, 0, 1])
  }
  box "zip" (size=[0.8, 0.012, 0.04], mat="rubber")
  conform (target="bag", child="zip", from="zip_a", to="zip_b",
           along=x, lift=0.005)
}
```

```
// Patch mode — round pocket bent onto the bag's side.
scene {
  superellipsoid "body" (size=[0.6, 0.3, 0.3], ew=1.5, ns=1.2, mat="fabric") {
    connector "spot" (at=[0.3, 0, 0], dir=[1, 0, 0])
  }
  disc "pocket" (radius=0.08, segments=32, mat="accent")
  conform (target="body", child="pocket", at="spot", lift=0.002)
}
```

Tube children use `along=y` (the cylinder's long axis). Wrap-around paths
(e.g. a label around a bottle) use `along=x` on a `curved_plane`.

### Pass ordering and reparenting

Conform runs after `attach` and before skin binding. With `reparent=1`
(default) the child is moved under the target with identity local
transform; the child's user `pos=`/`rot=` is discarded. `reparent=0` keeps
the original parent and transforms the deformed mesh back into the child's
local frame.

### Path generation

Built by chord-and-snap: each sample's chord-interpolated point is
projected onto the target via closest-point query. Not a true geodesic;
crank `samples=` for high-curvature paths. `twist` ramps roll around the
path tangent from 0 at the start to `twist°` at the end.

### Reserved (not yet implemented)

Lowering rejects with a clear message: `direction` (projection-mode decal
splat), `curve` (only `"geodesic_lerp"` in v1), `via` (multi-segment paths).

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

Both copies share the body's node names, and both bind when the mesh
carries `skin="…"`.

#### `flip_bind=1`: rebind the mirrored copy to the symmetric bone

Module parameters are numeric-only, so skin-bound limb pairs that differ
**only** in their bone suffix (`shoulder_l` ↔ `shoulder_r`) can't be
DRY'd via a module. `flip_bind=1` solves it on `mirror`:

```
mirror "sleeves" (axis=x, flip_bind=1) {
  chamfered_box "sleeve" (
    pos=[0.135, 0.695, 0], size=[0.052, 0.158, 0.052],
    skin="rig", bind="shoulder_l", faceted=1
  )
}
```

emits the authored copy bound to `shoulder_l` AND a mirrored copy bound
to `shoulder_r`. The flip swaps a trailing `_l`/`_r` on the AST-resolved
`bind=` of every mesh-bearing descendant (authored locally or inherited
from a wrapping `group (bind=…)`). Binds without an `_l`/`_r` suffix pass
through unchanged. Defaults to `0`; ignored on `array`/`stack`/`grid`.

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

Children are cloned `count` times; the i-th copy is rotated by
`start_angle + 360° * i / count` around `around`. Wrap in an offset
`group` to place the first copy off-axis.

### `stack`

Lays children out along one axis using each child's computed AABB as its
slot — no manual half-size math.

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

Each child's `pos`/`x`/`y`/`z` is **additive** inside its slot.

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

A scalar `count`/`step` applies to X (1D rows); a 2-element list to X/Z
(floor patterns); a vec3 for 3D.

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

- `union` — N ≥ 1 operands. Optional `smooth=<radius>` swaps boolean union
  for an `smin` blend (used by humanoid stdlib for limb-to-torso fillets;
  a few cm at human scale). `smooth=0` (default) is the hard boolean.
- `difference` — first operand minus every subsequent operand.
- `intersect` — N ≥ 2 operands; the shared volume.

Operand transforms bake into vertices at eval time. Connectors / `material`
children on the CSG node apply; those on operands are ignored. Output is
cleaned (weld, degen-tri cull, normal recompute) for a watertight mesh.

---

## Implicit-field blob: `blob`

`blob { … }` meshes its children as a true SDF field via surface nets —
distinct from both `metaball` (sphere-cluster on a vertex-fillet approximate
union) and `union(smooth=k)` (vertex-fillet pull applied to a sharp mesh
CSG). Use `blob` whenever you want **smooth concave junctions** —
eye sockets melted into a cranium, the cleft of a heart, a nostril dipping
into a snout. The vertex-fillet path breaks down on concave junctions and
large blend radii; `blob` does not.

```
blob "heart" (blend=0.22, resolution=80, subdivide=1, mat="heart") {
  sphere    (radius=0.45, pos=[-0.30, 0.32, 0])      // left lobe
  sphere    (radius=0.45, pos=[ 0.30, 0.32, 0])      // right lobe
  ellipsoid (size=[0.85, 1.00, 0.85], pos=[0, -0.25, 0])   // apex
  sphere    (radius=0.20, pos=[0, 0.58, 0], op=subtract)   // cleft
}
```

### Attributes

- `blend` — smooth-min radius in scene units. `0` collapses to a sharp
  union. Typical organic values: `0.05–0.25`. Higher = more melt.
- `resolution` — voxels along the longest world-space axis of the AABB
  enclosing every additive child (plus padding). Default `96`; clamped to
  `[16, 256]`. Cost scales O(N³); `128` is usually plenty. Scales with the
  active `lod_scale` (linear) and per-node `lod=`.
- `subdivide` — optional Loop subdivision count applied to the surface-nets
  output. Default `0` (surface nets is already clean enough for most uses);
  bump to `1` only when you need extra shading smoothness. Stepped by
  `round(log2(lod_scale))`, capped at 3. See
  [Mesh subdivision](#mesh-subdivision-subdivide).
- `mat`, `pos`, `rot`, `scale`, `role`, `tags` — same as any other mesh node.

### Children

Only implicit-field primitives are allowed inside a `blob`:

| kind | attrs | SDF |
|---|---|---|
| `sphere` | `radius` | sphere |
| `ellipsoid` | `size=[x,y,z]` | axis-aligned ellipsoid (radii are `size/2`) |
| `box` | `size=[x,y,z]` | axis-aligned box |
| `rounded_box` | `size`, `radius` | box with rounded edges |
| `capsule` | `radius`, `height` | Y-axis capsule (caps at `±height/2 + radius`) |
| `cylinder` | `radius`, `height` | Y-axis closed cylinder |
| `torus` | `major`, `minor` | torus in XZ plane |

Each child accepts `pos` / `rot` / `scale`, which set the local-to-blob
transform applied before SDF evaluation. Non-uniform `scale` is allowed
(distances are compressed by the smallest scale axis as a conservative
Lipschitz fix); for elongated shapes prefer `ellipsoid` over a scaled
`sphere`.

The optional `op="subtract"` (synonyms: `"sub"`, `"carve"`) makes the child
a smooth carver instead of an additive mass. At least one additive child
is required — a blob of only subtracts has nothing to carve into.

### Notes

- Output is one mesh, one node. Children do not become separate scene nodes.
- The mesh is always watertight (surface nets closes any iso it extracts).
- UVs are bbox-projected (XZ planar). Pair with `uv_mode="tile"` for
  tileable surface textures; fitted image textures will project from above.
- A `blob` without `subdivide` shows mild stairstepping from the voxel
  grid; one round of Loop subdivision polishes it. Two rounds is glassy
  but quadruples tri count.

See [`examples/nature/skull.mog`](../examples/nature/skull.mog) and
[`examples/nature/heart.mog`](../examples/nature/heart.mog) for full
worked examples.

---

## Mesh subdivision: `subdivide`

Add `subdivide = N` to any mesh-producing node — primitives (`sphere`,
`box`, `lathe`, …), CSG ops (`union` / `difference` / `intersect`), or
`blob` — to run `N` rounds of Loop subdivision on the resulting mesh
before it is attached to the scene graph. Each round splits every
triangle into four; `N` is clamped to `[0, 3]` so the cap is `64×` the
input triangles.

```
union (subdivide=1, mat="bone") {
  sphere    (radius=0.5)
  ellipsoid (size=[0.6, 0.4, 0.8], pos=[0, -0.3, 0])
}
```

Use `subdivide` to polish staircased blob outputs, smooth a low-poly
primitive without re-tessellating it (`icosphere(radius=1, subdivide=2)`
is often nicer than `icosphere(subdivisions=4)` because Loop smooths
existing topology), or refine a CSG output for shading. It is a no-op on
container kinds with no own mesh (`group`, `scene`, `mirror`, `array`,
`stack`, `grid`).

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

- Children lower as normal scene nodes (still `attach`-able, still
  editable); the merge is export-time only.
- Only same-material leaf siblings merge. Different-material children stay
  separate so textures/PBR factors are preserved.
- Skinned meshes, joint-referenced nodes, and groups pass through unchanged.

### `cleanup="coplanar"`

Drops triangle pairs that share a plane with opposite normals — fixes the
"two boxes touch but don't overlap" case (perpendicular walls at a corner)
that CSG union alone can't resolve. Values: `"coplanar"` or `"none"`
(default).

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

- Defaults are either scalars (number or `$param`-expression) or constant
  `vec3`s. vec3 defaults must be fully constant (no cross-param refs).
  `list`/string/ident defaults are rejected.
- Reference inside the body as `$name`, including nested vec3 expressions
  like `[0, $h * 0.5, 0]`.

Invoke with `use`:

```
scene {
  array "legs" (count=4, around=y) {
    group "offset" (pos=[0.45, 0, 0.45]) {
      use "leg" (height=0.5, radius=0.05)
    }
  }
}
```

Rules: `use` takes the **declared** name (unknowns error); omitted args
fall back to defaults (unknown arg names error); modules can call modules
(recursion rejected); `$param` outside a module body is rejected.

## Control flow: `if`, `else`, `for`, string interpolation

Inside any `{ … }` body, three control-flow constructs branch/repeat at
module-expansion time. They run before lowering, so the resulting scene
graph never sees `if` or `for`; only the geometry they emit.

### `if (cond=…)` and `else`

`cond=` accepts any expression; comparisons evaluate to `1.0`/`0.0`. An
immediately-following sibling `else { … }` covers the false branch; a
standalone `else` is rejected.

```
if (cond=$is_glass) {
  box "pane" (size=[0.6, 1.0, 0.01], mat="glass")
}
else {
  box "panel" (size=[0.6, 1.0, 0.04], mat="oak")
}
```

### `for (var=…, from=…, to=…[, step=…])`

```
for (var="i", from=0, to=4) {
  box "fence_post_$i" (size=[0.04, 0.6, 0.04],
                       pos=[$i * 0.30, 0, 0], mat="oak")
}
```

- `var` is a string or bare ident; `$<var>` resolves to the loop value.
- Iteration covers `[from, to)` (Python-style); `from == to` is empty.
- `step` defaults to `1.0`, must be non-zero; negative walks downward.
- Module parameters and the loop var coexist; nested `for`s compose.

### String interpolation

Inside any string literal (including node names), `$name`/`${name}` are
replaced with the binding's value at expansion time. Integer bindings
render without a decimal (`leg_$i` → `leg_3`, not `leg_3.0`). `${name}`
disambiguates when followed by `_` etc. A `$` with no identifier or
unbound name is left literal.

```
for (var="i", from=0, to=3) {
  cylinder "leg_$i" (radius=0.05, height=0.6, pos=[$i * 0.4, 0, 0])
}
// Names: leg_0, leg_1, leg_2
```

Limitations: no chained comparisons (use `*` for AND, `+` for OR), no
boolean `&&`/`||`/`!`, no string concat beyond interpolation.

---

## Imports: `import`

Pull `module` declarations, `material` declarations, and the top-level
`scene { … }` of another `.mog` file into the current file. The imported
scene becomes an implicit module named after the file stem, so
`import "chair.mog"` lets you `use "chair" ()`.

```
import "shared/legs.mog"
import "objects/chair.mog"

scene {
  use "leg"   (h=0.6)
  use "chair" (pos=[ 1, 0, 0])
  use "chair" (pos=[-1, 0, 0], rot=[0, 180, 0])
}
```

`use` accepts the standard transform shortcuts (`pos`, `rot`, `scale`,
`x`/`y`/`z`, `rx`/`ry`/`rz`, `from`/`to`) and applies them as an implicit
wrapping group. If the module declares a parameter with one of those
names, the caller's value binds to the parameter instead.

`import` is a top-level directive. Takes a quoted file path and an
optional `(as=<ident>)` to override the synthesised module name.

Path resolution: relative paths join onto the importing file's directory;
absolute paths are used verbatim; paths are canonicalised before
deduplication (so `"lib.mog"` and `"./lib.mog"` load once).

What gets lifted:

- **`module` declarations** — added to the importer's module registry.
- **`material` declarations** (top-level or inside the imported scene) —
  added to the material registry. Texture paths are rooted at the
  *defining* file's directory.
- **Top-level `scene { … }`** — synthesised as `module "<stem>" () { … }`.

Rules:

- Imports are transitive; cycles are rejected with the chain in the error;
  double-import is a no-op.
- Module shadowing: **stdlib < imports < user declarations**.
- Synthesised scene-as-module name collisions across imports are a hard
  error — rename one with `(as=chair_a)`.
- Material name collisions across imports are a hard error — re-declare
  the material in the importer to shadow.
- Imported files may contain only `import`, `module`, `material`, and one
  top-level `scene { … }`. Joints, clips, skeletons aren't composable yet.

---

## Animation: `joint`, `clip`, templates

Animation lowers to glTF node-transform tracks. Every clip animates scene
nodes (or joints — scene-node aliases with a typed DOF). Clips are
top-level alongside `material`/`module`/`joint`. Skinning is additive
(see [Skeletons](#skeletons-and-skinning-skeleton-bone-skin-bind)).

### Joints

A `joint` names an articulation, picks a DOF type, and points at the scene
node it drives.

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

- `clip` holds a single `seconds=` duration and an ordered list of `track`s.
- `track` targets a joint (by name) or a scene node. For nodes, set
  `prop="translation"|"rotation"|"scale"` (`pos`/`rot` aliases accepted).
- `from`/`to` are scalars: degrees for rotation (around the joint's `axis`
  or the track's `axis=`), distance for translation, factor for scale.
  Emits two keyframes at `0` and `seconds`, linearly interpolated.
- For multi-keyframe curves, pass `keys=[[t, v], …]` (strictly ascending
  times in `[0, seconds]`). One glTF keyframe per pair, linear between.

### Easing

`track` and procedural templates accept `easing=` for non-linear curves.
glTF samplers only support LINEAR/STEP/CUBICSPLINE, so MoGen bakes the
easing into dense LINEAR samples at lower time. For authored tracks,
easing applies between consecutive keys; for templates, it warps the
procedural phase.

| name | shape |
|---|---|
| `linear` (default) | t |
| `ease_in` / `ease_out` / `ease_in_out` | quadratic |
| `ease_in_cubic` / `ease_out_cubic` / `ease_in_out_cubic` | cubic |
| `ease_in_sine` / `ease_out_sine` / `ease_in_out_sine` | half-cosine |
| `ease_in_back` / `ease_out_back` / `ease_in_out_back` | overshoots the endpoint |
| `ease_in_bounce` / `ease_out_bounce` / `ease_in_out_bounce` | multi-stage bounce |

```
clip "hop" (seconds=0.6) {
  track "body" (prop=translation, axis=[0, 1, 0],
                easing=ease_out_bounce, from=0, to=0.4)
}
spin "fan" (target="rotor", rpm=120, easing=ease_in_out_sine)
```

### Procedural templates

One-line declarations that expand into a full clip. They all take a
`target="name"` pointing at a joint or a scene node.

| template | extra attrs | effect |
|---|---|---|
| `spin` | `axis`, `rpm` (60), `easing` | continuous rotation |
| `open_close` | `axis`, `angle` (90), `seconds` (1.0), `easing` | 0° → angle → 0° swing |
| `wave` | `axis`, `amplitude` (15°), `hz` (1.0), `easing` | sinusoidal wobble |
| `flap` | `axis`, `amplitude` (30°), `hz` (2.0), `easing` | faster wobble, bigger amplitude |
| `idle` | `amplitude` (0.02 m), `hz` (0.5), `easing` | tiny translation breathe |

When the target is a joint, its `axis` is used by default; when it's a node,
pass `axis` explicitly.

```
spin "rotor_spin" (target="rotor", axis=[0, 0, 1], rpm=30)
open_close "door_swing" (target="door_hinge", angle=90, seconds=1.2)
```

---

## Skeletons and skinning: `skeleton`, `bone`, `skin=`, `bind=`

A `skeleton` is a hierarchy of named `bone` nodes that drives skinning
weights on procedural meshes. Bones are ordinary scene nodes (`kind="bone"`);
the skeleton block produces a `Skin` whose `joints` list captures every
descendant bone in depth-first order. Bind-pose inverse matrices are
computed automatically from the bones' world transforms at lower time, so
the author never writes a `Mat4`.

```
scene {
  skeleton "rig" {
    bone "hip"      (pos=[0, 0.95, 0], envelope=0.25) {
      bone "spine"  (pos=[0, 0.30, 0], envelope=0.25) {
        bone "neck" (pos=[0, 0.30, 0], envelope=0.10)
      }
      bone "thigh_l" (pos=[ 0.10, -0.05, 0], envelope=0.25)
      bone "thigh_r" (pos=[-0.10, -0.05, 0], envelope=0.25)
    }
  }

  capsule "torso" (pos=[0, 1.25, 0], radius=0.18, height=0.6,
                   mat="cloth", skin="rig")
  sphere  "head"  (pos=[0, 1.7,  0], radius=0.12, mat="skin",
                   skin="rig", bind="neck")
}
```

### `skeleton` and `bone`

- `skeleton "name" { … }` — declares the rig. Every child must be a
  `bone`. Bones nest arbitrarily; `pos`/`rot`/`scale` are parent-relative.
- `bone "name" (pos, rot, scale, envelope)` — declares a joint.
  `envelope=` (default `0.75`) is the auto-skinner's influence radius;
  smaller = tighter, larger = blends across more joints. Adjacent bones
  should overlap in envelope.

Skeletons are top-level or scene-level; they place themselves in the
scene tree but carry no geometry.

### Binding meshes: `skin="rig"`

Any mesh-bearing node with `skin="<skel name>"` becomes skinned: lowering
walks the skeleton, computes weights against the four nearest bones
(capped by `envelope`), and writes `JOINTS_0`/`WEIGHTS_0` accessors.
Group-like containers (`group`, `solid`, `stack`, `grid`, `array`,
`mirror`, `module`, `use`) propagate `skin=` to every mesh descendant.

### Rigid pinning: `bind="bone_name"`

`bind="bone"` alongside `skin=` pins every vertex rigidly to a single
bone (weight 1.0, no envelope blend) — accessories that track a joint
without deforming: heads, helmets, backpacks, hand-held props. Propagates
through groups like `skin=`. For `_l`/`_r` pairs, wrap one side in
`mirror (axis=x, flip_bind=1)` (see [`mirror`](#mirror)).

### Animating bones

Bones are scene nodes — drive them with regular `clip { track … }`:
`track "thigh_l" (prop=rotation, axis=[1, 0, 0], keys=[…])`. Stdlib
`humanoid_walk`/`humanoid_run`/`humanoid_idle`/`humanoid_jump` expand
into this shape against `humanoid_full`'s bones.

`skin=` on a CSG operand is lost (operands are fused into the parent's
mesh); put it on the CSG node or a wrapping group.

---

## Lights: `light`

Exported via glTF `KHR_lights_punctual`. A `light` is a transform-only
scene node — no mesh, no children. Direction is implicit (along local `-Z`).

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

Lights ignore `mat`, `anchor`, `from`/`to`, and relative-placement
shortcuts — only transforms, `role`, and `tags` apply.

No ambient term is emitted (glTF core has none; Godot derives ambient
from `WorldEnvironment`). For fill, use a dim directional light.

---

## Colliders

Annotate any geometry, `group`, `solid`, or `use` with `collider="aabb"` to
mark it as a collision volume:

```
slab "floor"     (size=[18.4, 0.1, 10.4], mat="wood", collider="aabb")
slab "wall_back" (size=[18.4, 3, 0.2], z=-5.1, mat="plaster", collider="aabb")
use  "desk"      (pos=[0, 0, 0.4], collider="aabb")
```

The bounding box is computed from the node's **subtree mesh extents** in
node-local space, after attach/conform/skin binding — so a collider on
`use "desk"` encloses the whole desk, and one on a `conform`-deformed
plank reflects the bent vertices. `"aabb"` is the only accepted value in
v1; mesh-less subtrees silently drop the box.

The export writes `node.extras.collider = { type, min, max }`. No physics
sim runs — this is metadata for downstream `CollisionShape3D` conversion.
Studio renders an off-by-default wireframe gizmo (**View → Show Colliders**).

---

## Shadow casting

Every node casts shadows by default. Add `cast_shadow=0` to opt a node — and
its entire subtree — out of the realtime shadow pre-pass and the exported
shadow hint:

```
plane "ground" (size=20, mat="grass", cast_shadow=0)
group "filler" (cast_shadow=0) {
  box (size=[1, 1, 1])     # inherits cast_shadow=0
  use  "rocks"             # inherits too
}
```

The flag propagates monotonically: an ancestor's `cast_shadow=0` wins over
a descendant default. The export writes `extras.cast_shadow=false` only on
opted-out nodes; the default-on case omits the key entirely. Studio's
right inspector exposes the toggle under **Shadow**.

---

## Diagnostics and tooling

- `mogen check <file>.mog` validates without building. Pass `--json` for machine-readable diagnostics (the format the LLM repair loop consumes).
- `mogen dump-scene <file>.mog --json` prints the lowered graph for debugging.
- `mogen inspect <file>.glb` reads back a GLB and prints its top-level structure.

See [`ROADMAP.md`](./ROADMAP.md) §8 for the full diagnostic catalog.
