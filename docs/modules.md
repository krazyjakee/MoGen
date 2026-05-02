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

Complete rigged humanoid in one declaration — torso, head, arms, hands,
legs, feet, face, attached and skinned to a `"rig"` skeleton.

| parameter | default | meaning |
|---|---|---|
| `height` | `1.7` | overall height in meters |

**Caller declares materials:** `skin`, `cloth`, `eye`, `mouth`, `boot`.
Hair is not bundled — `use "humanoid_hair_short"` (or
`humanoid_hair_long`) and attach to the head's `crown` socket if the
figure needs hair. Use only **once** per scene since it owns the global
`"rig"` skeleton; pair with one of the humanoid animation modules below.

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
