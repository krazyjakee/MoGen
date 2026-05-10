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
- [Decals](#decals)
- [Connectors](#connectors)
- [Attach: rigid alignment of two connector frames](#attach-rigid-alignment-of-two-connector-frames)
- [Conform: moulding a primitive onto a target surface](#conform-moulding-a-primitive-onto-a-target-surface)
- [Replicators: `mirror`, `array`, `stack`, `grid`](#replicators-mirror-array-stack-grid)
- [CSG: `union` / `difference` / `intersect`](#csg-union--difference--intersect)
- [Solid groups: `solid`](#solid-groups-solid)
- [Modules: `module` and `use`](#modules-module-and-use)
- [Imports: `import`](#imports-import)
- [Animation: `joint`, `clip`, templates](#animation-joint-clip-templates)
- [Skeletons and skinning: `skeleton`, `bone`, `skin=`, `bind=`](#skeletons-and-skinning-skeleton-bone-skin-bind)
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
| `version` | string | author | author-controlled (semver-style is conventional but not enforced) |
| `mogen_version` | string | **toolchain** | auto-stamped from the running mogen version on every save (`mogen generate`/`modify`/`animate`/`repair`/`textures` and Studio Save). Don't write it yourself. |
| `description` | string | author | one-line summary |
| `tags` | list of string | author | free-form labels |
| `style` | string | toolchain | visual-style hint stamped by `mogen generate --style …` (or Studio's "New from Prompt" dropdown). One of `ps1`, `n64`, `low_poly`, `high_detail`, `arcade`, `voxel`, `cel_shaded`, `stylized_fantasy`, `cyberpunk`, `pixel_art`. The validator accepts any string so hand-edited experimental keys still load; `mogen` and Studio inherit the value on `modify` / `animate` / `repair` so styled files stay styled. |
| `seed` | string | toolchain | random seed stamped by `mogen generate` / `modify` so rebuilds are reproducible. Round-trips through edits. |
| `thinking` | string | toolchain | per-file Gemini thinking budget stamped alongside `seed`; survives `modify` so the original budget keeps applying. |
| `prompt` | string | toolchain | original natural-language prompt that produced the file; round-trips through `modify` so the LLM can revise against the source intent. |
| `moghub_model_id` | string | toolchain | stamped by Studio's Publish dialog after a successful MoGHub upload; reused on subsequent publishes to republish into the same model. |
| `moghub_slug` | string | toolchain | MoGHub slug for the published model — paired with `moghub_model_id`. |
| `moghub_version` | string | toolchain | last-published MoGHub version string — paired with `moghub_model_id`. |

The block is purely informational — it's not consumed by the geometry
pipeline. It survives lowering on `SceneGraph::meta` so tooling (Studio,
exporters) can read it without re-parsing. Old files without a `meta` block
keep building; on the next save the toolchain inserts a fresh
`meta (mogen_version = "...")` line.

Diagnostic codes for `meta`:

- `E0310` — `meta` cannot have a `{ … }` body block.
- `E0311` — `meta` cannot take a quoted name (`meta "x" (...)`); use `name=` instead.
- `E0312` — duplicate `meta` block.
- `E0313` — `meta` only allowed at the top level.
- `W0107` — file's `mogen_version` doesn't match the running toolchain (will be re-stamped on next save).

---

## Global settings

Top-level directives that tune the build itself rather than describing
geometry. They sit at the file level (alongside `material` / `module`) and are
consumed during lowering.

| directive | value | effect |
|---|---|---|
| `lod_scale (value=N)` | number, default `1.0` | multiplies every primitive `segments` / `rings` / `samples` count — both the implicit defaults *and* author-supplied explicit values like `segments_u=64`. `0.5` halves them, `2.0` doubles them. `icosphere` `subdivisions` step by `round(log2(N))` instead, since each step quadruples its triangle count. Counts are clamped to a per-primitive minimum so circles still close. |

```
lod_scale (value=0.5)

scene {
  sphere "head" (radius=0.5)         // 8 rings, 12 segments (default 16/24 halved)
  sphere "lod0" (radius=0.5, segments=48, rings=32)  // 16 rings, 24 segments (48/32 halved)
}
```

The studio's "LOD scale" slider (under the build summary) edits this directive
in place — drag it down to iterate quickly on big scenes, then drag back to
`1.0` for export. The slider clears the directive when it returns to `1.0` so
saved files stay clean by default.

### Per-node `lod=` overrides

Any geometry/group node accepts a `lod=N` attribute that **multiplies**
the active LOD scale for the duration of that node and its subtree. Use
it to mark hero parts (`lod=2`) and background filler (`lod=0.5`) without
touching the file-global `lod_scale`. The override is RAII-scoped — it
does not leak into siblings — and compounds with the global setting:
`lod=2` on top of `lod_scale (value=0.5)` yields an effective multiplier
of `1.0` for that subtree.

```
scene {
  group "hero" (lod=2) {                  // boosted detail
    sphere "face" (radius=0.12)
  }
  sphere "background_rock" (radius=0.4, lod=0.5)  // halved detail
  sphere "default_rock" (radius=0.4)              // baseline
}
```

`lod=` scales every tessellation count in the marked subtree, including
explicit per-primitive `segments=` / `rings=` / `subdivisions=`. Use it
to dial detail on dense surfaces (heightfields, curved planes) without
having to drop their explicit segment counts. The minimum per-primitive
floor still applies, so a `lod=0.1` won't collapse a cylinder to fewer
than three sides.

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
| comparison | `$h > 0`, `$count == 4`, `$a != $b` | `<`, `<=`, `>`, `>=`, `==`, `!=` — used in `if` conditions and `for` ranges |

Inside `module` bodies, any expression may reference a declared parameter as
`$name`. Expressions support `+ - * /` with conventional precedence and
parentheses, plus the six comparison operators above for control-flow
conditions. An expression is evaluated at module-expansion time — by the
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

Common combinations:

```
// Asteroid / rock — coherent bumps + per-vertex jitter + flat shading.
icosphere "rock"   (radius=0.4, noise=0.30, jitter=0.15, faceted=1, seed=7)
// Bent timber / pipe — single-axis bend with light surface noise.
cylinder  "post"   (radius=0.05, height=2.0, bend_z=12, noise=0.04)
// Melted wax / jelly — gravity-sagged top with soft surface texture.
cylinder  "candle" (radius=0.18, height=0.7, droop=0.4, noise=0.05)
// Twisted, tapered beam — combine deterministic deformations freely.
box       "beam"   (size=[0.2, 0.2, 3], twist_y=20, taper=0.7)
```

Stochastic modifiers are deterministic for a given `seed` so rebuilds are
reproducible. Two unnamed primitives with `noise=0.3` and no `seed` share the
same `seed` default (1) and therefore the same surface — set distinct seeds
when you want sibling rocks to differ.

Each modifier accepts an optional `*_range=[a, b]` (`bend_x_range`,
`bend_y_range`, `bend_z_range`, `twist_y_range`, `taper_range`,
`droop_range`, `noise_range`, `jitter_range`) that gates the deformation
to a normalised slice along its length axis. Vertices below `a` are
unchanged, vertices inside `[a, b]` ramp in via smoothstep, and vertices
above `b` get the full effect. Use it to bend the tip but not the base of
a sword, twist only the upper half of a tower, or jitter just the top
third of a column. Endpoints can be in either order; `[1.0, 0.5]` is
normalised to `[0.5, 1.0]` automatically.

```
// Sword that bows toward its tip but keeps a straight grip.
box "blade" (size=[0.06, 1.4, 0.01], bend_z=18, bend_z_range=[0.55, 1.0])
// Tower that twists only above the cornice.
cylinder "spire" (radius=0.4, height=4.0, twist_y=70, twist_y_range=[0.6, 1.0])
// Cliff face that's smooth at the foot, jagged at the crown.
box "cliff" (size=[3, 4, 1], jitter=0.4, jitter_range=[0.5, 1.0], seed=11)
```
Default tessellation auto-bumps (×2 segments / +1 icosphere subdivision) when
a smooth deformer (`bend_*`, `twist_y`, `noise`, `droop`) is present so a
bent cylinder doesn't read as faceted. Author's explicit `segments=`,
`rings=`, `subdivisions=` always override.

Modifiers are not applied to `mesh` (loaded glb) primitives — their joints,
UVs, and skinning contract are wider than what the deform pass preserves.

See `examples/asteroid_field.mog` for a runnable showcase.

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
| `chamfered_box` | `size=[x,y,z]` | `radius` (bevel offset, 0.1) — sharp 45° bevels on all 12 edges + 8 corner triangles. Use for hard-edged industrial parts where a fully-rounded `rounded_box` reads as too organic |
| `inset_box` | `size=[x,y,z]` | `face` (`"+y"\|"-y"\|"+x"\|"-x"\|"+z"\|"-z"` or `"top"/"bottom"/"left"/"right"/"front"/"back"`, default `"+y"`), `amount` (inset distance, 0.1), `depth` (sink depth, 0.05) — five plain box faces + one sunken panel; window frames, recessed door panels, button caps, sunken pickup wells |
| `wedge` | `size=[x,y,z]` | right-triangle prism — flat bottom on -Y, hypotenuse climbing toward +Y/+Z. Useful for ramps, roof pitches, doorstops |
| `frustum` | `bottom=[w,d]`, `top=[w,d]`, `height` | truncated rectangular pyramid (defaults `bottom=[1,1]`, `top=[0.5,0.5]`, `height=1`) |
| `tube` | `outer`, `inner`, `height` | hollow cylinder (pipe / ring); `segments` (24) |
| `hemisphere` | `radius` | half-sphere, flat side on -Y; `rings` (8), `segments` (24) |
| `half_cylinder` | `radius`, `height` | D-profile half-cylinder, flat side facing -Z; `segments` (24) |
| `torus_arc` | `major`, `minor` | partial torus; `arc` (degrees, default 90) sweeps around +Y; `major_segments` (24), `minor_segments` (12). Useful for arches and handles |
| `ellipsoid` | `size=[x,y,z]` | `rings` (16), `segments` (24); independent radii per axis |
| `superellipsoid` | `size=[x,y,z]` | `ew`, `ns` (1 = sphere, > 1 boxy, < 1 pinched), `rings` (16), `segments` (24) |
| `curved_plane` | `size=[x,z]` or `vec3` | `bend_u`, `bend_v` (degrees; arc angle along X/Z), `segments_u`/`segments_v` (12) |
| `lathe` | `profile=[[r,y], …]` | `segments` (24), `cap_ends` (1 = capped); profile authored bottom-to-top in `(radius, y)` pairs |
| `spline_tube` | `points=[[x,y,z], …]` | `radius` (scalar) or `radii=[…]` (per-point), `segments` (12), `samples` (8), `cap_ends` (1) |
| `spline_ribbon` | `points=[[x,y,z], …]` | `width` (scalar) or `widths=[…]` (per-point), `samples` (8), `twist` (degrees, default `0`); flat strip along a Catmull–Rom curve |
| `coil` | — | `radius` (helix, 0.5), `height` (Y rise, 1.0), `turns` (revolutions, 3), `profile_radius` (cross-section, 0.05), `segments` (cross-section sides, 12), `samples` (per turn, 16), `cap_ends` (1), `handedness` (`"right"`/`"left"`, default `"right"`). Helical sweep — springs, screw threads, snail-shell ribs, twisted vines. Builds on `spline_tube` under the hood; the helix path is generated for you instead of authored point-by-point. |
| `heightfield` | — | `size=[w,d]` (XZ extent, default `[1, 1]`), `segments_u`/`segments_v` (32 each), `amplitude` (peak Y, 0.5), `octaves` (1..=8, default 3), `frequency` (cycles/unit, 1.0), `persistence` (per-octave amplitude falloff, 0.5), `seed` (1). Tessellated XZ grid displaced by deterministic fBm value-noise — terrain patches, dunes, rooftops, bumpy stone slabs. Layer the `wave` deformer on top for water surfaces. |
| `bezier_patch` | `points=[[x,y,z], …]` (exactly 16 control points, row-major u rows × v columns) | `segments_u`/`segments_v` (12 each). Bicubic Bézier surface — `points[0]`/`[3]`/`[12]`/`[15]` pin the patch corners, the inner four `points[5]/[6]/[9]/[10]` shape the bulge, and the eight edge points control curvature along each side. Use for organic skin panels: faces, hoods, fenders, pillows, sails, soft plates, leaves with controlled silhouette. |
| `metaball` | `points=[[x,y,z], …]` (≥1) plus one of `radius=` (scalar) or `radii=[…]` (per-point) | `blend` (smooth-union radius in m, default `0`), `rings` (per-sphere, 12), `segments` (per-sphere, 16). N implicit-field spheres unioned with smooth blending — creature bodies (torso + thigh masses), slime, clouds, cell clusters, pumpkin lobes, soft ammo pouches, asymmetric organic props. Sugar over `union (smooth=k) { sphere … }`; reuses the same vertex-fillet kernel as `union`'s `smooth=`. |
| `extrude` | `points=[[x,z], …]` | closed CCW outline; `hole=[[x,z], …]` (one CW inner contour), `height` (Y span, 1.0), `taper` (top scale ratio, 1.0), `twist` (degrees, 0), `caps` (1). Push a 2D polygon up — I-beams, gear teeth, custom pillars. Multi-hole authoring not yet supported (chain `extrude` + `difference`). |
| `sweep` | `profile=[[x,y], …]`, `path=[[x,y,z], …]` | closed CCW profile in the path's local XY plane; `samples` (8) per path segment, `twist` (degrees uniform), `roll=[deg, …]` and `scale_along=[s, …]` modulators (per-control-point), `caps` (1). Generalises `spline_tube` (always circular) and `spline_ribbon` (always flat). |
| `loft` | `points=[[x,z], …]`, `heights=[y, …]` | sections flat-packed in `points` (each section's vertices in order; counts must match across sections); `samples` (rings between adjacent sections, 4), `caps` (1). Boat hulls, fuselages, shaped bottles. |
| `leaf_card` | `size=[w,h]` | `cards` (default `2`); alpha-cutout foliage card cluster — one quad plus `cards-1` rotated copies sharing the same XY plane. Pair with a `mat="…"` whose `alpha_mode="mask"` and `double_sided=1` |
| `mesh` | `src="path.glb"` | load and embed an external glTF binary as a single mesh. Path is relative to the calling `.mog`. Materials, skinning, and animations on the source GLB are dropped — set them in the DSL instead |
| `branch` | — | procedural tree / vine / antler. See [Branch](#branch) below |
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

`curved_plane`, `lathe`, `spline_tube`, and `spline_ribbon` accept nested
list literals: `points=[[0, 0, 0], [1, 0.5, 0]]`,
`profile=[[0.2, 0], [0.5, 0.4]]`. Inner lists must be constant (no
`$param`) — parameterise the whole node via a module wrapper instead.
`spline_tube` and `spline_ribbon` both run a Catmull–Rom curve through
their control points and use a parallel-transport frame so the cross
section doesn't flip at inflection points; `spline_ribbon` adds a `twist`
that ramps a roll around the path tangent.

`tube`, `hemisphere`, `half_cylinder`, and `torus_arc` are the open /
hollow / partial counterparts of the canonical round primitives. They give
you ring tops, bowls, columns with a flat back, arches, and handles
without an extra CSG step.

`leaf_card` is the workhorse for foliage and feathers: it builds a small
cluster of crossed quads that sit on top of an alpha-cutout texture, so an
entire bush or pine sprig reads as one mesh.

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

### Branch

`branch` is a self-contained procedural tree builder. One node expands
into a recursive cluster of `spline_tube` segments tapering from a thick
trunk down to twigs, with optional `leaf_card` clusters at the tips. The
result reads as one editable wrapper in the scene graph; the inner
segments are stamped non-editable because their geometry is a
deterministic function of `seed=`.

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

Wrap a `branch` in a `group` and apply `scale=` / `rot=` to compose
forests, antlers, vines, or root systems out of the same generator. Pair
it with the stdlib `branch` and `leaf` modules for hand-tuned shapes.

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
- `prompt` — optional free-form description of the surface, used by
  `mogen textures` as the subject hint when generating an albedo image. Lets
  you steer the model away from the auto-derived "material name + colour"
  framing — useful when the material name is generic (`fabric_main`) or when
  the default phrasing trips Gemini's recitation filter. Example:
  `prompt="navy nylon ripstop weave"`. The texture pipeline rephrases this
  on retry if the image generator rejects the request for recitation, so a
  literal brand-adjacent phrasing won't permanently jam a build.
- `shader` — `"standard"` (default) or `"water"`. Selects a per-material
  shader override in **MoGen Studio's preview only** — the exported `.glb`
  always uses standard PBR, since glTF 2.0 cannot carry custom shader code.
  `"water"` swaps the live preview for animated ripples + fresnel-driven
  body/sky mix + sun glints. The water branch reads the standard material
  knobs:
  - `color` is the absorbed body tint when looking straight down
    (`[0.12, 0.55, 0.62]` reads as a lagoon, `[0.02, 0.05, 0.15]` as deep
    ocean).
  - `uv_scale` controls ripple density: `1.0` ≈ pool-scale chop, raise it
    for choppier small ponds, lower it for lazy ocean swells.
  - `roughness` ties together chop, sky-reflection blur, sun-glint
    sharpness, and foam. `0.05` is glassy mirror, `0.4` is a calm pool,
    `0.9` (the default) is ocean-style ripples, `1.0` adds whitecaps.
  - `metallic` lerps the Fresnel base from clean dielectric water (`0`)
    toward liquid metal at `1` — mercury / molten silver, where the body
    tint becomes the reflection colour at all angles.
  - `transmission` makes the body absorption recede so the sky reflection
    and what's behind the surface dominate. Combine with
    `alpha_mode="blend"` to actually see the pool floor through the water.
  - `emissive` / `emissive_strength` light the water from within (lava,
    magic potion, bioluminescent surf).
  - `normal_strength` multiplies the wave-slope (default `1.5`); raising
    it deepens the ripples without retuning chop.
  - `normal_texture` and `base_color_texture` are blended into the
    procedural waves and body tint respectively so authors can paint in
    high-frequency detail or shallow/deep variation.

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

Texture files are embedded in the output GLB, so the resulting `.glb` is
self-contained and can be moved without the source images. Missing files
are a hard error at export.

Reference a material on any geometry or group via `mat="wood"`. The lookup is
by exact string match; unknown names are a hard error at lowering.

---

## Decals

A `decal` is a transparent image (logo, label, sticker, scribble, seal,
handwritten note) projected onto a surface. It lowers to a thin double-sided
quad floating slightly off the parent surface, with an auto-synthesized
`alpha_mode="blend"` material whose albedo is an RGBA PNG.

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
- `offset` — `+Z` gap from the surface, in local units, to avoid
  z-fighting against the underlying mesh. Default `0.001` reads flush at
  typical scales; raise on coarse geometry.

### Curved surfaces: `on=` / `at=`

For flat surfaces, place the decal as a child of the surface and let `pos=`
handle alignment. For *curved* surfaces, write the decal once with `on=` and
`at=` and the lowering pass synthesizes a `conform` patch behind the scenes —
the decal's vertices are bent onto the target's surface so it actually hugs
the curvature.

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

When `on=` is used the decal is reparented under the target. Its `pos=` is
dropped (positioning comes from `at=`, not user transforms), but `rot=` and
`scale=` are baked into the artwork before projection — so `rot=[0, 0, 90]`
spins the logo 90° in the tangent plane (the useful "rotate the artwork
around the surface normal" case), and `scale=[2, 1, 1]` makes it twice as
wide. Off-plane rotations like `rot=[90, 0, 0]` tilt the artwork off the
surface; the conform kernel reproduces them faithfully but the result is
rarely what authors want — reach for `rz=` / `rot=[0, 0, deg]` for the
common "spin the logo" case.

If neither `prompt=` nor `image=` is set, the decal's name is used as the
prompt. That makes the compact form `decal "embroidered logo, white thread"
(size=[0.2, 0.1], pos=[0, 0.1, 0.101])` valid — handy when you want to keep
the description and the node identity in one place.

A few rules that aren't optional:

- `mat=` is rejected on decals — they own their material outright. Use
  `tint=`/`roughness=` to influence shading.
- Each decal gets its own auto-named material (`__decal_<name>`) and
  is never merged into adjacent same-material siblings by the export-time
  merge pass.
- The `mogen textures` pipeline asks Gemini for transparent-background
  RGBA directly. There is no chroma-key step: the `alpha_mode="mask"`
  foliage path is for foliage, not decals.
- `at=`, `up=`, and `lift=` are inert without `on=` — the validator rejects
  them so a typo doesn't silently disappear.

Example: a logo on the front of a shirt, plus an authored handwriting
overlay on a paper card.

```
material "shirt" (color=[0.1, 0.2, 0.6])
material "paper" (color=[0.96, 0.94, 0.88], roughness=0.95)

scene {
  box "shirt" (size=[0.6, 0.8, 0.2], mat="shirt")
  decal "shirt_logo" (
    pos = [0, 0.1, 0.101],
    size = [0.25, 0.12],
    prompt = "embroidered MoGen logo, white thread on dark fabric"
  )

  panel "card" (size=[0.4, 0.3, 0.01], mat="paper", right_of="shirt", gap=0.2)
  decal "note" (
    pos = [0.6, 0.0, 0.0061],
    size = [0.30, 0.20],
    rot = [0, 0, 0],
    image = "textures/notes/handwritten_thanks.png"
  )
}
```

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

## Attach: rigid alignment of two connector frames

`attach` is the rigid counterpart to `conform`: it sets a child node's
transform so its `plug` connector lines up exactly with a `socket`
connector on a parent, then reparents the child under the parent. No
deformation, no per-vertex work — just a clean alignment with optional
roll.

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

After lowering, the child is reparented under the parent and its local
TRS is recomputed so the two connector frames are coincident (`+offset`
along the socket normal, plus `twist` around it). Any `pos=` / `rot=`
declared on the child stays as an additive local offset on top of the
alignment — that's what lets a Studio gizmo drag survive a rebuild.

`attach` also runs per-instance inside `array` and `mirror` replicators:
when you write the attach inside the body of an `array (count=4)`, each
of the four expanded copies resolves its own pair of connectors, so a
single declaration glues every copy.

---

## Conform: moulding a primitive onto a target surface

`conform` deforms a child primitive's *vertex positions* so it lies on a
target mesh's surface. It has two modes:

- **Path mode (`from=` / `to=`)** — stretches a strip or tube between two
  connectors on the target. The canonical case is a zip on a curved sports
  bag; covers labels wrapped around bottles, gold trim along a shield's edge,
  hoses lying on a chassis, ribbons spiralling around a vase, stitched seams.
- **Patch mode (`at=`)** — lays a flat / disc-shaped child down at a single
  anchor connector and bends it to follow surface curvature locally. The
  canonical case is a round pocket on the side of the bag; covers brand
  decals, plates, lids, eye spots, leather patches.

Conform is the *deforming* counterpart to `attach`: where attach sets a rigid
transform aligning two connector frames, conform mutates the child's mesh.
Pick the mode by which attrs you provide — mixing `at=` with `from=`/`to=` is
an error, and so is omitting both.

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
(`union`/`difference`/`intersect`) are rejected in both modes. The error
message names the kind and points to the other mode if it would have worked
there (e.g. `disc` rejected in path mode → suggests patch mode).

### Tessellation

The deformation reads each child vertex's coordinate on the `along` axis as
its position along the path. A bare `box` only has two distinct values per
axis (the eight corners), so an un-subdivided box can't bend — every
interior path frame is skipped and the strip stays straight no matter how
curved the target surface is.

`conform` therefore inserts planar cuts perpendicular to `along` whenever
the child's tessellation is coarser than `samples / 4` segments (clamped to
8–64). Author-controlled subdivision still wins: pass a primitive that's
already dense (`curved_plane (segments_u=48)`, `cylinder (segments=64)`,
`spline_ribbon (samples=64)`) and the auto-subdivision is a no-op.

### Examples

```
// Zip on a sports bag.
material "leather" (color=[0.18, 0.16, 0.14], roughness=0.85)
material "rubber"  (color=[0.08, 0.08, 0.08], roughness=0.7)

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
// Wine-bottle label wrapped around a cylindrical bottle.
scene {
  cylinder "bottle" (radius=0.04, height=0.3, mat="glass") {
    connector "label_l" (at=[-0.04, 0.12, 0],  dir=[-1, 0, 0])
    connector "label_r" (at=[ 0.04, 0.12, 0],  dir=[ 1, 0, 0])
  }
  curved_plane "label" (size=[0.25, 0.06], segments_u=48, mat="paper")
  conform (target="bottle", child="label", from="label_l", to="label_r",
           along=x, lift=0.0005)
}
```

```
// Hose draped along a chassis: tube child, along=y matches cylinder's long axis.
scene {
  superellipsoid "chassis" (size=[1.6, 0.4, 0.7], ew=2.0, ns=2.0, mat="metal") {
    connector "port_a" (at=[-0.7, 0.20, 0.30], dir=[0, 1, 0])
    connector "port_b" (at=[ 0.7, 0.20, 0.30], dir=[0, 1, 0])
  }
  cylinder "hose" (radius=0.03, height=1.4, mat="rubber")
  conform (target="chassis", child="hose", from="port_a", to="port_b",
           along=y, samples=96, lift=0.005)
}
```

```
// Patch mode — round pocket decals on the sides of a sports bag.
scene {
  superellipsoid "body" (size=[0.6, 0.3, 0.3], ew=1.5, ns=1.2, mat="fabric") {
    connector "left_spot"  (at=[-0.3, 0, 0], dir=[-1, 0, 0])
    connector "right_spot" (at=[ 0.3, 0, 0], dir=[ 1, 0, 0])
  }
  disc "pocket_l" (radius=0.08, segments=32, mat="accent")
  disc "pocket_r" (radius=0.08, segments=32, mat="accent")
  conform (target="body", child="pocket_l", at="left_spot",  lift=0.002)
  conform (target="body", child="pocket_r", at="right_spot", lift=0.002)
}
```

### Pass ordering and reparenting

Conform runs after `attach` and before skin binding, so an attached child can
also be conformed and bind-pose world matrices reflect the deformed geometry.

By default (`reparent=1`) the child is moved under the target with an identity
local transform — its deformed vertices already live in the target's local
frame, so this keeps the scene tree clean. Any user `pos=` / `rot=` declared
on the child is intentionally discarded once the conform fires. Pass
`reparent=0` to keep the child's original parent; the deformed mesh is
transformed back into the child's local frame and the child's location in the
hierarchy is untouched.

### Path generation

The path is built by chord-and-snap: each sample's chord-interpolated point is
projected onto the target surface via closest-point query. This is *not* a
true geodesic, but for the typical conform use cases (smoothly curving
surfaces between two connectors), it is visually indistinguishable. Crank
`samples=` up for high-curvature paths.

`twist` ramps a roll around the path tangent linearly from 0 at the first
sample to `twist` degrees at the last — useful for spiralling ribbons or
bandages where the strip rotates around the path as it walks.

### Reserved (not yet implemented)

The validator accepts but lowering rejects, with a clear message: `direction`
(projection mode — decal splat from a direction), `curve` (only
`"geodesic_lerp"` is supported in v1), and `via` (multi-segment paths).

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

Both copies share the body's node names, and both are bound when their
mesh carries `skin="…"`. (Replicator-produced nodes inherit the AST node's
binding; the skinning pass walks every duplicate, not just the first.)

#### `flip_bind=1`: rebind the mirrored copy to the symmetric bone

Module parameters are numeric only, so two skin-bound limbs that differ
**only** in their bone suffix (`shoulder_l` ↔ `shoulder_r`,
`ankle_l` ↔ `ankle_r`, …) can't be DRY'd by passing the bone name into
a shared module. `flip_bind=1` solves this directly on `mirror`:

```
mirror "sleeves" (axis=x, flip_bind=1) {
  chamfered_box "sleeve" (
    pos=[0.135, 0.695, 0],
    size=[0.052, 0.158, 0.052],
    skin="rig", bind="shoulder_l", faceted=1
  )
}
```

emits the authored copy bound to `shoulder_l` AND a mirrored copy bound
to `shoulder_r`. The flip applies on every mesh-bearing descendant of the
mirrored instance: the AST-resolved `bind="…"` (whether authored locally
or inherited from a wrapping `group (bind=…)`) is matched against a
trailing `_l` or `_r` and the suffix swapped. Binds that don't end in
`_l`/`_r` (e.g. `bind="spine_chest"`) pass through unchanged, so plain
`mirror (axis=x)` is fine for shared-bone pairs and `flip_bind=1` only
needs to be added when the symmetry crosses a per-side bone.

`flip_bind` defaults to `0`. It only has an effect on the `mirror` kind;
other replicators (`array`, `stack`, `grid`) ignore it.

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

- `union` — N ≥ 1 operands; the union of all. Accepts an optional
  `smooth=<radius>` attribute that swaps the boolean union for a smooth
  minimum (`smin`) blend with that radius. Used by the humanoid stdlib
  modules to fillet limb-to-torso seams; values of a few centimetres are
  typical at human scale. `smooth=0` (the default) is identical to the
  hard boolean.
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

- Each parameter has a default that is either a **scalar** (number or
  `$param`-expression) or a **constant `vec3`** (e.g. `offset=[0, 1, 0]`).
  vec3 defaults must be fully constant — components referencing other
  parameters are rejected because parameter defaults are evaluated before
  the binding scope exists. `list`, string, and ident defaults are rejected.
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

## Control flow: `if`, `else`, `for`, string interpolation

Inside any `{ … }` body — including `scene`, `group`, `solid`, and `module`
bodies — three control-flow constructs let you branch and repeat at
module-expansion time. They run before lowering, so the resulting scene
graph never sees `if` or `for`; only the geometry they emit.

### `if (cond=…)` and `else`

```
module "switch" (has_label=0) {
  cylinder "shaft" (radius=0.02, height=0.05)
  if (cond=$has_label) {
    box "label" (size=[0.04, 0.005, 0.02])
  }
}
scene {
  use "switch" (has_label=0)            // no label
  use "switch" (has_label=1, x=0.10)    // with label
}
```

`cond=` accepts any expression. Comparisons (`<`, `<=`, `>`, `>=`, `==`,
`!=`) evaluate to `1.0` (true) or `0.0` (false), so `cond=$count > 1`
works directly. An immediately-following sibling `else { … }` covers the
false branch:

```
if (cond=$is_glass) {
  box "pane" (size=[0.6, 1.0, 0.01], mat="glass")
}
else {
  box "panel" (size=[0.6, 1.0, 0.04], mat="oak")
}
```

A standalone `else` with no preceding `if` is rejected at expansion time.

### `for (var=…, from=…, to=…[, step=…])`

```
for (var="i", from=0, to=4) {
  box "fence_post_$i" (size=[0.04, 0.6, 0.04],
                       pos=[$i * 0.30, 0, 0],
                       mat="oak")
}
```

- `var` is the loop binding name (string or bare identifier).
- `from`/`to` are the bounds; iteration covers `[from, to)` like Python's
  `range`. `from == to` produces zero iterations.
- `step` defaults to `1.0`. Must be non-zero. Negative `step` walks
  downward as long as `from > to`.
- Inside the body, `$<var>` resolves to the current loop value.
- `for` blocks inside module bodies see both module parameters and the
  loop variable in scope; nested `for` loops compose normally.

### String interpolation

Inside any string literal (including node names), `$name` and `${name}`
are replaced with the named binding's value at expansion time:

```
for (var="i", from=0, to=3) {
  cylinder "leg_$i" (radius=0.05, height=0.6, pos=[$i * 0.4, 0, 0])
}
// Names: leg_0, leg_1, leg_2
```

- Integer-valued bindings render without a decimal: `leg_$i` becomes
  `leg_3`, not `leg_3.0`.
- The `${name}` form delimits the binding explicitly so `${prefix}_panel`
  is unambiguous when followed by underscore characters.
- A `$` not followed by an identifier (or referencing an unbound name)
  is left literal — handy for prompts that include the dollar sign.

Limitations (deliberate, demand-driven):

- Comparisons can't be chained: `a < b < c` does not parse. Combine with
  multiplication for AND (`($a > 0) * ($b > 0)`) or addition for OR
  (`($a > 0) + ($b > 0)`).
- No boolean `&&` / `||` / `!` operators yet.
- Expressions remain numeric; there are no string concatenation operators
  beyond interpolation.

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
  use "chair" (pos=[ 1, 0, 0])
  use "table" (pos=[ 0, 0, 0])
  use "chair" (pos=[-1, 0, 0], rot=[0, 180, 0])
}
```

`use` accepts the same translation/rotation/scale shortcuts as every other
node kind — `pos`, `rot`, `scale`, `x` / `y` / `z`, `rx` / `ry` / `rz`, and
`from` / `to` — and applies them as an implicit wrapping `group` around the
expanded body. Equivalent to `group (pos=…) { use "x" () }` but without the
ceremony. If the module declares a parameter with one of those names (e.g.
a scalar `pos` param), the caller's value binds to the parameter instead.

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

Animation lowers to glTF node-transform tracks. Every clip animates one or
more scene nodes (or joints, which are scene-node aliases with a typed
DOF) — there is no separate animation graph or state machine. Clips are
top-level declarations alongside `material`/`module`/`joint`. Skinning is
additive: see [Skeletons and skinning](#skeletons-and-skinning-skeleton-bone-skin-bind)
below for the `skeleton`/`bone`/`skin=` half of the story.

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
  a node, add `prop="translation"|"rotation"|"scale"` to pick the channel
  (`pos` / `rot` are accepted aliases).
- `from` / `to` are scalars. For rotation they're degrees around the joint's
  `axis` (or the track's `axis=` when targeting a plain node); for
  translation they're distance along the axis; for scale they're the
  uniform factor. Two keyframes are emitted at `0` and `seconds` and
  linearly interpolated.
- For multi-keyframe authored curves, pass `keys=[[t, v], …]` instead of
  `from`/`to`. Times must be strictly ascending and span any subset of
  `[0, seconds]` — the exporter emits one glTF keyframe per pair and
  interpolates linearly between them. This is what the stdlib walk / run
  / jump clips use to drive bones with hand-tuned curves.

### Easing

Every `track` and procedural template accepts an optional `easing=` attribute
that selects a non-linear interpolation curve. glTF samplers themselves only
support LINEAR / STEP / CUBICSPLINE, so MoGen bakes the easing curve into a
dense LINEAR sampling at lower time — the resulting `.glb` plays back the
same in any compliant viewer (Godot, Blender, Three.js, glTF-Validator).

For an authored `track`, easing is applied between consecutive user
keyframes (so `keys=[[0, 0], [1, 90]]` with `easing=ease_in_out` produces a
smooth S-curve from 0° to 90°). For a template, easing warps the procedural
phase parameter — e.g. `open_close (..., easing=ease_in_out_back)` opens
slowly, overshoots, and settles back.

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

- `skeleton "name" { … }` declares the rig. Inside, every child must be a
  `bone`. Bones may nest arbitrarily — each `bone` becomes a scene node
  parented under the previous one, so its `pos`/`rot`/`scale` are
  parent-relative.
- `bone "name" (pos=…, rot=…, scale=…, envelope=…)` declares a joint.
  `envelope=` (default `0.75`) controls how far the bone's influence
  reaches when the auto-skinner assigns weights to nearby vertices —
  smaller envelopes are tighter, larger envelopes blend more across
  joints. Adjacent bones should overlap in envelope so vertices near a
  shared joint receive weight from both sides.

Skeletons are top-level or scene-level declarations. They place
themselves in the scene tree (so they animate alongside other nodes),
but they don't carry geometry of their own.

### Binding meshes: `skin="rig"`

Any mesh-bearing node (primitive or import) with `skin="<skel name>"`
becomes a skinned mesh: the lowering pass walks the skeleton, computes
per-vertex weights against the four nearest bones (capped by each bone's
`envelope`), and writes them into the GLB as `JOINTS_0` / `WEIGHTS_0`
accessors. Group-like containers (`group`, `solid`, `stack`, `grid`,
`array`, `mirror`, `module`, `use`) propagate `skin=` to every mesh
descendant, so wrapping a sub-tree in `group (skin="rig") { … }` skins
the whole thing in one line.

### Rigid pinning: `bind="bone_name"`

Add `bind="bone"` alongside `skin=` to pin every vertex of that mesh
rigidly to a single bone — weight 1.0, no envelope blend. Used for
accessories that should track a joint without deforming: heads (bound to
the neck), helmets, backpacks, hand-held props. `bind` propagates from a
group to its descendants the same way `skin=` does, so a face cluster
parented under a `group (bind="neck")` follows the head as one rigid
piece.

For `_l`/`_r`-suffixed pairs (e.g. left and right sleeves bound to
`shoulder_l` / `shoulder_r`) — where the only difference between the two
sides is the bone suffix — author one side and wrap it in
`mirror (axis=x, flip_bind=1)`. The mirrored copy keeps every other
attribute identical and rebinds to the swapped bone. See
[`mirror`](#mirror) above.

### Animating bones

Bones are scene nodes, so the regular `clip { track … }` machinery drives
them: `track "thigh_l" (prop=rotation, axis=[1, 0, 0], keys=[…])`. The
stdlib `humanoid_walk` / `humanoid_run` / `humanoid_idle` / `humanoid_jump`
modules expand into clips of exactly this shape, targeting the bones
declared by `humanoid_full`. There is no separate "animation rig" — if
you can drive a node, you can drive a bone.

CSG operands (`union`/`difference`/`intersect` children) are fused into
the parent's mesh during lowering, so a stray `skin=` on an operand never
survives. Put `skin=` on the CSG node itself (or on a wrapping group)
instead.

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

## Colliders

Annotate any geometry, `group`, `solid`, or `use` with `collider="aabb"` to
mark it as a collision volume:

```
slab "floor"     (size=[18.4, 0.1, 10.4], mat="wood", collider="aabb")
slab "wall_back" (size=[18.4, 3, 0.2], z=-5.1, mat="plaster", collider="aabb")
use  "desk"      (pos=[0, 0, 0.4], collider="aabb")
```

The bounding box is derived at compile time from the node's **subtree mesh
extents** in node-local space, after attach / conform / skin binding have
finished — so a collider on `use "desk"` encloses the whole desk, and a
collider on a `conform`-deformed plank reflects the bent vertices, not the
straight ones.

`"aabb"` is the only accepted value in v1; anything else raises a build error.
A collider on a node whose subtree carries no mesh is silently dropped (the
attribute lives on, but the box is omitted from the output).

The export writes one entry per collider'd node into glTF
`node.extras.collider`:

```json
"extras": {
  "collider": {
    "type": "aabb",
    "min": [-9.2, -0.05, -5.2],
    "max": [ 9.2,  0.05,  5.2]
  }
}
```

`mogen` does not run a physics simulation — this is metadata for the
downstream importer to convert into a `CollisionShape3D` (or equivalent).
MoGen Studio renders an off-by-default wireframe gizmo at each collider'd
node; toggle it from **View → Show Colliders** or the viewport context menu.

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

The flag propagates monotonically: an ancestor's `cast_shadow=0` overrides a
descendant default. Setting it on a child while a parent already disabled it
is a no-op (the descendant stays opted out).

The export writes `extras.cast_shadow=false` only on opted-out nodes; the
typical "casts shadow" case omits the key entirely so the JSON chunk stays
lean. Downstream importers that don't recognise the key fall back to the
glTF default (casts shadow), matching the spec.

The right-sidebar inspector exposes the toggle as a checkbox under
**Shadow**.

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
