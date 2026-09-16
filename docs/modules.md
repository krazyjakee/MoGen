# MoGen module catalog

Stdlib modules invoked via `use "name" (param=value)`. Language rules are in
[`dsl.md` §Modules](./dsl.md#modules-module-and-use) and
[§Imports](./dsl.md#imports-import).

- [How modules resolve](#how-modules-resolve)
- [Authoring a new module](#authoring-a-new-module)
- [Stdlib catalog](#stdlib-catalog)

---

## How modules resolve

For `use "leg" (height=0.5)`:

1. Look up `"leg"` with precedence **user declarations > imports > stdlib**, so
   any stdlib module can be overridden by re-declaring it.
2. Bind caller arguments; unknown argument names are a hard error.
3. Fill defaults for omitted parameters.
4. Substitute every `$name` in the body. Defaults must be a scalar or a constant
   `vec3` (e.g. colours); a `vec3` param substitutes as a whole attribute value
   only, no per-component arithmetic.
5. Recurse into nested `use`; recursion loops are detected and rejected.

Expansion happens before lowering. Imported module geometry uses its defining
file's `lod_scale`; enclosing `lod=` still multiplies through (see
[Per-node LOD](./dsl.md#per-node-lod-overrides)).

---

## Authoring a new module

- Declare defaults for every param; reference them as `$name` in any numeric
  expression (`pos=[0, $h * 0.5, 0]`, `height=$h + 0.1`).
- Expose oriented connectors where callers will join (`connector "top" (at=…,
  dir=…, tag=…)`); tags let fitting logic pair compatible anchors.
- Start the file with `// summary: <one-liner>` — it is injected verbatim into
  LLM prompts, so say what the module is and name its connectors.

```
// summary: A box-shaped part with top/bottom connectors. Caller declares mat=part.
module "my_part" (width=1.0, height=1.0, depth=1.0) {
  box "body" (size=[$width, $height, $depth]) {
    connector "top"    (at=[0,  $height * 0.5, 0], dir=[0,  1, 0], tag=part_top)
    connector "bottom" (at=[0, -$height * 0.5, 0], dir=[0, -1, 0], tag=part_bottom)
  }
}
```

Save as `crates/mogen-dsl/stdlib/<name>.mog` and add it to `STDLIB_FILES` in
`crates/mogen-dsl/src/stdlib.rs`. Tests require every entry to parse, carry a
summary line, and lower with its defaults.

---

## Stdlib catalog

Unless noted, modules carry no material: wrap them in a `group` with `mat=`.

### Humanoid

#### `humanoid_full`

Complete stylized low-poly humanoid: torso, head, arms, mitten hands, legs,
feet, eyes, brows, nose, mouth. Each part is rigid-bound to one bone of a
17-bone `"rig"` skeleton (`hip`, `spine_lower`, `spine_mid`, `spine_chest`,
`neck`, `shoulder_l/r`, `elbow_l/r`, `wrist_l/r`, `hip_l/r`, `knee_l/r`,
`ankle_l/r`, `toe_l/r`).

| parameter | default | meaning |
|---|---|---|
| `height` | `1.7` | overall height (m) |
| `weight` | `1.0` | girth multiplier on torso, hips, legs, arms; keep ~`0.7..1.3` to avoid arm/torso intersection |
| `head_size` | `1.0` | cranium scale; face features don't move, so values far from `1.0` misalign them |
| `leg_taper` | `0.0` | shin width from 84 % of thigh (`0.0`) to 60 % (`1.0`) |
| `skin` | `[0.85, 0.65, 0.55]` | skin colour |
| `hair` | `[0.20, 0.15, 0.10]` | brow / hair colour |
| `eye` | `[0.08, 0.08, 0.10]` | eye colour |
| `mouth` | `[0.55, 0.20, 0.20]` | mouth colour |
| `shirt` | `[0.30, 0.45, 0.65]` | torso colour |
| `pants` | `[0.20, 0.22, 0.28]` | hips + legs colour |
| `boot` | `[0.15, 0.10, 0.07]` | foot colour |

Declares materials `skin`, `hair`, `eye`, `mouth`, `shirt`, `pants`, `boot`
from these params; a scene-level material with the same name wins. Use
**once** per scene (it owns the `"rig"` skeleton), paired with one humanoid
animation module.

##### Slot connectors

Slots live on the bones, so attached items follow every animation. Use the
bone as the attach parent:
`attach (parent="neck", child="helmet", socket="slot_crown", plug="rim")`.
Each slot's `dir` points outward; give the attached item a plug connector
facing the opposite way.

| slot | bone | typical use |
|---|---|---|
| `slot_crown` | `neck` | hats, helmets |
| `slot_face` | `neck` | masks, visors |
| `slot_jaw` | `neck` | beards, chinstraps |
| `slot_ear_l` / `slot_ear_r` | `neck` | earrings |
| `slot_neck_back` | `neck` | collars, scarves |
| `slot_chest_front` | `spine_chest` | badges, emblems |
| `slot_chest_back` | `spine_chest` | capes |
| `slot_back_lower` | `spine_chest` | backpacks |
| `slot_shoulder_l` / `slot_shoulder_r` | `spine_chest` | pauldrons |
| `slot_waist_front` / `slot_waist_back` | `hip` | belt buckle / rear |
| `slot_waist_l` / `slot_waist_r` | `hip` | sheaths, holsters |
| `slot_pelvis_front` | `hip` | loincloths |
| `slot_hand_l_grip` / `slot_hand_r_grip` | `wrist_l` / `wrist_r` | held weapons, torches |
| `slot_hand_l_back` / `slot_hand_r_back` | `wrist_l` / `wrist_r` | gauntlets |
| `slot_foot_l_top` / `slot_foot_r_top` | `ankle_l` / `ankle_r` | greaves |
| `slot_foot_l_heel` / `slot_foot_r_heel` | `ankle_l` / `ankle_r` | spurs |
| `slot_foot_l_toe` / `slot_foot_r_toe` | `ankle_l` / `ankle_r` | toe caps |

Clothing, hair, and accessories are not stdlib modules — author them inline.
Alternatively place geometry in scene space with `skin="rig"` and
`bind="<bone>"`:

```mog
// hat at the crown: head block centre is height*0.890, hat sits +height*0.150 above
chamfered_box "hat" (
  pos=[0, 1.7 * 0.890 + 1.7 * 0.150, 0],
  size=[1.7 * 0.130, 1.7 * 0.040, 1.7 * 0.130],
  radius=1.7 * 0.010,
  mat="hat", skin="rig", bind="neck", faceted=1
)
```

Worked example: `examples/characters/businessman_casual.mog`.

#### `humanoid_torso`

Soft superellipsoid torso.

| parameter | default | meaning |
|---|---|---|
| `height` | `0.55` | length along +Y |
| `width` | `0.36` | extent along X |
| `depth` | `0.22` | extent along Z |

**Connectors:** `neck`, `shoulder_l`, `shoulder_r` (A-pose, 20° outward from
straight down), `hip_l`, `hip_r`.

#### `humanoid_head`

Smooth-blended cranium + jaw.

| parameter | default | meaning |
|---|---|---|
| `size` | `0.11` | head radius (m) |
| `jaw` | `0.7` | jaw fullness (0 = none, 1 = matches cranium) |

**Connectors:** `neck` (under), `crown` (top), `eye_l`, `eye_r`, `nose`,
`mouth`, `ear_l`, `ear_r`.

#### `humanoid_arm`

Upper-arm + forearm capsules joined at the elbow.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.55` | total length |
| `radius` | `0.05` | upper-arm radius (forearm 0.85×) |

**Connectors:** `shoulder` (top), `wrist` (bottom).

#### `humanoid_leg`

Thigh + shin capsules joined at the knee.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.9` | total length |
| `radius` | `0.07` | thigh radius |

**Connectors:** `hip` (top), `ankle` (bottom).

#### `humanoid_hand_5fingers`

Left hand: wrist at +Y, fingers extend -Y. `mirror axis=x` for a right hand.

| parameter | default | meaning |
|---|---|---|
| `size` | `0.09` | hand width |

**Connectors:** `wrist` (attach to arm), `grip` (held props).

#### `humanoid_foot`

Sole + toe block blended into a boot shape. Uses material `boot` unless the
wrapping group sets `mat=`.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.26` | length along +Z |
| `width` | `0.10` | width |
| `height` | `0.10` | height |

**Connectors:** `ankle` (top), `toe` (front), `heel` (back).

#### `humanoid_face`

`eye_l`, `eye_r`, `nose`, `mouth` as separate top-level nodes, each with a
`back` connector; attach each to the matching `humanoid_head` connector.
Caller declares materials `eye`, `skin`, `mouth`.

| parameter | default | meaning |
|---|---|---|
| `size` | `0.11` | head reference size |

---

### Humanoid animations

Parameterless; each emits one clip (named after the module) driving the
`"rig"` bones of `humanoid_full`. Use exactly one per scene — two would fight
over the same bones.

| module | duration | motion |
|---|---|---|
| `humanoid_idle` | 4.0 s loop | subtle breathing + weight shift |
| `humanoid_walk` | 1.0 s loop | hip swing, opposite-arm swing, knee lift, spine counter-rotation |
| `humanoid_run` | 0.55 s loop | larger amplitudes, elbows at 90° |
| `humanoid_jump` | 1.2 s one-shot | crouch → extend → tuck → land |

---

### Animals

#### `quadruped_torso`

Elongated soft body.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.9` | length along +Z |
| `height` | `0.32` | height |
| `width` | `0.30` | width |

**Connectors:** `neck` (front), `tail` (back), `leg_fl`, `leg_fr`, `leg_bl`,
`leg_br`.

#### `quadruped_leg`

Single capsule, hip to paw.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.45` | length |
| `radius` | `0.045` | radius |

**Connectors:** `top` (tag `hip`), `paw` (bottom).

#### `tail`

Tapered `spline_tube` (~0.4 m, base radius ~0.04 m) curving down then up.
No parameters; resize via group `scale=`. **Connector:** `base`.

#### `ear`

Curved-plane triangular pinna.

| parameter | default | meaning |
|---|---|---|
| `size` | `0.06` | extent (m) |

**Connector:** `base` (head-side edge).

#### `eye`

Spherical eyeball.

| parameter | default | meaning |
|---|---|---|
| `radius` | `0.022` | radius |

**Connector:** `back` (socket-facing pole).

---

### Foliage

#### `leaf`

Curved-plane leaf, slightly cupped. Give its material `double_sided=1`.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.12` | length along +Y |
| `width` | `0.05` | width along X |

**Connector:** `stem`.

#### `branch`

Tapered S-curved branch (~0.6 m, base 0.04 m → tip 0.008 m). No parameters;
compose trees/antlers/vines via group `scale=`/`rot=`.
**Connectors:** `base`, `tip`. For recursive procedural trees use the `branch`
node kind instead ([`dsl.md` §Branch](./dsl.md#branch)).

---

### Detailing modules

#### `bolt_circle`

Ring of cylindrical bolt heads around +Y, sitting on `y=0`.

| parameter | default | meaning |
|---|---|---|
| `count` | `6` | heads |
| `ring_radius` | `0.1` | ring radius |
| `head_radius` | `0.012` | head radius |
| `head_height` | `0.008` | head thickness |

#### `vent_strip`

Stack of parallel slats: slats run along X, stack grows along Y.

| parameter | default | meaning |
|---|---|---|
| `count` | `6` | slats |
| `length` | `0.4` | slat length (X) |
| `slat_thickness` | `0.005` | thickness (Y) |
| `slat_height` | `0.05` | depth (Z) |
| `gap` | `0.012` | spacing |

#### `panel_seam`

Thin line for hard-surface panel joins; top face at `y=0` so it lies flush on
a host's +Y face.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.5` | length (X) |
| `width` | `0.002` | width (Z) |
| `depth` | `0.0005` | depth into host |

#### `rivet_line`

Row of hemispherical rivets along X; bases on `y=0`. `count=1` gives one
centred rivet.

| parameter | default | meaning |
|---|---|---|
| `count` | `8` | rivets |
| `length` | `0.4` | total span (X) |
| `radius` | `0.006` | head radius |

#### `step_taper`

Lathed column with four steps, radius `0.5` → `0.1` over height `1.0`
(nozzles, chimneys, pillars). No parameters; resize with `scale=` or `w/h/d`.

#### `cable`

`spline_tube` spanning 1 m along X with a gravity dip.

| parameter | default | meaning |
|---|---|---|
| `radius` | `0.008` | tube radius |

**Connectors:** `start` (-X), `end` (+X). Span anchors with
`pos=`/`scale=`/`rot=`, or `conform` onto a surface.

#### `chain`

Torus links along X alternating 90°; decorative overlap, not truly interlocked.

| parameter | default | meaning |
|---|---|---|
| `pairs` | `4` | link pairs (2 links each) |
| `link_radius` | `0.05` | link inner radius |
| `wire` | `0.012` | wire radius |

#### `feather_card`

Curved plane cupped along its length (feather, fin ray). Pair with
`alpha_mode="mask", double_sided=1`.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.15` | length (Z) |
| `width` | `0.04` | width (X) |

**Connector:** `stem`.

#### `scale_band`

Ring of outward-cupped curved-plane scales around Y; stack bands along Y for a
body.

| parameter | default | meaning |
|---|---|---|
| `count` | `12` | scales |
| `ring_radius` | `0.06` | ring radius |
| `scale_w` | `0.04` | scale width |
| `scale_h` | `0.06` | scale height |

#### `gear`

Coarse gear: cylindrical hub plus rectangular teeth (not involute).

| parameter | default | meaning |
|---|---|---|
| `teeth` | `16` | teeth |
| `hub_radius` | `0.1` | hub radius |
| `tooth_height` | `0.014` | radial tooth extent |
| `tooth_width` | `0.014` | tangential tooth width |
| `thickness` | `0.02` | thickness (Y) |

---

### Organic shape wrappers

#### `spring`

Helical compression spring along +Y, centred on origin (wraps `coil`).

| parameter | default | meaning |
|---|---|---|
| `radius` | `0.025` | helix radius |
| `length` | `0.1` | height (Y) |
| `turns` | `8` | revolutions |
| `wire_radius` | `0.004` | wire radius |

#### `terrain_patch`

Square `heightfield` with rolling-hill fbm defaults. Craggy: `amplitude≈1.4`,
`segments≈96`. Dunes: `amplitude≈0.3`.

| parameter | default | meaning |
|---|---|---|
| `size` | `4.0` | side length (XZ) |
| `segments` | `64` | grid resolution |
| `amplitude` | `0.6` | relief (Y) |
| `seed` | `1` | noise seed |

#### `blob`

Three offset metaballs smooth-unioned (slime, creature mass, jellyfish), centred
on origin. Needs a CSG-enabled build.

| parameter | default | meaning |
|---|---|---|
| `radius` | `0.30` | per-sphere radius |
| `blend` | `0.15` | smooth-union distance; 0 = hard union |

#### `water_patch`

Flat XZ `curved_plane` with a `wave=` deformer (water, lava, jelly). Water
material e.g. `alpha=0.85, transmission=0.6`. Choppy: `frequency≈1.5`,
`ripple≈0.12`.

| parameter | default | meaning |
|---|---|---|
| `size` | `2.0` | side length (XZ) |
| `segments` | `64` | grid resolution |
| `ripple` | `0.04` | wave amplitude |
| `frequency` | `0.6` | wave frequency along X |

---

### Building fittings

Used by the pascal importer and `building` generator; usable directly. Frames bind named materials; panes/slabs inherit from the parent
group.

| module | params (defaults) | notes |
|---|---|---|
| `door_simple` | `width=0.9, height=2.1` | bottom-anchored in wall plane, +Z outside; frame `door_frame`; connectors `hinge`, `handle` |
| `window_simple` | `width=1.2, height=1.4` | bottom-anchored in wall plane, +Z outside; frame `window_frame` |
| `skylight_simple` | `width=1.0, height=1.0` | on a slab's +Y face; frame `skylight_frame` |
| `stair_simple` | `width=2.0, depth=3.0, rise=2.8, steps=14` | straight flight along +Z from `y=0` to `y=rise` |
| `elevator_shaft_simple` | `width=2.0, depth=2.0, height=8.4` | four open-ended walls centred on origin |
