# MoGen DSL reference

`mogen` compiles `.mog` source into glTF 2.0 GLB. This is the authoritative
reference for every node kind, attribute, and grammar form. Reusable modules
are catalogued in [`modules.md`](./modules.md).

- [Grammar at a glance](#grammar-at-a-glance)
- [File metadata: `meta`](#file-metadata-meta)
- [Global settings](#global-settings)
- [Values and expressions](#values-and-expressions)
- [Common attributes](#common-attributes)
- [Placement shortcuts](#placement-shortcuts)
- [Deformation modifiers](#deformation-modifiers)
- [Scene structure](#scene-structure-scene-group)
- [Primitives](#primitives)
- [Materials](#materials)
- [Shaders](#shaders)
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
- [Control flow: `if`, `else`, `for`, string interpolation](#control-flow-if-else-for-string-interpolation)
- [Imports: `import`](#imports-import)
- [Animation: `joint`, `clip`, templates](#animation-joint-clip-templates)
- [Skeletons and skinning: `skeleton`, `bone`, `skin=`, `bind=`](#skeletons-and-skinning-skeleton-bone-skin-bind)
- [Lights: `light`](#lights-light)
- [Colliders](#colliders)
- [Shadow casting](#shadow-casting)
- [Diagnostics and tooling](#diagnostics-and-tooling)
- [Scalar arrays and coordinate lists](#scalar-arrays-and-coordinate-lists)
- [Explicit sweep frames](#explicit-sweep-frames)
- [Relational modeling](#relational-modeling)
- [Surface-derived details](#surface-derived-details)

---

## Grammar at a glance

Every node has the same shape:

```
kind ["optional name"] [(attr=value, ...)] [{ child_nodes... }]
```

- `kind` — identifier: `box`, `group`, `scene`, `material`, `joint`, …
- `name` — quoted string. Required for `material`, `module`, `joint`, `clip`, `connector`; optional on geometry, where it defaults to the kind.
- `(…)` — comma-separated `key=value` attributes.
- `{…}` — child nodes.

`//` comments run to end of line; whitespace is insignificant. The top of the
file may hold `import`, `material`, `module`, `joint`, and `clip` declarations;
`scene { ... }` holds the geometry.

---

## File metadata: `meta`

Optional, at most once, before `material` / `scene` / `module`.

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
| `version` | string | author | semver conventional, not enforced |
| `mogen_version` | string | **toolchain** | auto-stamped on every save; don't write it |
| `description` | string | author | one-line summary |
| `tags` | list of string | author | free-form labels |
| `style` | string | toolchain | style hint from `mogen generate --style …` (e.g. `ps1`, `n64`, `low_poly`, `high_detail`, `arcade`, `voxel`, `cel_shaded`, `stylized_fantasy`, `cyberpunk`, `pixel_art`). Any string accepted; survives `modify`/`animate`/`repair` |
| `seed` | string | toolchain | random seed from `generate`/`modify`; round-trips through edits |
| `thinking` | string | toolchain | per-file thinking budget; survives `modify` |
| `prompt` | string | toolchain | original prompt; round-trips through `modify` |
| `moghub_model_id` | string | toolchain | stamped by Studio's Publish after a MoGHub upload |
| `moghub_slug` | string | toolchain | MoGHub slug, paired with `moghub_model_id` |
| `moghub_version` | string | toolchain | last-published MoGHub version |

Informational only — not consumed by geometry. Files without `meta` still
build; the toolchain inserts `meta (mogen_version = "...")` on next save.

Diagnostics: `E0310` (no body block), `E0311` (quoted name — use `name=`),
`E0312` (duplicate), `E0313` (must be top level), `W0107` (`mogen_version`
mismatch).

---

## Global settings

| directive | value | effect |
|---|---|---|
| `lod_scale (value=N)` | number, default `1.0` | multiplies every primitive `segments`/`rings`/`samples` (implicit defaults *and* explicit values) and `blob` `resolution`. `icosphere` `subdivisions` and `subdivide=` step by `round(log2(N))` instead. Counts clamp to a per-primitive minimum |

```
lod_scale (value=0.5)

scene {
  sphere "head" (radius=0.5)                         // 8 rings, 12 segments
  sphere "lod0" (radius=0.5, segments=48, rings=32)  // 16 rings, 24 segments
}
```

Studio's "LOD scale" slider edits this directive and removes it at `1.0`.

### Per-node `lod=` overrides

Any geometry/group node accepts `lod=N`, which **multiplies** the active LOD
scale for its subtree only (siblings unaffected); nested overrides multiply.
Invalid values are ignored. Per-primitive minimums still apply.

Each file's first positive, finite top-level `lod_scale` is its default (else
`1.0`). Imported geometry uses its *defining* file's scale in place of the
importer's, while enclosing `lod=` multipliers still apply across the import.
Library callers can add a further multiplier with `lower_with_loader_lod`
(invalid requests use `1.0`).

```
group "hero" (lod=2) { sphere "face" (radius=0.12) }
sphere "background_rock" (radius=0.4, lod=0.5)
```

```mog
// parts.mog
lod_scale (value=0.5)
module "part" {
  group (lod=2) { sphere "detail" (radius=0.5) }   // 0.5 × 2 × 2 = 2.0 when used below
  sphere "plain" (radius=0.5)                     // 0.5 × 2 = 1.0
}

// main.mog
import "parts.mog"
lod_scale (value=0.25)
scene {
  group (lod=2) { use "part" }
  sphere "local" (radius=0.5)                     // 0.25
}
```

---

## Values and expressions

| form | example | notes |
|---|---|---|
| number | `0.5`, `-90`, `1` | `f32` |
| length | `1.5m`, `90cm`, `18in`, `2ft` | normalised to metres (see [Length units](#length-units)) |
| vec3 | `[1.0, 0.5, 0.0]` | three expressions |
| list | `[0, 90]`, `[1, 2, 3, 4]` | any arity |
| string | `"wood"` | names and references |
| ident | `wood`, `y` | axes and enum-like values |
| expression | `$height * 0.5`, `($a - $b) / 2` | arithmetic over `$param` refs |
| comparison | `$h > 0`, `$count == 4` | `<`, `<=`, `>`, `>=`, `==`, `!=` — for `if`/`for` |

Operators `+ - * /` use conventional precedence with parentheses. Expressions
are evaluated at module-expansion time.

### Length units

The base unit is the **metre**; a bare number is metres. Append a suffix to
any number:

| suffix | metres |
|---|---|
| `mm` | `0.001` |
| `cm` | `0.01` |
| `m` | `1.0` |
| `km` | `1000.0` |
| `in` | `0.0254` |
| `ft` | `0.3048` |
| `yd` | `0.9144` |

```
box "wall"  (size=[2m, 100cm, 8in], pos=[3ft, 0, 0])
box "joist" (height=5ft + 6in)        // units compose through arithmetic
```

Suffixes resolve at parse time in every numeric position (scalars, vec3
components, list items, `$param` arithmetic), and units may be mixed. A unit
literal lowers identically to its metre equivalent. Use them for lengths only —
rotations stay degrees, `scale` unit-less, counts/colours plain.

### Weight units

Weights normalise to the **kilogram**; a bare number is kilograms.

| suffix | kg |
|---|---|
| `g` | 0.001 |
| `kg` | 1.0 |
| `t` | 1000 |
| `lb` | 0.4536 |
| `oz` | 0.02835 |
| `st` | 6.350 |

A `physics` block's `weight=` is **per cubic metre**: append `/m3` (or `/m³`),
e.g. `700kg/m3`, `0.7t/m3`, `62lb/m3`. A node's `weight=` is a flat mass
(`5kg`). Mass literals are only valid on `weight=`; `size=[5kg,1,1]` is a parse
error.

---

## Common attributes

Apply to every geometry node and to `group`:

| attribute | value | effect |
|---|---|---|
| `pos` | `vec3` | translation in the parent frame; default `[0, 0, 0]` |
| `rot` | `vec3` Euler XYZ **degrees**, or list | default identity |
| `scale` | scalar or `vec3` | default `1` |
| `mat` | string/ident | declared `material` name |
| `phys` | string/ident | declared `physics` substance; see [Physics](#physics) |
| `weight` | mass (`5kg`) | flat per-node weight override |
| `role` | string/ident | semantic label, exported to glTF `extras` |
| `tags` | comma-separated string | free-form labels, exported to `extras` |

Transforms compose child → parent, as in glTF.

---

## Placement shortcuts

### Per-component shortcuts

| shortcut | overrides | notes |
|---|---|---|
| `x=`, `y=`, `z=` | components of `pos` | other axes fall back to `pos`, else `0` |
| `rx=`, `ry=`, `rz=` | components of `rot` (degrees) | same fallback |
| `w=`, `h=`, `d=` | components of `size` (X, Y, Z) | `plane`/`curved_plane` use `w`/`d` (XZ); `quad` uses `w`/`h` (XY) |

```
box (y=1.5, size=1)            // pos=[0, 1.5, 0], size=[1,1,1]
box (size=[2, 2, 0.1], h=3)    // size=[2, 3, 0.1]
cylinder (rx=90, radius=0.2, height=1)
```

### Scalar `size` (cube shorthand)

`size=<number>` expands to a uniform vec3 on any primitive taking `size=[…]`.

### `from` / `to` — axis-aligned box by corners

On any `size` primitive, `from=[x1,y1,z1]` + `to=[x2,y2,z2]` sets `size` to
`|to − from|` and `pos` to the midpoint:

```
box (from=[-2, 0, -1.5], to=[2, 2.8, -1.4], mat="wall")
// = box (pos=[0, 1.4, -1.45], size=[4.0, 2.8, 0.1])
```

### `anchor` — place by face, not centre

`pos` places the primitive's **anchor point** (default `center`).
Values are underscore-joined tokens from `center`, `top`, `bottom`, `left`,
`right`, `front`, `back`:

```
box (y=0, size=[1, 2, 1], anchor=bottom)             // bottom face on y=0
box (size=2, anchor=bottom_left_front)               // corner at the origin
```

The anchor shifts mesh vertices; default face connectors (`top`, `bottom`, …)
move with it.

### Relative placement: `above`, `below`, `left_of`, `right_of`, `in_front_of`, `behind`

Set one (at most one per node) to the name of a **prior sibling** in the same
parent; the node moves so its face is flush with the sibling's opposite face,
plus optional `gap`. Uses the sibling's fully lowered subtree AABB.

```
group "chests" {
  box "chest_lo" (size=[0.8, 0.6, 0.5])
  box "chest_hi" (above="chest_lo", gap=0.02, size=[0.8, 0.6, 0.5])
}
```

---

## Deformation modifiers

Every primitive accepts these, applied after mesh construction and before
anchor placement:

| attribute | value | effect |
|---|---|---|
| `bend_x`, `bend_y`, `bend_z` | degrees | arc-length-preserving bend around the axis. Length axis: Y for `bend_x`/`bend_z`, X for `bend_y` |
| `twist_y` | degrees | helical twist around Y, 0 at `y_min` → full at `y_max` |
| `taper` | ratio (1 = unchanged, 0.5 = half-width at top) | linear shrink along Y |
| `droop` | 0..1 of length | quadratic sag along -Y; base fixed, top sinks `amount * height` |
| `noise` | 0..1 | coherent value-noise displacement along normals (rocky) |
| `jitter` | 0..1 | per-vertex random displacement along normals (jagged) |
| `faceted` | 0/1 | unshared vertices + flat normals (low-poly look) |
| `seed` | integer, default `1` | seed for `noise`/`jitter` |
| `wave` | peak displacement (m) | sinusoidal displacement along normals |
| `wave_frequency` | cycles/unit, default `1.0` | `0.5` = crest every 2 m |
| `wave_axis` | `"x"`/`"y"`/`"z"`, default `"x"` | propagation axis |
| `wave_phase` | radians, default `0.0` | phase offset |
| `wave_range` | `[a, b]` | gates the wave along `wave_axis` like `*_range` below |

```
icosphere "rock"   (radius=0.4, noise=0.30, jitter=0.15, faceted=1, seed=7)
cylinder  "candle" (radius=0.18, height=0.7, droop=0.4, noise=0.05)
box       "beam"   (size=[0.2, 0.2, 3], twist_y=20, taper=0.7)
```

Same seed ⇒ same surface; unseeded siblings with equal `noise` are identical,
so give them distinct seeds.

`bend_x_range`, `bend_y_range`, `bend_z_range`, `twist_y_range`,
`taper_range`, `droop_range`, `noise_range`, `jitter_range` take `[a, b]`
(normalised along the length axis; endpoints auto-sorted): unchanged below
`a`, smoothstep ramp across `[a, b]`, full above `b`.

```
box "blade" (size=[0.06, 1.4, 0.01], bend_z=18, bend_z_range=[0.55, 1.0])
```

Default tessellation doubles segments (+1 icosphere subdivision) when a smooth
deformer is present; explicit `segments=`/`rings=`/`subdivisions=` override.
Modifiers don't apply to `mesh` primitives.

Implicit tessellation on curved primitives also scales with the cube root of
the primitive's local extent (radius etc.) relative to its default size.
Placement `scale=` is not considered; set explicit counts for an unusually
scaled placement.

---

## Scene structure: `scene`, `group`

```
scene {
  group "chair" (pos=[0, 0, 0]) {
    box "seat" (pos=[0, 0.5, 0], size=[1.0, 0.1, 1.0])
  }
}
```

- `scene` — root container. Top-level nodes outside it become extra roots.
- `group` — transform-only container.
- `solid` — like `group`, but same-material leaf children are CSG-unioned at export. See [Solid groups](#solid-groups-solid).

---

## Primitives

All primitives take the common attributes plus:

| kind | required attrs | other attrs |
|---|---|---|
| `box` | `size=[x,y,z]` | `faces=[…]` — six entries, `+X,-X,+Y,-Y,+Z,-Z`; see [per-face materials](#per-face-box-materials-faces) |
| `plane` | `size=[x,_,z]` (Y ignored) | XZ-aligned |
| `quad` | `size=[w,h]` or vec3 | XY-aligned |
| `cylinder` | `radius`, `height` | `segments` (24) |
| `cone` | `radius`, `height` | `segments` (24) |
| `sphere` | `radius` | `rings` (16), `segments` (24) |
| `capsule` | `radius`, `height` | `rings` (8), `segments` (24) |
| `torus` | `major`, `minor` | `major_segments` (24), `minor_segments` (12) |
| `prism` | `size=[x,y,z]` | triangular prism along +Z |
| `pyramid` | `radius`, `height`, `sides` | N-sided base |
| `disc` | `radius` | `segments` (24) |
| `icosphere` | `radius` | `subdivisions` (2) |
| `rounded_box` | `size`, `radius` | `segments` per corner (4) |
| `chamfered_box` | `size` | `radius` (bevel, 0.1) — 45° bevels on all edges |
| `inset_box` | `size` | `face` (`"+y"`/`"-y"`/`"+x"`/`"-x"`/`"+z"`/`"-z"` or `"top"`/`"bottom"`/`"left"`/`"right"`/`"front"`/`"back"`, default `"+y"`), `amount` (inset, 0.1), `depth` (sink, 0.05) — one sunken panel |
| `wedge` | `size` | right-triangle prism: flat bottom on -Y, slope rising toward +Y/+Z |
| `frustum` | `bottom=[w,d]`, `top=[w,d]`, `height` | truncated rectangular pyramid (defaults `[1,1]`, `[0.5,0.5]`, `1`) |
| `tube` | `outer`, `inner`, `height` | hollow cylinder; `segments` (24) |
| `hemisphere` | `radius` | flat side on -Y; `rings` (8), `segments` (24) |
| `half_cylinder` | `radius`, `height` | flat side facing -Z; `segments` (24) |
| `torus_arc` | `major`, `minor` | `arc` (degrees, 90) around +Y; `major_segments` (24), `minor_segments` (12) |
| `ellipsoid` | `size` | `rings` (16), `segments` (24) |
| `superellipsoid` | `size` | `ew`, `ns` (1 = sphere, >1 boxy, <1 pinched), `rings` (16), `segments` (24) |
| `curved_plane` | `size=[x,z]` or vec3 | `bend_u`, `bend_v` (degrees along X/Z), `segments_u`/`segments_v` (12) |
| `lathe` | `profile=[[r,y], …]` | bottom-to-top; `segments` (24), `cap_ends` (1) |
| `spline_tube` | `points=[[x,y,z], …]` | `radius` or per-point `radii=[…]`, `segments` (12), `samples` (8), `cap_ends` (1) |
| `spline_ribbon` | `points=[[x,y,z], …]` | `width` or `widths=[…]`, `samples` (8), `twist` (degrees, 0); flat strip |
| `coil` | — | helix. `radius` (0.5), `height` (1.0), `turns` (3), `profile_radius` (0.05), `segments` (12), `samples` per turn (16), `cap_ends` (1), `handedness` (`"right"`/`"left"`) |
| `heightfield` | — | fBm-displaced XZ grid. `size=[w,d]` (`[1,1]`), `segments_u`/`segments_v` (32), `amplitude` (0.5), `octaves` (1..=8, 3), `frequency` (1.0), `persistence` (0.5), `seed` (1) |
| `bezier_patch` | `points=[…]` (16, row-major u × v) | `segments_u`/`segments_v` (12). Corners `[0]/[3]/[12]/[15]`; `[5]/[6]/[9]/[10]` shape the bulge |
| `metaball` | `points=[…]` (≥1) + `radius=` or `radii=[…]` | `blend` (smooth-union radius, 0), `rings` (12), `segments` (16). Sugar for `union (smooth=k) { sphere … }` |
| `blob` | implicit-field children | true SDF blend; see [Implicit-field blob](#implicit-field-blob-blob) |
| `extrude` | `points=[[x,z], …]` (closed CCW) | `hole=[[x,z], …]` (one CW contour), `height` (1.0), `taper` (1.0), `twist` (0), `caps` (1). Multiple holes: `extrude` + `difference` |
| `sweep` | `profile=[[x,y], …]` (closed CCW), `path=[[x,y,z], …]` | `samples` per segment (8), `twist` (deg), `roll=[deg, …]`, `scale_along=[s, …]`, `caps` (1) |
| `loft` | `points=[[x,z], …]`, `heights=[y, …]` | sections flat-packed in `points` (equal vertex counts); `samples` (4), `caps` (1) |
| `hull` | `points=[[x,y,z], …]` (≥4) | convex hull; interior points ignored |
| `poly` | `points=[[x,y,z], …]`, `indices=[i0,i1,i2, …]` | optional `uvs=[[u,v], …]` (one per point, used verbatim; ignores material UV mode). Shared vertices shade smooth, unshared flat. One `mat=` |
| `leaf_card` | `size=[w,h]` | `cards` (2) crossed quads. Pair with `alpha_mode="mask"`, `double_sided=1` |
| `mesh` | `src="path.glb"` | external GLB as one mesh, path relative to the `.mog`. Its materials/skins/animations are dropped |
| `branch` | — | procedural tree; see [Branch](#branch) |
| `slab` | `size` | `box` with default anchor `bottom` |
| `post` | `size` | `box` with default anchor `bottom` |
| `panel` | `size` | `box` with default anchor `back` |
| `wall` | `size` | `holes=[[cx,cy,w,h], …]` rectangular cutouts through Z |

Nested list attributes (`points`, `profile`, …) must be constant (no `$param`);
parameterise via a module wrapper. `spline_tube`/`spline_ribbon` follow a
Catmull–Rom curve with a parallel-transport frame.

`wall` holes are in local X/Y, cut through Z, and produce one watertight mesh:

```
wall "barracks" (size=[3, 3, 0.1], holes=[
  [-0.75, -0.4, 0.9, 2.0],   // door
  [ 0.9,  0.3, 0.8, 0.8],    // window
])
```

Defaults make a bare `cylinder "leg"` a 1 m, unit-radius cylinder at the origin.

### Per-face box materials (`faces=`)

`faces=[…]` gives a `box` exactly six materials in order **`+X, -X, +Y, -Y, +Z, -Z`**:

```
box "crate" (size=[2, 1, 1], faces=[
  "wood", "wood",   // +X, -X
  "metal",          // +Y (lid)
  "wood", "wood", "wood"
])
```

An empty entry `""` uses the box's own/inherited `mat=`. The box lowers to an
editable wrapper (transform, anchor, connectors) plus one frozen child per
distinct material (the crate above has two children).

#### Authored per-face UV transforms (`face(...)`)

Any `faces=` entry may be `face("mat", …)` to bake an explicit UV transform,
bypassing the material's `fit`/`tile` projection:

```
box "wall_seg" (size=[4, 3, 1], faces=[
  "wall", "wall", "wall", "wall",
  face("panel", uv_scale=[0.25, 0.333], uv_offset=[0, 0], uv_swap=false),  // +Z
  "wall"
])
```

- first argument — material name (`""` falls back as above).
- `uv_scale=[sx, sy]` — may be negative to mirror. Default `[1, 1]`.
- `uv_offset=[ox, oy]` — added after scaling. Default `[0, 0]`.
- `uv_swap=<bool>` — swap the in-plane axes first. Default `false`.

`face("mat")` with no UV fields equals `"mat"`. For a face normal to axis *A*,
the in-plane axes are the other two in ascending order (`X→(Y,Z)`, `Y→(X,Z)`,
`Z→(X,Y)`), with local coordinate `vertex + size/2`. UV =
`offset + scale * (swap ? swapped : local)`, emitted unclamped.

### Branch

`branch` expands into a recursive cluster of `spline_tube` segments from trunk
to twigs, with optional `leaf_card` clusters at tips. Output is frozen
(a function of `seed=`).

| attribute | default | effect |
|---|---|---|
| `form` | `"decurrent"` | habit preset (below); sets defaults for this table, explicit attrs win |
| `length` | `1.0` | trunk length (m) |
| `radius` | `0.05` | trunk base radius (m) |
| `depth` | `4` | branching levels |
| `splits` | `2` | children per split |
| `length_falloff` | `0.7` | per-level length multiplier |
| `radius_falloff` | `0.6` | per-level radius multiplier |
| `branch_angle` | `35` | child lean off parent tangent (deg) |
| `roll` | `137.5` | roll between successive children (deg) |
| `tropism` | `0.0` | per-segment bias toward +Y (negative droops) |
| `bend` | `10` | random bend per segment (deg) |
| `leader_bias` | `0.0` | `0`–`1`; at `1` child 0 continues straight at full size (pine-like) |
| `multi_stem` | `1` | trunks from the base; `form="shrub"` only |
| `segments` | `8` | radial segments per tube |
| `samples` | `4` | samples per spline segment |
| `seed` | `1` | RNG seed |
| `jitter` | `0.2` | `0`–`1` random perturbation of lengths/angles |
| `leaves` | `1` | `0` disables leaf cards |
| `leaf_size` | `0.35` | leaf-card height (m) |
| `leaf_aspect` | `1.0` | width/height; `<1` needles, `>1` broad |
| `leaf_cards` | `2` | cards per cluster (fronds for palm) |
| `leaf_mat` | inherited `mat=` | leaf material |

| `form` | habit | distinctive defaults |
|---|---|---|
| `"decurrent"` | broadleaf (oak, maple) | equal forks, square leaves |
| `"excurrent"` | conifer | strong leader, near-horizontal branches, needle leaves |
| `"weeping"` | willow | strong negative tropism, long narrow leaves |
| `"shrub"` | bush | `multi_stem=4`, no leader |
| `"palm"` | palm | straight trunk, frond fan at tip, no recursion |

### Building

`building` expands into a multi-storey interior: floor/ceiling slabs, perimeter
walls with window and entrance cutouts, interior walls with doors, stamped
door/window/skylight modules, stairs, elevators, and a roof. The subtree is
frozen (a function of `seed=` and attrs). It emits no furniture geometry — only
POI markers.

| attribute | default | effect |
|---|---|---|
| `seed` | `1` | RNG seed. |
| `style` | `"grid"` | Layout: `grid`, `apartment-block` (BSP, aspect ≤ 2.5), `office-core` (central corridor, ~3 m offices both sides), `hotel-corridor` (central corridor, ≥ 2.4 m rooms both sides), `radial` (concentric bands), `organic` (jittered grid), `maze` (spanning-tree corridor + small rooms). All rooms are axis-aligned rectangles; corridor styles fall back to `grid` on plates too small for them and synthesise a `corridor` room type if undeclared. |
| `mat_style` | `""` | Style hint for texture generation; no geometric effect. |
| `floor_area` | `120` | Target area per floor (m²). Aspect ratio near √2. |
| `cellar_area` | _unset_ | Smaller basement footprint (m²), east-aligned with the above-ground plate. Clamped to ≤ `floor_area` (`W1116`); too small for the circulation column → `W1115`. |
| `rooms` | `4` | Total rooms (≥ 1) across all floors, distributed by floor area. |
| `floors_above` | `1` | Storeys above ground incl. ground (≥ 1). |
| `floors_below` | `0` | Basement storeys. |
| `windows` | `0` | Total above-ground windows, spread across storeys (remainder to lower floors). Basements get none. |
| `skylights` | `0` | Top-storey ceiling cutouts. Flat roof only; otherwise skipped with `W1114`. |
| `roof` | `"flat"` | `flat`, `pitched`, `gabled`, `hipped`, `mansard`, `shed` (see below). |
| `roof_overhang` | _unset_ | Eaves projection (m) for `gabled`/`pitched`. Unset: `gabled` flush, `pitched` modest default. |
| `ceiling_height` | `2.6` | Clear height per storey (m). |
| `door_w`, `door_h` | `0.9, 2.1` | Door opening (m); used for internal and external doors. |
| `window_w`, `window_h` | `1.2, 1.4` | Medium window opening (m). Small ×0.6, large ×1.4. |
| `wall_thickness` | `0.12` | Exterior and interior walls (m). |
| `ceiling_thickness` | `0.2` | Slab thickness (m); a storey's floor is the ceiling below. |
| `entrances` | `1` | Ground-floor external doors (≥ 1). |
| `external_door` | `"door_simple"` | Module stamped at each entrance. |
| `internal_door` | `"door_simple"` | Module stamped at each interior door. |
| `window_small`, `window_medium`, `window_large` | `"window_simple"` | Per-size-class window modules. |
| `skylight` | `"skylight_simple"` | Skylight module. |
| `elevators` | `0` | Full-height shafts in the east circulation column (`elevator_shaft_simple`). |
| `staircases` | `0` | One flight per storey transition (`stair_simple`). Multi-storey with `0` → `W1113`. |
| `furnish` | `1` | Emit furniture POI markers per room; `0` = bare shells. |
| `debug_show_poi` | `0` | Give every POI a small emissive sphere (`building_poi_<role>`), colour-coded. Turn off for production. |
| `debug_hide_roof` | `0` | Drop the top ceiling slab (and skylights) to view from above. |
| `debug_render_floor` | _unset_ | Render only this storey (`0` ground, `1..` upper, `-1..` basement), without ceiling or circulation. |

Numeric sizes/areas must be > 0 and counts ≥ 0 (`E1104`/`E1105`); unknown
`style`/`roof` → `E1102`/`E1103`.

Roofs: non-flat roofs replace the top ceiling slab, base at `ceiling_height`,
fixed 30° pitch. `shed` slopes south→north; `gabled` ridges along the longer
axis with flush triangular end-walls (`role="gable_wall"`); `pitched` is a gable
whose slopes overhang all sides; `hipped` has four slopes to an apex or ridge;
`mansard` has a steep ~60° lower tier and shallow upper tier.

Children: only `room_type` and `adjacency` (`E1101` otherwise).

```
room_type "<name>" (kind=public|private|service|utility|secure|staff_only,
                    density=0..10, mat="<material>",
                    min_area=…, max_area=…)
adjacency "<room_type>" (adjacent_to=["a", "b"], away_from=["c"])
```

`kind` is required. `density` (default `1`) weights sampling; `0` disables the
type. `mat` sets that room's floor/wall material. `min_area`/`max_area` (m²) are
soft bounds. Adjacency is **soft**: 10 sub-seeded layouts are scored and the
best kept (ties → lowest attempt). Score: `+1`/m shared wall for `adjacent_to`,
`-1`/m for `away_from`, plus built-in priors (same-kind cells cluster;
public↔private and public↔secure repel; public near the south entrance,
private/secure away, service mid-plate) and a ~0.2/m² penalty outside the area
band. An undeclared room type is a validation error.

```
building "house" (seed=4, style="apartment-block",
                  floor_area=85, rooms=6, windows=6, mat="wood") {
  room_type "bedroom"  (kind=private, density=4)
  room_type "kitchen"  (kind=service, density=1, mat="tile")
  room_type "living"   (kind=public,  density=2)
  adjacency "kitchen"  (adjacent_to=["living"])
}
```

Output structure (basement storeys are `floor_b1`, …):

```
house (building)                     editable wrapper
├── floor_0 (group)                  one per storey
│   ├── shell: slab_floor, slab_ceiling, wall_N/E/S/W
│   ├── rooms/room_<n>: interior_wall_<i>, connector "centre", furniture/<prop>_<i> (poi)
│   ├── openings: door_<i>, ext_door_<j>, window_<k>, skylight_<m>   (module instances)
│   ├── opening_pois: entrance_<i>, door_<i>, window_<i>             (poi)
│   └── circulation
│       ├── staircase_<n>/…, stair_access_pois/stair_access_<i>     (poi)
│       └── elevator_<n>/…,  elevator_stops/elevator_stop_<i>       (poi)
└── roof (group)
```

Geometry nodes carry `tags` (`building`, storey e.g. `floor_0`, kind such as
`exterior_wall`/`interior_wall`/`entrance`/`window`, `room_type=<name>` on rooms)
and `role` (`floor`, `ceiling`, `wall`, `door`, `roof`, …). Opening module
groups carry width/height in `extras.slot`.

POI markers (`kind="poi"`, no mesh, no collider; LOD-invariant):

| role | tags | pose |
|---|---|---|
| furniture prop (`bed`, `stove`, `server_rack`, …) | `building, poi, furniture, <prop>` | room-local position + yaw: wall props face in, corner props face centre, ceiling fixtures at `ceiling_height` |
| `entrance` | `building, poi, door, entrance` | threshold, local +Z = wall outward normal (same pose as the module instance) |
| `door` | `building, poi, door` | as above |
| `window` | `building, poi, window` | sill, +Z outward |
| `stair_access` | `building, poi, staircase, stair_access, floor=<n>` | one per storey at the stair entry platform, +Z toward shaft door |
| `elevator_stop` | `building, poi, elevator, elevator_stop, floor=<n>` | one per served storey at shaft centre on the floor, +Z toward shaft door |

`floor=<n>` is the signed storey index. Opening and circulation POIs are always
emitted; skylights get none. Furniture markers only when `furnish=1`; the
`furniture` group carries `cat=<category>`. Prop count scales with room area
(capped); rooms under ~0.5 m² usable floor get none.

Category is keyword-matched from the `room_type` name (fallback: `kind`):

| category | name contains | sample props |
|---|---|---|
| bedroom | `bed`, `dorm`, `master`, `cabin`, `nursery` | bed, wardrobe, nightstand, dresser, desk |
| bathroom | `bath`, `toilet`, `shower`, `ensuite`, `wc` | toilet, sink, bathtub, shower, vanity |
| kitchen | `kitchen`, `galley`, `kitchenette` | counter, stove, oven, fridge, dishwasher, island |
| pantry | `pantry`, `larder` | shelving, dry-goods rack |
| dining | `dining`, `mess`, `canteen` | dining table, chairs, sideboard, chandelier |
| living | `living`, `lounge`, `den`, `family room` | sofa, armchair, tv, coffee table, fireplace |
| office | `office`, `study`, `cubicle`, `workspace` | desk, office chair, filing cabinet, whiteboard |
| meeting | `meeting`, `conference`, `boardroom` | conference table, chairs, projector screen |
| reception | `reception`, `waiting`, `concierge` | reception desk, waiting sofa, magazine rack |
| lobby | `lobby`, `foyer`, `atrium`, `entrance` | bench, planters, info board, directory sign |
| corridor | `corridor`, `hallway`, `landing`, `stair` | bench, wall art, fire extinguisher |
| storage | `storage`, `store`, `stock`, `supply` | shelving, storage boxes, pallets |
| closet | `closet`, `cloak`, `cupboard` | clothes rail, shelves, shoe rack |
| garage | `garage`, `carport`, `parking` | car, workbench, tool cabinet, pegboard |
| workshop | `workshop`, `maker`, `machine shop` | workbench, machine tool, material rack |
| laundry | `laundry`, `washing` | washing machine, dryer, ironing board |
| utility | `mechanical`, `boiler`, `hvac`, `electrical` | boiler, electrical panel, hvac unit |
| server room | `server`, `data centre`, `comms`, `rack room` | server racks, network cabinet, ups, crac |
| retail | `retail`, `shop`, `showroom`, `sales floor` | display shelves, checkout, clothing racks |
| warehouse | `warehouse`, `depot`, `loading`, `freight` | pallet racks, forklift, packing station |
| classroom | `classroom`, `lecture`, `seminar` | student desks, teacher desk, whiteboard |
| library | `library`, `archive`, `reading` | bookshelves, study tables, librarian desk |
| lab | `lab`, `research`, `cleanroom` | lab benches, fume hood, biosafety cabinet |
| medical | `clinic`, `exam`, `surgery`, `dental` | exam table, supply cabinet, exam light |
| ward | `ward`, `patient`, `recovery`, `icu` | hospital beds, iv stands, vitals monitors |
| gym | `gym`, `fitness`, `workout` | treadmills, weight benches, racks, mirror wall |
| restaurant | `restaurant`, `diner`, `cafe`, `coffee` | tables, chairs, booths, host stand |
| bar | `bar`, `pub`, `tavern`, `saloon` | bar counter, stools, back-bar shelf, beer tap |
| cell | `cell`, `holding`, `prison` | bunk, toilet, sink, stool |
| generic | anything else | table, chairs, shelving, plant |

### Cave

`cave` expands into one watertight rock shell — a bounding block minus a
smooth-blended union of chamber and passage carvers, surface-nets meshed — plus
optional decorations and POIs. The subtree is frozen (a function of `seed=` and
attrs). Props, lights and creatures are not generated; place them with `use`.

Layout: chambers (oblate ellipsoids) sit in `levels` stacked layers separated
by `level_gap` rock and joined by `level_links` vertical passages. Within a
layer, chambers are kept `spacing` apart, except that with probability
`overlap` one merges with a neighbour into a larger cavern. Each layer is
connected by a spanning tree plus `loops`; every chamber is reachable. Passages
steeper than `max_slope` become switchback ramps, so no walkable surface exceeds
it. Chamber radius is silently capped to fit a layer's height (more `levels` ⇒
smaller rooms). Entrances punch horizontal mouths through the nearest side face;
the shell is otherwise closed.

| attribute | default | effect |
|---|---|---|
| `seed` | `1` | RNG seed. |
| `size` | `[24, 10, 24]` | Rock block `[width, height, depth]` (m); base on `y=0`. |
| `chambers` | `6` | Chambers carved. |
| `levels` | `2` | Stacked layers. |
| `level_gap` | `1.5` | Rock between layers (m). |
| `level_links` | `1` | Vertical ramps per adjacent layer pair (≥ 1 when `levels > 1`). |
| `chamber_min`, `chamber_max` | `2.5, 5.0` | Chamber radius range (m); swapped if reversed (`W1205`). |
| `spacing` | `2.0` | Min rock gap between same-layer chambers (m); crowded layers shrink chambers. |
| `overlap` | `0.35` | `[0, 1]` probability of merging with a neighbour. |
| `chamber_flatten` | `0.6` | `height = radius × this`, clamped `[0.2, 1.0]`. |
| `passage_radius` | `1.1` | Tunnel radius (m). |
| `loops` | `1` | Extra connections beyond the spanning tree. |
| `max_slope` | `45` | Max walkable slope (deg), clamped `[5, 89]`. |
| `roughness` | `0.35` | Wall noise `[0, 1]` (displacement capped ~0.35 m). |
| `blend` | `1.5` | Smooth-union radius (m). |
| `margin` | `2.0` | Rock thickness around the void (m). |
| `resolution` | `96` | Voxel grid, clamped `[32, 224]`. A 26 m cave at 112 is ~75k tris. |
| `lod_scale` | `1.0` | `[0.1, 1.0]`; scales voxel grid + decoration tessellation only. Layout/counts/POIs unchanged. |
| `entrances` | `1` | Scalar `N`: mouths on the top (surface) band. Array `[b0, b1, …]`: per band from the bottom (e.g. `[1, 0, 1]`); bands past the end get none. Prefers chambers nearest a wall. |
| `mat` | `cave_rock` | Rock material (auto-stamped grey stone if undeclared). |
| `mat_style` | `""` | Style hint for texture generation. |
| `water_mat` | `cave_water` | Pool/lake material. |
| `colliders` | `all` | Trimesh colliders on rock: `all` (shell + solid decorations), `shell`, `none`. |
| `water_collider` | `0` | `1` = pools/lakes get a collider (stand on water). |
| `rock_piles`, `pools`, `lakes`, `stalagmites`, `stalactites`, `columns` | `0` | Decoration counts (`columns` span floor→ceiling). |
| `mushrooms` | `0` | `mushroom_spot` POI count. |
| `debug_hide_shell` | `0` | Cut away the front (+Z) half of the rock (and decorations there) for a cross-section. |
| `debug_show_poi` | `0` | Give each POI a bright `cave_poi_<kind>` sphere (dead-end magenta, column blue, ladder lime, mushroom amber). |

`collider="aabb"` is ignored on `cave`; use `colliders=`. Decorations are placed
on the real carved floor/ceiling (sunk slightly to sit flush) but not
collision-tested against each other; re-seed if one reads badly.

Children: only `feature` (`E1201`), each tuning one decoration kind:

```
feature "<name>" (kind=stalagmite|stalactite|column|rock_pile|pool|lake,
                  count=…, min_size=…, max_size=…, mat="<material>")
```

`kind` is required. `count` overrides the top-level count; `min_size`/`max_size`
set the size range (m; radius for spikes/piles, surface radius for water); `mat`
overrides the material.

```
cave "hollow" (seed=12, size=[26, 12, 26], chambers=7, levels=2,
               max_slope=45, entrances=2, mat="limestone",
               stalagmites=14, stalactites=10, pools=2) {
  feature "spires" (kind=stalagmite, min_size=0.3, max_size=1.1)
}
```

Output (all nodes tagged `cave`):

```
hollow (cave wrapper)
├── rock                 mesh, role="cave_rock"
├── decorations          rock_piles/, pools/, lakes/, stalagmites/, stalactites/, columns/  (<kind>_<i>)
└── points_of_interest
```

POIs (`role=<kind>`, `tags=["cave", "poi", <kind>]`, no mesh/collider):

| role | placed at |
|---|---|
| `dead_end_chamber` | floor centre of each chamber with exactly one connection |
| `column_base` | floor anchor of each column |
| `ladder_anchor` | foot of each passage steeper than `max_slope` |
| `mushroom_spot` | random chamber-floor points (`mushrooms=`) |

Validation: negative counts/`loops`/`entrances`/`level_gap`/`level_links`
`E1202`; non-positive `size` component `E1203`; `max_slope` outside `(0, 90)`
`E1204`; non-positive `chambers`/`levels`/`chamber_min`/`chamber_max`/
`passage_radius`/`margin`/`resolution` `E1207`/`E1208`; `roughness`/
`chamber_flatten`/`overlap` outside `[0, 1]` `W1206` (clamped); `lod_scale`
outside `(0, 1]` `W1210`. On `feature`: body block `E1210`, missing/unknown
`kind` `E1211`/`E1212`, negative `count` `E1213`, non-positive sizes `E1214`.

### Terrain

`terrain` expands into a grid of cullable **chunk meshes** sampled from a noise
field normalised `0..1` and scaled by `size`'s height, then smoothed and
terraced. Chunk seams are crack-free. `sea_level` adds a flat water plane
without reshaping land. The subtree is frozen.

| attribute | default | effect |
|---|---|---|
| `seed` | `1` | RNG seed. |
| `size` | `[40, 6, 40]` | `[width, max_height, depth]` (m), centred on origin. |
| `source` | `fbm` | `fbm` (hills), `ridged` (ridges), `billow` (mounds), `island` (land falling to water at edges), `voronoi` (craters, cracked basins). |
| `octaves` | `4` | Noise octaves. |
| `frequency` | `0.06` | Base frequency. |
| `persistence` | `0.5` | Per-octave falloff `[0, 1]`. |
| `resolution` | `128` | Grid divisions (4–1024), rounded up to a multiple of `chunks`. |
| `chunks` | `4` | `chunks × chunks` meshes (1–16). |
| `lod_levels` | `3` | Render LODs per chunk (1–4); each level halves sampling. |
| `smooth` | `0` | Box-blur passes (0–32). |
| `terrace` | `0` | Stepped height bands (0 = off, max 64). |
| `sea_level` | `0.0` | Normalised waterline `[0, 1]`; adds water plane, enables shoreline POIs. |
| `peaks` | `0` | `peak` POIs on local maxima. |
| `flat_spots` | `0` | `flat_spot` POIs (flattest areas above sea level). |
| `shore_points` | `0` | `shoreline` POIs (needs `sea_level > 0`). |
| `colliders` | `none` | `all` = trimesh collider per chunk. |
| `lod_scale` | `1.0` | `[0.1, 1.0]`; scales resolution. POIs/sea level unchanged. |
| `mat` | `terrain_ground` | Ground material; default is white so the vertex colours show. A custom `mat=` tints them. |
| `debug_show_poi` | `0` | Bright `terrain_poi_<kind>` sphere per POI. |

Vertices carry baked `COLOR_0`: grass→rock by slope, plus (with `sea_level > 0`)
sand at the waterline fading to mud underwater. No engine shader needed.

POIs (under `points_of_interest`, `role`/`tags` in extras, spread apart):
`peak`, `flat_spot`, `shoreline`.

Each chunk emits `lod_levels` meshes `chunk_<i>_<j>_lod<n>` with
`extras.lod = { level, min_distance, max_distance }`; bands partition `[0, ∞)`
(maps to Godot `VisibilityRange`). Only level 0 has the collider and casts
shadows; coarser levels have a downward skirt to hide LOD cracks.

#### Carving children: `hole` and `road`

Only `hole` and `road` children are allowed. Both edit the shared height field,
so seams, skirts and POIs stay consistent.

**`hole`** — drops the surface inside a footprint with rim walls.

| attribute | default | effect |
|---|---|---|
| `at` | `[0, 0]` | Centre `[x, z]` (m, patch-local). |
| `radius` | — | Circular footprint (m). Give `radius` **or** `size`, not both. |
| `size` | — | Rectangular footprint `[w, d]` (m). |
| `depth` | `4` | Floor depth below rim (m). |
| `cap` | `open` | `open` = walls only, bottom open (for a basement/cave); `floor` = sealed watertight pit. |

**`road`** — flattens a corridor along a polyline to a smoothed profile at
terrain height (no trench) and tints it dirt/gravel in `COLOR_0`.

| attribute | default | effect |
|---|---|---|
| `path` | — | Required `[[x, z], …]` (≥ 2 points). |
| `width` | `3` | Flat corridor width (m). |
| `shoulder` | `width/2` | Blend distance outside the corridor (m). |

```
terrain "valley" (seed=2, size=[200, 12, 200], source="island", chunks=6,
                  sea_level=0.28, peaks=8, colliders="all") {
  hole ( at=[-30, -20], radius=14, depth=6, cap="floor" )
  road ( path=[[-90, -60], [-20, -10], [30, 20], [90, 70]], width=8, shoulder=5 )
}
```

---

### Dungeon

`dungeon` builds a **tile-grid** level: axis-aligned rooms on a `cell`-metre
lattice, straight/L-shaped corridors, `levels` floors linked by staircases.
Walls, floors and ceilings are box meshes; doorways are wall gaps; watertight by
construction. No body block. The subtree is frozen.

| attribute | default | effect |
|---|---|---|
| `seed` | `1` | RNG seed (`0` → `1`). |
| `size` | `[48, 4, 48]` | `[width, room_height, depth]` (m); width/depth ÷ `cell` = grid. |
| `cell` | `4` | Cell edge (m). |
| `levels` | `1` | Stacked floors. |
| `stairs` | `1` | Staircases per adjacent level pair. |
| `entrances` | `1` | Exterior doorways. Scalar = ground floor; array `[g, f1, …]` per floor (floors past the end get none). Prefers dead-end rooms and short border stubs. |
| `rooms` | `6` | Target rooms per level (best-effort). |
| `room_min`, `room_max` | `2`, `5` | Room side range in cells (swapped if reversed). |
| `spacing` | `1` | Min gap between rooms (cells). |
| `corridor_width` | `1` | Cells. |
| `loops` | `1` | Extra corridors per level beyond the spanning tree. |
| `wall_thickness` | `0.4` | m. |
| `floor_thickness` | `0.4` | Deck thickness (m). |
| `ceilings` | `1` | `0` = open-topped levels. |
| `prop_spots` | `0` | `prop_spot` POIs on room floors. |
| `colliders` | `all` | Trimesh on decks, walls, steps; `none` = visual only. |
| `lod_scale` | `1.0` | `[0.1, 1.0]`, compounds with global `lod_scale`; layout/POIs unchanged. |
| `debug_hide_roof` | `0` | Omit the top deck. |
| `debug_render_floor` | _unset_ | Render only this level (`0` = ground) without ceiling; out-of-range renders all. |
| `debug_show_poi` | `0` | Bright `dungeon_poi_<kind>` sphere per POI. |

Deck `k` is both floor of level `k` and ceiling of level `k-1`; every walkable
cell including stair landings is covered. Staircases cut the upper deck and fill
it with ~0.18 m risers. Each exterior doorway is a one-cell stub to the grid
border. Walls extend by `wall_thickness` at ends so corners close. Materials
`dungeon_floor`/`dungeon_stone` are auto-stamped when `mat=` is absent.

POIs (under `points_of_interest`): `entrance` (facing out), `spawn` (room behind
the entrance), `treasure_room` (other dead ends), `stair_top`/`stair_bottom`,
`prop_spot`.

```
dungeon "keep" (seed=7, size=[56, 4, 56], cell=4, levels=3, stairs=1,
                rooms=7, room_min=2, room_max=5, prop_spots=8)
```

---

## Materials

```
material "wood"  (color=[0.55, 0.35, 0.18], metallic=0.0, roughness=0.8)
material "glass" (color=[0.9, 0.95, 1.0], alpha=0.3, roughness=0.05, transmission=0.9)
material "neon"  (color=[1, 0.2, 1], emissive=[1, 0.2, 1], emissive_strength=8.0)
material "leaf"  (color=[0.25, 0.6, 0.2], alpha_mode="mask", alpha_cutoff=0.5)
```

Declared at top level or inside `scene`. Referenced via `mat="wood"`; unknown
names are an error.

- `color` — linear RGB vec3; alpha `1.0` unless `alpha` is set.
- `alpha` — transparency; `< 1` without `alpha_mode` selects `"blend"`.
- `metallic` — `0`–`1`, default `0.0`.
- `roughness` — `0`–`1`, default `0.9`.
- `alpha_mode` — `"opaque"` (default), `"blend"`, `"mask"` (cutout).
- `alpha_cutoff` — mask threshold, default `0.5`.
- `emissive` — vec3 glow, default `[0, 0, 0]`.
- `emissive_strength` — HDR multiplier (`KHR_materials_emissive_strength`), default `1.0`; `> 1` drives bloom.
- `transmission` — `0`–`1` (`KHR_materials_transmission`); `1` = clear glass. For glass/water; use `alpha` for tints/smoke.
- `double_sided` — `0` (default) / `1`. For leaves, cloth, thin planes seen from both sides.
- `uv_mode` — `"tile"` (default; world-space UVs, 1 unit = 1 tile) or `"fit"` (each face `[0,1]²`, image shown once).
- `uv_scale` — scalar or vec2, default `1.0`. Tile: tiles per unit. Fit: multiplies coords.
- `base_color_texture` — `.png`/`.jpg`/`.svg` path relative to the `.mog`; multiplies `color`; sRGB. See [SVG textures](#svg-textures).
- `metallic_roughness_texture` — green = roughness, blue = metallic; linear.
- `normal_texture` — tangent-space; linear.
- `occlusion_texture` — red channel; linear.
- `emissive_texture` — multiplies `emissive`; sRGB.
- `normal_strength` — slope multiplier for the normal map *derived* by `mogen textures`; `~0..8`, default `1.5`. Ignored if `normal_texture` is set.
- `occlusion_strength` — `0`–`1` max darkness of the *derived* AO, default `0.7`. Ignored if `occlusion_texture` is set.
- `texture_size` — `.svg` raster edge length in px, default `1024`.
- `texture_wrap` — `0` (default) / `1`: rasterize `.svg` as a seamless tile.
- `prompt` — subject hint for `mogen textures` albedo generation (e.g. `"navy nylon ripstop weave"`).
- `shader` — `"standard"`/`"pbr"` (default), built-in `"water"`, or a declared `shader` name (see [Shaders](#shaders)). Affects Studio preview only; GLB keeps standard PBR and carries the shader name + params in `node.extras.shader`. `"water"` reinterprets: `color` = body tint, `uv_scale` = ripple density, `roughness` = chop/foam (`0.05` glassy → `1.0` whitecaps), `metallic` → liquid-metal Fresnel, `transmission` + `alpha_mode="blend"` shows the floor, textures blend into the waves. Its param `absorption` (float, default `1.5`) is the depth over which transmitted light falls to `1/e`; `0` = clear.
- `gradient` — ramp baked into `COLOR_0`; colours are sRGB vec3:
  - `linear(from=[…], to=[…], axis=x|y|z)` — along a local axis (default `y`).
  - `vertical(from=[…], to=[…])` — `linear` on Y.
  - `radial(center=[…], edge=[…])` — centre-to-corner over the local AABB.
  - `stops(colors=[[…], …], positions=[…], axis=…, kind=linear|radial)` — `positions` default evenly spaced; `kind` default `linear`.

  `COLOR_0` multiplies `color`; use `color=[1, 1, 1]` for the raw ramp. Needs
  vertices to interpolate — raise tessellation (a default box shows a hard step).

```
material "oak" (color=[1, 1, 1], roughness=0.8,
  base_color_texture="textures/oak_albedo.png",
  normal_texture="textures/oak_normal.png")
material "lake" (color=[0.05, 0.32, 0.45], shader="water")
```

Textures are embedded in the GLB; a missing file is an export error.

### SVG textures

Any texture slot accepts `.svg`; it is rasterized to PNG at build time (glTF
has no vector image type), before export, FBX, preview, or derived-PBR steps.

```
material "tiles" (color=[1, 1, 1], uv_mode="fit",
  base_color_texture="textures/hex.svg", texture_size=2048, texture_wrap=1)
```

- `texture_size` (default `1024`) sets the square output; the viewBox is stretched to fill it (adjust aspect with `uv_scale`).
- Text is not supported — convert to paths. (No fonts, so output is identical on every machine.)
- Output is deterministic; the same SVG at two sizes embeds twice.
- `texture_wrap=1` draws the art on a 3×3 lattice and keeps the centre, so overhanging shapes reappear on the opposite edge. A translucent shape wider than the whole tile overlaps itself and darkens at the edges (still seamless).

A seamless tile doesn't fix seams from UV layout: sphere UVs pinch at the poles
and `uv_mode="tile"` wraps mid-tile for most radii. On spheres use a wrap tile
with `uv_mode="fit"`.

`.svg` textures combined with the `bundle_lods_and_imposter` export option are
an export error; pre-rasterize to `.png`.

---

## Shaders

`shader "<name>"` names a GLSL fragment snippet and its parameters. mogen never
compiles it; it writes path + resolved params to `node.extras.shader` on nodes
using the material. Studio compiles it for preview. Declaring
`shader "water"` shadows the built-in preset.

```
shader "ripple" (source="shaders/ripple.glsl") {
  param "speed" (type=float, default=2.0)
  param "tint"  (type=color, default=[0.0, 0.3, 0.5])
}

material "pond" (color=[0.1, 0.2, 0.3], shader="ripple") {
  shader_params (speed=3.5, tint=[0.0, 0.4, 0.6])
}
```

- `source` — GLSL path relative to the `.mog`.
- `param` `type` — `float`, `vec2`, `vec3`, `vec4`, or `color` (RGB vec3).
- `param` `default` — used when `shader_params` doesn't override; must match `type`.

Unknown `shader_params` names warn `W0108`. An undeclared, non-built-in shader
name is `E0106`. Shaders from imported files are not visible to the importer —
declare them where used.

---

## Physics

A `physics` substance is an object's *feel* (as `material` is its look). mogen
runs no simulation; it computes weight and centre of gravity from the mesh and
writes them to `node.extras.physics` for an engine to build a rigid body.

```
material "wood" (color=[0.55, 0.35, 0.18])
physics  "oak"  (weight=700kg/m3, friction=0.55, bounce=0.15)

scene {
  box "crate" (size=[1, 1, 1], mat="wood", phys="oak")   // → 700 kg
  box "prop"  (size=[1, 1, 1], phys="oak", weight=5kg)   // → exactly 5 kg
}
```

| attribute | value | default | meaning |
|---|---|---|---|
| `weight` | per cubic metre (`700kg/m3`) | `1000kg/m3` | density; `density=` is not accepted |
| `friction` | `0` (ice) … `1` (rubber), some engines allow `>1` | `0.5` | grip |
| `bounce` | `0` (dead) … `1` (superball) | `0.0` | restitution |

Substance names carry no built-in constants and are scoped to the declaring
file. `mat=` and `phys=` are independent.

- **weight** = `weight_per_m3 × enclosed mesh volume`, including world scale (`scale=2` ⇒ 8×). Node `weight=` overrides it.
- **centre of gravity** = volume centroid, node-local; always geometry-derived, even when `weight=` is overridden.
- Both are computed after attach, conform and skin binding, from final geometry and transforms.
- `phys=` **inherits** like `mat=`; a descendant's own `phys=` overrides.
- A node with a substance but no own mesh (e.g. `group`) reports the **total** weight and mass-weighted centre of gravity of its mesh descendants, so it can be one rigid body. A group `weight=` (even `0`) overrides its total but keeps the descendant-derived centre of gravity; it doesn't propagate to children, and enclosing compounds sum mesh-bearing descendants, not nested groups' overrides.

```mog
physics "oak" (weight=700kg/m3)
scene {
  group "assembly" (phys="oak", weight=5kg, pos=[10,20,30], rot=[0,0,90], scale=2) {
    group "parts" {                                  // 4 kg
      box "light" (size=[4,1,1], x=-1, weight=1kg)
      box "heavy" (size=[4,1,1], x=3, weight=3kg)
    }
  }
}
```

`assembly` weighs 5 kg with local centre of gravity `[2, 0, 0]`
(`(-1×1 + 3×3) / 4`); its own transform doesn't change that local point.
- Export mesh-merge keeps a combined body when all merged leaves share a substance; mixed substances drop physics.
- No inertia tensor is emitted. Collision shape comes from `collider=` or generator colliders.

```json
"extras": { "physics": {
  "material": "oak", "weight_per_m3": 700.0, "friction": 0.55, "bounce": 0.15,
  "weight": 700.0, "center_of_gravity": [0.0, 0.0, 0.0]
} }
```

`weight` is emitted when a computed mass or explicit override exists;
`center_of_gravity` only when a centroid was computed (meshless compound groups
included). An empty group with `phys="oak", weight=5kg` exports `weight: 5`
and no `center_of_gravity`.

Validation: `E0105` unknown substance; `E0214` unnamed `physics`;
`W0211`/`W0212`/`W0213` non-positive `weight`, negative `friction`, `bounce`
outside `[0, 1]`; `W0102` unknown attribute (including `density=`); `W0215`
node `weight=` without `phys=` on or inherited by the node (value would be
dropped).

---

## Decals

A `decal` is a transparent image projected onto a surface: a thin double-sided
quad with an auto-generated `alpha_mode="blend"` material using an RGBA PNG.

```
decal "logo" (
  pos = [0, 0.1, 0.101],
  size = [0.25, 0.12],
  prompt = "embroidered MoGen logo, white thread on dark fabric"
)
```

- `size` — `[w, h]`, default `[0.5, 0.5]`. XY quad facing local +Z; orient with `rot=`/`rx`/`ry`/`rz`.
- `prompt` — image description for `mogen textures` (transparent-background RGBA, no chroma key). The result path is written back as `image="…"`. Defaults to the decal's name when neither `prompt` nor `image` is set.
- `image` — existing RGBA PNG path; wins over `prompt`.
- `tint` — vec3 multiplier, default `[1, 1, 1]`.
- `roughness` — default `0.6`.
- `offset` — +Z gap from the surface, default `0.001`.

`mat=` is rejected. Each decal gets its own material `__decal_<name>` and is
never merged by the export merge pass.

### Curved surfaces: `on=` / `at=`

On flat surfaces, parent the decal and use `pos=`. On curved ones, `on=`/`at=`
generate a `conform` patch:

```
ellipsoid "bag" (size=[1.0, 0.5, 0.5], mat="leather") {
  connector "front_spot" (at=[0.0, 0.0, 0.25], dir=[0, 0, 1])
}
decal "bag_logo" (size=[0.18, 0.10], on="bag", at="front_spot",
                  prompt="embroidered MoGen wordmark, cream thread on dark leather")
```

- `on` — target node.
- `at` — required with `on`; anchor connector on the target.
- `up` — `x|y|z` axis along the surface normal, default `z`.
- `lift` — extra outward offset on top of `offset=`, default `0`.

`at=`/`up=`/`lift=` without `on=` are errors. With `on=`, the decal is
reparented under the target and `pos=` ignored; `rot=`/`scale=` apply in the
tangent plane (`rz=90` spins the logo, `scale=[2,1,1]` widens it).

---

## Connectors

Oriented frames a node exposes for attachment; no geometry.

```
box "seat" (pos=[0, 0.5, 0], size=[1.0, 0.1, 1.0]) {
  connector "top"    (at=[0,  0.05, 0], dir=[0,  1, 0], tag=seat_top)
  connector "bottom" (at=[0, -0.05, 0], dir=[0, -1, 0], tag=seat_bottom)
}
```

| attribute | value | default |
|---|---|---|
| `at` | `vec3` | `[0, 0, 0]` |
| `dir` | nonzero `vec3` | `[0, 1, 0]` |
| `tag` | string/ident | empty |
| `radius` | number | unset |

Stored as position + quaternion rotating `+Y` onto `dir`. `tag` groups
compatible attach points.

---

## Attach: rigid alignment of two connector frames

Aligns the child's `plug` with the parent's `socket` and reparents the child.

```
scene {
  cylinder "trunk"  (radius=0.2, height=1.0) {
    connector "top" (at=[0, 0.5, 0], dir=[0, 1, 0])
  }
  sphere   "canopy" (radius=0.5) {
    connector "stem" (at=[0, -0.5, 0], dir=[0, -1, 0])
  }
  attach (parent="trunk", child="canopy", socket="top", plug="stem")
}
```

| attribute | required | default | effect |
|---|---|---|---|
| `parent` | yes | — | node with the `socket` |
| `child` | yes | — | node with the `plug` |
| `socket` | no | `"top"` | connector on `parent` |
| `plug` | no | `"bottom"` | connector on `child` |
| `offset` | no | `0.0` | gap (m) along the socket direction |
| `twist` | no | `0.0` | roll (deg) around the socket axis |

The child's own `pos=`/`rot=` are applied as an additional offset after
alignment. Inside `array`/`mirror`, `attach` applies to every copy.

---

## Conform: moulding a primitive onto a target surface

`conform` deforms a child's vertices onto a target mesh surface (where `attach`
only aligns rigidly). Mode is chosen by attrs; mixing or omitting both is an
error.

- **Path mode (`from=`/`to=`)** — strip or tube stretched between two connectors (zips, trim, hoses).
- **Patch mode (`at=`)** — flat child bent onto the surface around one connector (pockets, decals, plates).

### Shared attributes

| attribute | required | default | effect |
|---|---|---|---|
| `target` | yes | — | surface mesh node |
| `child` | yes | — | primitive to deform |
| `lift` | no | `0.0` | outward offset along the normal (m) |
| `reparent` | no | `1` | `1`: child moves under target with identity transform (its `pos=`/`rot=` discarded). `0`: keeps its parent; mesh is transformed back into its frame |

### Path mode — `from=` / `to=`

Each vertex's `along` coordinate maps to a position on the path; the other
axes lie tangent/normal to the surface. The path is chord-interpolated then
snapped to the surface (not a true geodesic), so raise `samples` on
high-curvature targets.

| attribute | required | default | effect |
|---|---|---|---|
| `from` | yes | — | start connector on `target` |
| `to` | yes | — | end connector on `target` |
| `along` | no | `x` (strips) / `y` (tubes) | child path axis |
| `width` | no | inferred | child axis tangent to surface |
| `height` | no | inferred | child axis along the normal |
| `samples` | no | `64` | path subdivisions (≥ 2) |
| `twist` | no | `0` | roll (deg) ramped from start to end |

Compatible: strips `box`, `plane`, `quad`, `curved_plane`, `slab`, `post`,
`panel`, `wall`, `spline_ribbon`; tubes `cylinder`, `capsule`, `tube`,
`spline_tube`; `mesh` (requires `along=`).

```
scene {
  ellipsoid "bag" (size=[1.0, 0.5, 0.5], mat="leather") {
    connector "zip_a" (at=[-0.4, 0.20, 0.22], dir=[0, 0, 1])
    connector "zip_b" (at=[ 0.4, 0.20, 0.22], dir=[0, 0, 1])
  }
  box "zip" (size=[0.8, 0.012, 0.04], mat="rubber")
  conform (target="bag", child="zip", from="zip_a", to="zip_b", along=x, lift=0.005)
}
```

Tubes use `along=y`; wrap-around labels use `along=x` on a `curved_plane`.

### Patch mode — `at=`

Each vertex snaps to its closest surface point; the child's `up` axis follows
the surface normal.

| attribute | required | default | effect |
|---|---|---|---|
| `at` | yes | — | patch-centre connector on `target` |
| `up` | no | `y` / `z` for `quad`, `leaf_card` | child axis aligned with the normal |

Compatible: `disc`, `plane`, `quad`, `curved_plane`, `leaf_card`; thin `box`,
`slab`, `panel`, `wall`; `cylinder`, `hemisphere`, `half_cylinder`; `mesh`
(requires `up=`).

```
conform (target="body", child="pocket", at="spot", lift=0.002)
```

### Rejected primitives

`sphere`, `ellipsoid`, `icosphere`, `torus`, `torus_arc`, `superellipsoid`,
`pyramid`, `cone`, `frustum`, `lathe`, `prism`, `rounded_box`, `wedge`, and CSG
results are rejected in both modes (the error suggests the other mode when it
would work).

### Tessellation

Children coarser than `samples / 4` segments along `along` get planar cuts
inserted automatically (clamped 8–64), so a bare `box` can bend. Explicit
subdivision (`segments_u=48`, …) overrides.

### Pass ordering and reparenting

Conform runs after `attach` and before skin binding.

### Reserved (not yet implemented)

Rejected with an error: `direction` (projection splat), `curve` (only
`"geodesic_lerp"`), `via` (multi-segment paths).

---

## Replicators: `mirror`, `array`, `stack`, `grid`

Wrapper nodes that replicate or lay out children; all accept transform attrs.

### `mirror`

```
mirror "pair" (axis=x) {
  sphere "ball" (pos=[0.5, 0.5, 0], radius=0.25)
}
```

`axis` = `x`/`y`/`z`. Emits the body unchanged plus a copy negated on the axis.
Both copies keep the body's names and both bind when skinned.

#### `flip_bind=1`: rebind the mirrored copy to the symmetric bone

```
mirror "sleeves" (axis=x, flip_bind=1) {
  chamfered_box "sleeve" (pos=[0.135, 0.695, 0], size=[0.052, 0.158, 0.052],
                          skin="rig", bind="shoulder_l")
}
```

The mirrored copy swaps a trailing `_l`/`_r` on the resolved `bind=` of every
mesh descendant (local or inherited). Binds without the suffix are unchanged.
Default `0`. Useful because module parameters can't be strings.

### `array`

```
array "legs" (count=4, around=y) {
  group "offset" (pos=[0.45, 0, 0.45]) {
    cylinder "leg" (radius=0.05, height=0.5)
  }
}
```

- `count` — copies, default `1`.
- `around` — `x`/`y`/`z`, default `y`.
- `start_angle` — degrees, default `0`.

Copy *i* is rotated `start_angle + 360° * i / count`. Offset with a `group`.

### `stack`

Lays children along an axis using each child's AABB.

```
stack "cake" (axis=y, gap=0.02) {
  slab "tier_a" (size=[1.4, 0.25, 1.4])
  slab "tier_b" (size=[1.0, 0.20, 1.0])
}
```

| attribute | value | default | effect |
|---|---|---|---|
| `axis` | `x`/`y`/`z` | `y` | direction |
| `gap` | number | `0` | spacing |
| `align` | `center`/`start`/`end` | `center` | on perpendicular axes |
| `pack` | `start`/`center`/`end` | `start` | `start`: first child at origin; `center`: stack centred; `end`: last child's far face at origin |

Children's `pos`/`x`/`y`/`z` are added within their slot.

### `grid`

`count[0] × count[1] × count[2]` copies offset by `step * [i, j, k]`.

```
grid "tiles" (count=[5, 1, 3], step=[0.6, 0, 0.6], center=1) {
  slab "tile" (size=[0.55, 0.05, 0.55])
}
```

| attribute | value | default |
|---|---|---|
| `count` | vec3, list, or scalar | `[1, 1, 1]` |
| `step` | vec3, list, or scalar | `[0, 0, 0]` |
| `center` | `0`/`1` | `0`; `1` centres on the origin |

Scalar = X; 2-element list = X/Z; vec3 = XYZ.

---

## CSG: `union` / `difference` / `intersect`

Fold children into one mesh on the op node; operands don't become scene nodes.

```
difference "wall_with_door" (mat="concrete") {
  box "wall"    (size=[4.0, 3.0, 0.2])
  box "doorway" (pos=[0, -0.5, 0], size=[0.9, 2.0, 0.5])
}
```

- `union` — N ≥ 1. `smooth=<radius>` blends instead of hard union (a few cm at human scale); default `0`.
- `difference` — first operand minus the rest.
- `intersect` — N ≥ 2; shared volume.

Operand transforms are baked in. Connectors/materials on the CSG node apply;
those on operands are ignored. Output is watertight.

---

## Implicit-field blob: `blob`

Meshes children as a blended SDF. Unlike `metaball`/`union(smooth=)`, it
handles **concave junctions** and large blend radii (eye sockets, a heart's
cleft, nostrils).

```
blob "heart" (blend=0.22, resolution=80, subdivide=1, mat="heart") {
  sphere    (radius=0.45, pos=[-0.30, 0.32, 0])
  sphere    (radius=0.45, pos=[ 0.30, 0.32, 0])
  ellipsoid (size=[0.85, 1.00, 0.85], pos=[0, -0.25, 0])
  sphere    (radius=0.20, pos=[0, 0.58, 0], op=subtract)   // cleft
}
```

### Attributes

- `blend` — smooth-min radius; `0` = sharp. Typical `0.05–0.25`.
- `resolution` — voxels along the longest axis, default `96`, clamped `[16, 256]`; cost O(N³). Scales with `lod_scale`/`lod=`.
- `subdivide` — Loop subdivision rounds, default `0`; `1` removes voxel stairstepping, `2` is glassy (4× tris). Steps by `round(log2(lod_scale))`, max 3.
- `mat`, `pos`, `rot`, `scale`, `role`, `tags` — as usual.

### Children

| kind | attrs | SDF |
|---|---|---|
| `sphere` | `radius` | sphere |
| `ellipsoid` | `size` | radii `size/2` |
| `box` | `size` | box |
| `rounded_box` | `size`, `radius` | rounded box |
| `capsule` | `radius`, `height` | Y-axis, caps at `±height/2 + radius` |
| `cylinder` | `radius`, `height` | Y-axis, closed |
| `torus` | `major`, `minor` | XZ plane |

Children take `pos`/`rot`/`scale` (non-uniform scale allowed but conservative;
prefer `ellipsoid`). `op="subtract"` (or `"sub"`, `"carve"`) makes a smooth
carver; at least one additive child is required.

### Notes

- One mesh, one node, always watertight.
- UVs are XZ planar projection — use `uv_mode="tile"`.
- Examples: `examples/nature/skull.mog`, `examples/nature/heart.mog`.

---

## Mesh subdivision: `subdivide`

`subdivide = N` on any mesh-producing node (primitive, CSG op, `blob`) applies
N rounds of Loop subdivision (each ×4 triangles), clamped `[0, 3]`. No-op on
`group`, `scene`, `mirror`, `array`, `stack`, `grid`.

```
union (subdivide=1, mat="bone") {
  sphere    (radius=0.5)
  ellipsoid (size=[0.6, 0.4, 0.8], pos=[0, -0.3, 0])
}
```

`icosphere(radius=1, subdivide=2)` is often smoother than `subdivisions=4`.

---

## Solid groups: `solid`

A group whose same-material, non-skinned leaf children are CSG-unioned into one
mesh at export, removing interior faces.

```
solid "shell" (mat="stone", cleanup="coplanar") {
  box "floor" (pos=[0, 0.1, 0],   size=[6.2, 0.2, 4.2])
  box "north" (pos=[0, 1.7, 2.0], size=[6.0, 3.0, 0.2])
  box "east"  (pos=[3.0, 1.7, 0], size=[0.2, 3.0, 4.0])
}
```

- Children remain normal, editable, `attach`-able nodes until export.
- Different-material children stay separate.
- Skinned meshes, joint-referenced nodes, and groups pass through.

### `cleanup="coplanar"`

Removes coplanar opposite-facing triangle pairs — fixes boxes that touch without
overlapping, which union can't. Values `"coplanar"` / `"none"` (default).

---

## Modules: `module` and `use`

Parametric sub-graphs declared at top level:

```
module "leg" (height=0.5, radius=0.05) {
  cylinder "leg" (pos=[0, $height * 0.5, 0],
                  radius=$radius, height=$height, mat="wood") {
    connector "top" (at=[0, $height * 0.5, 0], dir=[0, 1, 0], tag=leg_top)
  }
}

scene {
  use "leg" (height=0.6)
}
```

- Parameter defaults: scalars (number or `$param` expression) or fully constant vec3s. List/string/ident defaults are rejected.
- Reference as `$name`, including inside vec3s.
- `use` takes the declared name; unknown module or arg names error; omitted args use defaults.
- Modules may call modules; recursion is rejected. `$param` outside a module is rejected.

---

## Control flow: `if`, `else`, `for`, string interpolation

Evaluated at expansion time inside any `{ … }` body.

### `if (cond=…)` and `else`

`cond` is any expression (comparisons yield `1.0`/`0.0`). `else { … }` must
immediately follow an `if` sibling.

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
for (var="i", from=0, to=3) {
  cylinder "leg_$i" (radius=0.05, height=0.6, pos=[$i * 0.4, 0, 0])
}
// leg_0, leg_1, leg_2
```

- `var` — string or ident; read as `$<var>`.
- Range `[from, to)`; `from == to` is empty.
- `step` — default `1.0`, non-zero; negative counts down.
- Nests, and coexists with module parameters.

### String interpolation

`$name`/`${name}` in any string (including node names) is substituted. Integers
render without decimals (`leg_3`). Use `${name}` before `_` etc. Unbound or bare
`$` stays literal.

No chained comparisons, `&&`/`||`/`!` (use `*` for AND, `+` for OR), or string
concatenation.

---

## Imports: `import`

Imports `module` and `material` declarations and the top-level `scene` of
another file. The scene becomes a module named after the file stem.

```
import "shared/legs.mog"
import "objects/chair.mog"

scene {
  use "leg"   (h=0.6)
  use "chair" (pos=[ 1, 0, 0])
  use "chair" (pos=[-1, 0, 0], rot=[0, 180, 0])
}
```

- Top-level directive; `(as=<ident>)` overrides the module name.
- Paths are relative to the importing file (absolute used as-is) and canonicalised, so each file loads once.
- Material texture paths resolve relative to the defining file.
- `use` accepts `pos`, `rot`, `scale`, `x`/`y`/`z`, `rx`/`ry`/`rz`, `from`/`to` as a wrapping group — unless the module declares a parameter of that name.
- Imports are transitive; cycles error with the chain; re-imports are no-ops.
- Shadowing: **stdlib < imports < user declarations**.
- Scene-module name collisions error (rename with `as=`).
- Same-named materials in different files stay distinct: geometry resolves materials in its defining file first, then falls back to a name lookup.
- Imported files may contain `import`, `module`, `material`, `lod_scale` (kept as that file's default, see [Per-node LOD](#per-node-lod-overrides)), `meta` (stays with its file), at most one `scene`, and animation/skeleton declarations (these need a scene; they join its module and apply when instantiated).

---

## Animation: `joint`, `clip`, templates

Clips become glTF node-transform tracks. `joint`s and clips are top-level
declarations. Skinning is separate (see [Skeletons](#skeletons-and-skinning-skeleton-bone-skin-bind)).

### Joints

```
joint "door_hinge" (type=hinge, axis=[0, 1, 0], limits=[0, 100], pivot="door")
```

| attribute | value | notes |
|---|---|---|
| `type` | `hinge`, `slider`, `ball`, `rotor` | DOF kind |
| `pivot` | node name | required |
| `axis` | vec3 | default `[0, 1, 0]` |
| `limits` | `[lo, hi]` | optional; degrees (rotary) or metres (slider) |

### Authored clips

```
clip "open" (seconds=1.0) {
  track "door_hinge" (from=0, to=90)
}
```

- `seconds` — duration.
- `track` targets a joint or scene node; for nodes set `prop="translation"|"rotation"|"scale"` (`pos`/`rot` aliases).
- `from`/`to` — degrees (around the joint `axis` or track `axis=`), distance, or scale factor; keys at `0` and `seconds`.
- `keys=[[t, v], …]` — multiple keys, strictly ascending in `[0, seconds]`, linear between.

### Easing

`easing=` on tracks (between keys) and templates (warps phase). Baked into
dense LINEAR samples.

| name | shape |
|---|---|
| `linear` (default) | t |
| `ease_in` / `ease_out` / `ease_in_out` | quadratic |
| `ease_in_cubic` / `ease_out_cubic` / `ease_in_out_cubic` | cubic |
| `ease_in_sine` / `ease_out_sine` / `ease_in_out_sine` | half-cosine |
| `ease_in_back` / `ease_out_back` / `ease_in_out_back` | overshoot |
| `ease_in_bounce` / `ease_out_bounce` / `ease_in_out_bounce` | bounce |

```
clip "hop" (seconds=0.6) {
  track "body" (prop=translation, axis=[0, 1, 0], easing=ease_out_bounce, from=0, to=0.4)
}
```

### Procedural templates

One-line clips; `target="name"` is a joint or node. Joint targets default to the
joint's `axis`; node targets need `axis`.

| template | extra attrs | effect |
|---|---|---|
| `spin` | `axis`, `rpm` (60), `easing` | continuous rotation |
| `open_close` | `axis`, `angle` (90), `seconds` (1.0), `easing` | 0° → angle → 0° |
| `wave` | `axis`, `amplitude` (15°), `hz` (1.0), `easing` | sinusoidal wobble |
| `flap` | `axis`, `amplitude` (30°), `hz` (2.0), `easing` | faster, bigger wobble |
| `idle` | `amplitude` (0.02 m), `hz` (0.5), `easing` | small translation breathe |

```
spin "rotor_spin" (target="rotor", axis=[0, 0, 1], rpm=30)
open_close "door_swing" (target="door_hinge", angle=90, seconds=1.2)
```

---

## Skeletons and skinning: `skeleton`, `bone`, `skin=`, `bind=`

A `skeleton` of nested `bone` nodes produces a glTF skin (joints in depth-first
order). Inverse bind matrices are computed from bone world transforms.

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
  capsule "torso" (pos=[0, 1.25, 0], radius=0.18, height=0.6, mat="cloth", skin="rig")
  sphere  "head"  (pos=[0, 1.7, 0], radius=0.12, mat="skin", skin="rig", bind="neck")
}
```

### `skeleton` and `bone`

- `skeleton "name" { … }` — children must be `bone`; top-level or in `scene`; no geometry.
- `bone "name" (pos, rot, scale, envelope)` — parent-relative transform. `envelope` (default `0.75`) is the auto-weight influence radius; neighbouring envelopes should overlap.

### Binding meshes: `skin="rig"`

A mesh node with `skin="<skeleton>"` gets automatic weights from the four
nearest bones within their envelopes (`JOINTS_0`/`WEIGHTS_0`). `group`, `solid`,
`stack`, `grid`, `array`, `mirror`, `module`, `use` pass `skin=` to mesh
descendants. `skin=` on a CSG operand is lost; put it on the CSG node or a
wrapper.

### Rigid pinning: `bind="bone_name"`

With `skin=`, `bind="bone"` weights every vertex 1.0 to that bone (heads,
helmets, props). Propagates like `skin=`. For `_l`/`_r` pairs see
[`mirror`](#mirror) `flip_bind=1`.

### Animating bones

Bones are scene nodes; animate with `clip`/`track`, e.g.
`track "thigh_l" (prop=rotation, axis=[1, 0, 0], keys=[…])`. Stdlib
`humanoid_walk`/`humanoid_run`/`humanoid_idle`/`humanoid_jump` target
`humanoid_full`'s bones.

---

## Lights: `light`

Exported via `KHR_lights_punctual`. Transform-only, no children; shines along
local `-Z`.

```
light "sun"  (kind=directional, dir=[-0.4, -1, -0.3], color=[1, 0.95, 0.85], intensity=3)
light "lamp" (kind=point, pos=[0, 2, 0], intensity=10, range=8)
light "spot" (kind=spot,  pos=[0, 3, 0], dir=[0, -1, 0], intensity=20,
              range=10, inner_cone=20, outer_cone=35)
```

| attribute | value | notes |
|---|---|---|
| `kind` | `directional`, `point`, `spot` | required |
| `color` | vec3 | linear RGB, default `[1, 1, 1]` |
| `intensity` | number | **candela** (point/spot), **lux** (directional); default `1.0` |
| `range` | number | point/spot cutoff; rejected on directional |
| `dir` | vec3 | aims `-Z` along `dir` (overrides `rot=`) |
| `inner_cone` | degrees | spot only, default `0` |
| `outer_cone` | degrees | spot only, default `45` |

Only transforms, `role`, `tags` apply (no `mat`, `anchor`, `from`/`to`, relative
placement). No ambient light is exported; use a dim directional fill.

---

## Colliders

`collider="aabb"` on any geometry, `group`, `solid`, or `use` marks a collision
box from the node's subtree mesh extents (node-local, after
attach/conform/skinning). `"aabb"` is the only value; mesh-less subtrees get
none.

```
slab "floor" (size=[18.4, 0.1, 10.4], mat="wood", collider="aabb")
use  "desk"  (pos=[0, 0, 0.4], collider="aabb")
```

Exported as `node.extras.collider = { type, min, max }`. Studio shows them via
**View → Show Colliders**.

---

## Shadow casting

Nodes cast shadows by default. `cast_shadow=0` opts a node and its whole
subtree out (descendants can't re-enable). Exported as `extras.cast_shadow=false`
on opted-out nodes only.

```
plane "ground" (size=20, mat="grass", cast_shadow=0)
```

---

## Diagnostics and tooling

- `mogen check <file>.mog [--json]` — validate without building; JSON is the format the LLM repair loop consumes.
- `mogen dump-scene <file>.mog --json` — print the lowered scene graph.
- `mogen inspect <file>.glb` — summarise a GLB.

---

## Scalar arrays and coordinate lists

`radii`, `widths`, sweep `roll`/`scale_along`, and loft `heights` accept numeric
arrays of any length, including exactly three (the attribute decides whether a
three-number literal is a vector or a scalar array). `pos`, `rot`, `size` and
coordinate/profile lists keep their dimensional rules.

- `radii`, `widths`, `roll`, `scale_along`: one constant or exactly one value per control point, interpolated linearly along the spline parameter (not arc length). Radii/widths/scale are finite and ≥ 0 (zero tapers an endpoint); roll is degrees, may be negative.
- Loft `heights`: at least two finite section heights; section points are flat-packed in the same order.
- Expressions and module params inside arrays resolve before these checks.
- Errors: `E0112` empty/mismatched count (reports expected vs actual), `E0113` invalid value, `E0114` malformed coordinate dimensions. Mismatched arrays are no longer silently truncated/repeated.

Example: [three_value_arrays.mog](../examples/features/three_value_arrays.mog).

## Explicit sweep frames

`frame_up=[0,1,0]` points local profile height toward +Y; width is height × tangent. `closed=1` uses a periodic path with matching seam frames. Omitting `frame_up` keeps the legacy orientation. See [sweep-frames.md](sweep-frames.md).

## Relational modeling

`relate` adds endpoint anchoring, connector-plane grounding and translation-only connector alignment on top of `attach`/`conform`. See [relational-modeling.md](relational-modeling.md) and [relational_chair.mog](../examples/furniture/relational_chair.mog).

## Surface-derived details

Declare `guide "welt" (target="cushion", section="latitude", level=0, tolerance=0.001)` and reuse it with `sweep (guide="welt", profile=[...], lift=0.002)`. Curved-frame trim uses `section="profile_edge", edge=...`. See [surface-guides.md](surface-guides.md).

### Measuring dimensions and fit

`mogen inspect model.mog` reports world bounds, dimensions, ground clearance and
resolved relationship checks. `mogen measure model.mog --first foot --second leg`
reports bounded surface distance, closest points and evidence in metres. See
[fit-measurements.md](fit-measurements.md).
