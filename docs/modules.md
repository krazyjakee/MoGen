# MoGen module catalog

Modules are parametric sub-graphs: reusable snippets of DSL that take scalar
parameters, expand to a tree of primitives, and can expose connectors for
downstream composition. The full language is documented in
[`dsl.md`](./dsl.md); this page is a catalog of the modules shipped in the
**stdlib** and a recipe for adding more.

- [How modules resolve](#how-modules-resolve)
- [Authoring a new module](#authoring-a-new-module)
- [Stdlib catalog](#stdlib-catalog)
  - [Humanoid](#humanoid) — body, head, limbs, hands, feet, face, hair
  - [Humanoid animations](#humanoid-animations) — idle, walk, run, jump
  - [Animals](#animals) — quadruped torso/leg, tail, ear, eye
  - [Foliage](#foliage) — leaf, branch

---

## How modules resolve

Given a call `use "leg" (height=0.5)`:

1. Look up `"leg"` in the **module registry** — a flat map populated from
   three sources, in this precedence order: **user declarations > imports
   > stdlib**. A user `module "leg" { … }` shadows any imported `leg`,
   and an imported `leg` shadows the stdlib's `leg`.
2. Bind caller arguments (`height=0.5`) against the declared parameter
   list. Unknown argument names are a hard error (catches typos).
3. Fill declared defaults for any parameter the caller omitted.
4. Expand the module body, substituting every `$name` with its bound
   numeric value. `vec3`, `list`, string, or ident defaults are not
   accepted — every parameter is scalar.
5. Recurse: module bodies may themselves call `use`, up to a recursion-
   depth check that prevents accidental loops.

Expansion happens **before** the scene graph is built — by the time
lowering runs, every `$name` has been replaced and every `use` node has
been replaced with its expanded body. See
[`dsl.md` §Modules](./dsl.md#modules-module-and-use) and
[§Imports](./dsl.md#imports-import) for the full resolution rules.

---

## Authoring a new module

Three rules cover almost everything:

1. **All parameters are scalars.** Numeric defaults are required
   (`height=0.5`, `count=4`); `vec3` / list / string / ident defaults are
   rejected. If you want a positioned pose, pass the three components as
   separate scalars.

2. **Reference parameters as `$name`** inside the body. They compose into
   expressions in any numeric attribute position — `pos=[0, $h * 0.5, 0]`,
   `radius=$r`, `height=$h + 0.1`, and so on.

3. **Expose connectors where the caller will join you.** A `leg` that the
   seat attaches on top of should emit `connector "top" (...)` inside its
   mesh node. Tagging them (`tag=leg_top`) lets downstream fitting logic
   pair compatible anchors without hard-coded positions.

Stdlib modules also include a `// summary: <one-liner>` comment on the
first line. The CLI's stdlib index reads this to inject a single-line doc
for each module into LLM prompts, so write a description that says what
the module *is* and what its connectors are called — the LLM consumes it
verbatim.

A skeleton:

```
// summary: A box-shaped part with top/bottom connectors. Caller declares mat=part.
module "my_part" (width=1.0, height=1.0, depth=1.0) {
  box "body" (size=[$width, $height, $depth]) {
    connector "top"    (at=[0,  $height * 0.5, 0], dir=[0,  1, 0], tag=part_top)
    connector "bottom" (at=[0, -$height * 0.5, 0], dir=[0, -1, 0], tag=part_bottom)
  }
}
```

Drop the file in `crates/mogen-dsl/stdlib/<name>.mog` and add an entry to
the `STDLIB_FILES` table in `crates/mogen-dsl/src/stdlib.rs`. The
`all_stdlib_modules_parse_and_load` test enforces that every entry parses
and carries a `// summary:` line; `each_stdlib_module_lowers_in_isolation`
checks that defaults produce a valid scene graph.

---

## Stdlib catalog

Every module below lives at
`crates/mogen-dsl/stdlib/<name>.mog` and is registered in `stdlib.rs`. All
stdlib content is shadowed by user declarations and imports, so a project
can override any module by re-declaring it in the importing file.

### Humanoid

Modular body parts that compose into a full character. Most accept a
single `size`/`length`/`radius` knob and a few independent tuning params.
The full body is `humanoid_full`; the others are useful for composing a
custom rig from a subset of parts.

#### `humanoid_full`

Complete rigged Synty/Quaternius-style humanoid in one declaration —
torso, head, arms, mitten hands, legs, feet, and visible face features
(eyes, brows, nose, mouth), all skinned to a `"rig"` skeleton with rigid
binding so each body part follows exactly one bone.

| parameter | default | meaning |
|---|---|---|
| `height` | `1.7` | overall scale (figure stands ~ this tall in metres) |
| `skin` | `[0.85, 0.65, 0.55]` | skin colour |
| `hair` | `[0.20, 0.15, 0.10]` | brow / hair colour |
| `eye` | `[0.08, 0.08, 0.10]` | eye-block colour |
| `mouth` | `[0.55, 0.20, 0.20]` | mouth-block colour |
| `shirt` | `[0.30, 0.45, 0.65]` | torso colour |
| `pants` | `[0.20, 0.22, 0.28]` | hips + leg colour |
| `boot` | `[0.15, 0.10, 0.07]` | foot colour |

The module declares matching `material "skin"`, `"hair"`, `"eye"`,
`"mouth"`, `"shirt"`, `"pants"`, `"boot"` internally from those params.
A scene-level `material "skin" (...)` declaration takes precedence (the
collector dedupes name-first, caller-wins).

Use only **once** per scene since it owns the global `"rig"` skeleton;
pair with one of the humanoid animation modules below so the figure
isn't frozen.

##### Slot connectors

The figure exposes a comprehensive set of named slot connectors so that
hats, masks, capes, packs, belts, sheaths, weapons, and gauntlets can
be authored as plain primitives and `attach`-snapped onto the figure.
Each slot bone-binds via its parent body part, so attached children
follow walks, runs, jumps, and idle motion automatically.

| slot | parent part | bone | usage |
|---|---|---|---|
| `slot_crown` | `head` | `neck` | hats, helmets, crowns |
| `slot_face` | `head` | `neck` | masks, visors |
| `slot_jaw` | `head` | `neck` | beards, chinstraps |
| `slot_ear_l` / `slot_ear_r` | `head` | `neck` | ear decoration, earrings |
| `slot_neck_back` | `head` | `neck` | collars, scarves |
| `slot_chest_front` | `torso` | `spine_chest` | badges, medals, breastplate emblem |
| `slot_chest_back` | `torso` | `spine_chest` | capes, banners (top edge) |
| `slot_back_lower` | `torso` | `spine_chest` | backpacks, satchels |
| `slot_shoulder_l` / `slot_shoulder_r` | `torso` | `spine_chest` | pauldrons, shoulder straps |
| `slot_waist_front` / `slot_waist_back` | `hips` | `hip` | belt buckle / rear of belt |
| `slot_waist_l` / `slot_waist_r` | `hips` | `hip` | sheaths, holsters |
| `slot_pelvis_front` | `hips` | `hip` | loincloths, codpieces |
| `slot_hand_l_grip` / `slot_hand_r_grip` | `hand_l` / `hand_r` | `wrist_l` / `wrist_r` | swords, staves, torches |
| `slot_hand_l_back` / `slot_hand_r_back` | `hand_l` / `hand_r` | `wrist_l` / `wrist_r` | gauntlets, signet rings |
| `slot_foot_l_top` / `slot_foot_r_top` | `foot_l` / `foot_r` | `ankle_l` / `ankle_r` | greaves attachment |
| `slot_foot_l_heel` / `slot_foot_r_heel` | `foot_l` / `foot_r` | `ankle_l` / `ankle_r` | spurs |
| `slot_foot_l_toe` / `slot_foot_r_toe` | `foot_l` / `foot_r` | `ankle_l` / `ankle_r` | toe caps, claws |

Each slot's `dir=` points outward from the figure; an attached child
should expose a `connector "..." (dir=[opposite], tag=plug)` so the
attach pass aligns the surfaces.

Hair is not bundled — `use "humanoid_hair_short"` (or
`humanoid_hair_long`) and attach to `slot_crown` if the figure needs
hair.

#### `humanoid_torso`

Soft superellipsoid torso with neck, shoulder, and hip sockets.

| parameter | default | meaning |
|---|---|---|
| `height` | `0.55` | torso length along +Y |
| `width` | `0.36` | extent along +X |
| `depth` | `0.22` | extent along +Z |

**Connectors:** `neck`, `shoulder_l`, `shoulder_r`, `hip_l`, `hip_r`.
Shoulders point in an A-pose direction (20° outward from straight-down)
so attached arms hang in a natural rest pose.

#### `humanoid_head`

Smooth-blended cranium + jaw with a face-and-ears connector cluster.

| parameter | default | meaning |
|---|---|---|
| `size` | `0.11` | head radius (m) |
| `jaw` | `0.7` | jaw fullness fraction (0 = no jaw, 1 = matching cranium) |

**Connectors:** `neck` (under), `crown` (top, hair anchor), `eye_l`,
`eye_r`, `nose`, `mouth`, `ear_l`, `ear_r`.

#### `humanoid_arm`

Upper-arm + forearm capsules smoothly joined at the elbow.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.55` | total arm length |
| `radius` | `0.05` | upper-arm cross-section radius (forearm tapers to 0.85×) |

**Connectors:** `shoulder` (top), `wrist` (bottom).

#### `humanoid_leg`

Thigh + shin capsules smoothly joined at the knee.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.9` | total leg length |
| `radius` | `0.07` | thigh cross-section radius |

**Connectors:** `hip` (top), `ankle` (bottom).

#### `humanoid_hand_5fingers`

Five-fingered left hand. Mirror with `mirror axis=x` to get a right hand.
Wrist plug at +Y, fingers extending in -Y.

| parameter | default | meaning |
|---|---|---|
| `size` | `0.09` | hand width (drives finger / palm scale) |

**Connectors:** `wrist` (attach to arm), `grip` (anchor for held props).

#### `humanoid_foot`

Sole + toe block smoothly blended into a boot-shaped form.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.26` | foot length along +Z |
| `width` | `0.10` | foot width |
| `height` | `0.10` | foot height |

**Connectors:** `ankle` (top), `toe` (front tip), `heel` (back).
Caller declares material `boot` (or sets `mat=` on the wrapping group).

#### `humanoid_face`

Face cluster — `eye_l`, `eye_r`, `nose`, `mouth` as **separate top-level
nodes**. Attach each to its matching connector on `humanoid_head`.

| parameter | default | meaning |
|---|---|---|
| `size` | `0.11` | head reference size; drives feature scale |

Caller declares materials `eye`, `skin`, `mouth`.

#### `humanoid_hair_short`

Skullcap with substantial occiput / nape bulk. Sits on the head's `crown`
socket.

| parameter | default | meaning |
|---|---|---|
| `size` | `0.115` | hair-cap radius |

Caller declares material `hair`.

#### `humanoid_hair_long`

Skullcap + back-falling drape past the shoulders.

| parameter | default | meaning |
|---|---|---|
| `size` | `0.115` | cap radius |
| `length` | `0.45` | drape length |

Caller declares material `hair`.

---

### Humanoid animations

Each one expands into a single `clip { track … }` that drives bones on
the `"rig"` skeleton declared by `humanoid_full`. They take no parameters
because every track is hand-tuned. Pair one with `humanoid_full` (or any
rig that uses the same bone names).

| module | duration | shape |
|---|---|---|
| `humanoid_idle` | 4.0 s loop | subtle breathing + weight-shift, very small amplitudes |
| `humanoid_walk` | 1.0 s loop | hip swing, opposite-arm shoulder swing, mid-swing knee lift, subtle spine counter-rotation |
| `humanoid_run` | 0.55 s loop | bigger amplitudes than walk, elbows held at 90° flex |
| `humanoid_jump` | 1.2 s one-shot | crouch → extend → airborne tuck → land (does not loop) |

Use exactly one humanoid animation per scene — they all author a clip
named after themselves driving the same bones, so combining two is a
recipe for fighting tracks.

---

### Animals

#### `quadruped_torso`

Elongated soft body with head, tail, and four leg sockets.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.9` | body length along +Z |
| `height` | `0.32` | body height |
| `width` | `0.30` | body width |

**Connectors:** `neck` (front), `tail` (back), `leg_fl`, `leg_fr`,
`leg_bl`, `leg_br` (four hip anchors).

#### `quadruped_leg`

Single capsule from hip to paw.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.45` | leg length |
| `radius` | `0.045` | cross-section radius |

**Connectors:** `top` (tagged `hip`), `paw` (bottom).

#### `tail`

Tapered tail (~0.4 m, base radius ~0.04 m) using `spline_tube` curving
down then up. Wrap in a `group` and apply `scale=` / `rot=` to resize.

No parameters. **Connector:** `base` at the trunk end.

#### `ear`

Curved-plane animal ear (triangular pinna).

| parameter | default | meaning |
|---|---|---|
| `size` | `0.06` | ear extent (m) |

**Connector:** `base` at the head-side edge.

#### `eye`

Spherical eyeball.

| parameter | default | meaning |
|---|---|---|
| `radius` | `0.022` | eyeball radius |

**Connector:** `back` at the socket-facing pole.

---

### Foliage

#### `leaf`

Curved-plane leaf with a slight cup along its length. Set the leaf
material's `double_sided=1`.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.12` | leaf length along +Y |
| `width` | `0.05` | leaf width along +X |

**Connector:** `stem` at the leaf's base.

#### `branch`

Tapered single branch (~0.6 m, base 0.04 m → tip 0.008 m) with a gentle
S-curve. Wrap in a `group` plus `scale=` / `rot=` to compose trees,
antlers, vines, and root systems.

No parameters. **Connectors:** `base` (trunk end), `tip`.

For a fully procedural recursive tree (multiple levels of splits and
auto-emitted leaves), use the `branch` *primitive* instead — see
[`dsl.md` §Branch](./dsl.md#branch).

---

### Detailing modules

Small parametric details for hard-surface, organic, and decorative parts.
Each one collapses a frequently hand-authored "cloud of primitives" pattern
into a single `use` call so the LLM can spend its token budget on the
scene structure instead of re-deriving the same array of cylinders or
cards.

All ten modules expand into geometry the caller wraps with a `mat=`
(usually via a parent `group`) — none of them carry a default material so
the same module composes equally well into a steel rivet line, a brass
bolt circle, or a copper vent strip.

#### `bolt_circle`

Ring of cylindrical bolt heads arranged around +Y. Heads sit on top of
`y=0` (`anchor=bottom`) so the whole ring drops onto a flat host face.

| parameter | default | meaning |
|---|---|---|
| `count` | `6` | number of bolt heads in the ring |
| `ring_radius` | `0.1` | distance from origin to each head |
| `head_radius` | `0.012` | radius of one cylindrical head |
| `head_height` | `0.008` | head thickness along +Y |

#### `vent_strip`

Vertical stack of thin parallel slats (cooling vents, gills, louvers).
Slats run along X; the stack grows along Y.

| parameter | default | meaning |
|---|---|---|
| `count` | `6` | number of slats |
| `length` | `0.4` | slat length along X |
| `slat_thickness` | `0.005` | slat thickness along Y |
| `slat_height` | `0.05` | slat depth along Z |
| `gap` | `0.012` | spacing between slats |

#### `panel_seam`

Thin dark line for hard-surface panel joins (car body seams, robot armor
splits, console trim). Sits with its top face at `y=0` (`anchor=top`) so
it lies flush on the +Y face of a host.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.5` | seam length along X |
| `width` | `0.002` | seam width along Z |
| `depth` | `0.0005` | seam depth into the host |

#### `rivet_line`

Evenly spaced row of low hemispherical rivet heads along X. Each rivet's
flat base sits on `y=0`, dome up to `y=+radius`.

| parameter | default | meaning |
|---|---|---|
| `count` | `8` | number of rivets |
| `length` | `0.4` | total span end-to-end along X |
| `radius` | `0.006` | rivet head radius |

#### `step_taper`

Lathed column with four visible steps, base radius `0.5` → top radius
`0.1` across height `1.0`. Use for rocket nozzles, tapered chimneys,
segmented pillars. Wrap in a group + `scale=` (or per-axis `w/h/d`) to
resize.

No parameters — the profile is unitised so a single `scale` propagates
uniformly.

#### `cable`

Thin spline_tube spanning `1m` along X with a gentle gravity dip at the
centre. Use for power cables, ropes, hoses, wires.

| parameter | default | meaning |
|---|---|---|
| `radius` | `0.008` | tube radius |

**Connectors:** `start` (`-X` end), `end` (`+X` end). For a cable that
runs between two specific anchor points, place this module via
`pos=`/`scale=` or run `conform` to drape it along a target surface.

#### `chain`

Interlocked torus links along X with alternating 90° orientation. `pairs`
is the number of A/B link pairs (default 4 → 8 links total).

| parameter | default | meaning |
|---|---|---|
| `pairs` | `4` | number of A/B link pairs along X |
| `link_radius` | `0.05` | inner radius of one torus link |
| `wire` | `0.012` | tube radius of the link wire |
| `gap` | `0.005` | spacing between links |

#### `feather_card`

Single curved-plane cupped along its length, like a feather or fish-fin
ray. Pair with an alpha-cutout feather/fin material (`alpha_mode="mask",
double_sided=1`).

| parameter | default | meaning |
|---|---|---|
| `length` | `0.15` | feather length along Z |
| `width` | `0.04` | feather width along X |

**Connector:** `stem` at the base.

#### `scale_band`

Wraparound array of curved-plane scales for snake / fish / dragon skin.
`count` scales encircle Y at `ring_radius`, each cupped outward (+X).
Stack multiple bands along Y for a full body.

| parameter | default | meaning |
|---|---|---|
| `count` | `12` | number of scales in the ring |
| `ring_radius` | `0.06` | radius of the scale ring |
| `scale_w` | `0.04` | scale width |
| `scale_h` | `0.06` | scale height |

#### `gear`

Coarse Phase-A gear: cylindrical hub plus an array of rectangular teeth.
Approximation only; once `extrude` (Phase B) lands, prefer it for proper
involute gears.

| parameter | default | meaning |
|---|---|---|
| `teeth` | `16` | number of teeth around the hub |
| `hub_radius` | `0.1` | radius of the central disc |
| `tooth_height` | `0.014` | radial extent of one tooth |
| `tooth_width` | `0.014` | tangential width of one tooth |
| `thickness` | `0.02` | gear thickness along Y |

---

### Organic shape wrappers

Sensible-default wrappers over the Phase D organic primitives (`coil`,
`heightfield`, `metaball`, `wave`-deformed `curved_plane`). Each one
collapses the parameter sprawl of the underlying primitive into a small
set of intent-shaped knobs so the LLM can reach for the right shape
without re-tuning fbm octaves or metaball blend factors from scratch.

Like the detailing modules, none of these carry a default `mat=` — wrap
in a parent `group` with a steel/water/earth/slime material as needed.

#### `spring`

Tight helical compression spring along +Y, centred on origin. Wraps
`coil` with spring-typical defaults (small radius, short height, many
turns, thin wire). Wrap with a metal `mat=` (`steel`/`brass`).

| parameter | default | meaning |
|---|---|---|
| `radius` | `0.025` | helix radius (distance from spring axis to wire centre) |
| `length` | `0.1` | total height along Y |
| `turns` | `8` | number of full revolutions over the height |
| `wire_radius` | `0.004` | tube radius of the wire |

#### `terrain_patch`

Square terrain patch using `heightfield` with natural-fbm defaults baked
in (4 octaves, mid-frequency, 0.5 persistence). Reads as rolling hills
out of the box. Bump `amplitude` to ~1.4 + `segments` to ~96 for craggy
peaks; lower `amplitude` to ~0.3 for sandy dunes.

| parameter | default | meaning |
|---|---|---|
| `size` | `4.0` | side length of the square patch (XZ) |
| `segments` | `64` | grid resolution along U and V |
| `amplitude` | `0.6` | peak-to-peak relief along Y |
| `seed` | `1` | noise seed; change for variation |

#### `blob`

Three slightly-offset metaballs unioned with smooth blending. One
`radius` controls per-ball size, `blend=` controls how much the
spheres merge. Reads as a slime, soft-creature mass, or jellyfish
body. Wrap with an organic `mat=` (`alpha=0.85` for slime; opaque
flesh-tone for creatures); follow with `scale=[1.6, 0.9, 0.9]` for an
elongated body.

| parameter | default | meaning |
|---|---|---|
| `radius` | `0.30` | per-sphere radius (all three the same) |
| `blend` | `0.15` | smooth-union blend distance — 0 is a hard union, larger numbers melt the spheres further |

#### `water_patch`

Flat XZ surface with low-amplitude `wave=` deformer for water, lava,
jelly, or any rippling pool. Wrap with a transparent water-style
`mat=` (`alpha=0.85, transmission=0.6` for water; opaque green
`alpha=0.85` for jelly).

| parameter | default | meaning |
|---|---|---|
| `size` | `2.0` | side length of the square patch (XZ) |
| `segments` | `64` | grid resolution along U and V — keep dense so the wave reads smoothly |
| `ripple` | `0.04` | wave amplitude along the surface normal |
| `frequency` | `0.6` | wave spatial frequency along X — raise to ~1.5 for choppy, lower for calm |
