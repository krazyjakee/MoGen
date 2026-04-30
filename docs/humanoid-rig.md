# Humanoid rig plan

The repo currently ships two humanoid paths:

1. **Procedural `humanoid_full`** — has a working skeleton, A-pose, and per-region
   materials, but the stacked-capsule limbs blend with `smin` into lumpy, faintly
   alien joints. There is no built-in clip library; the user has to author tracks
   by hand.
2. **Imported variants** (`humanoid_<gender>_<outfit>(_smooth)?`) — nicer
   silhouettes, but ten independently-baked GLBs glued together with no skeleton,
   visible joint gaps, an axis bug (figure faces +Z), and a single `skin`
   material covering everything (no separation between face/hands and clothing).

This document replaces `imported-humanoid-pitfalls.md` (which catalogued the
imported-variant workarounds) with a forward plan: collapse onto the procedural
path, fix it properly, and demote the imported variants to a static-portrait
escape hatch.

- [Goals and non-goals](#goals-and-non-goals)
- [Skeleton hierarchy](#skeleton-hierarchy)
- [Mesh topology — ball-and-shaft joints](#mesh-topology--ball-and-shaft-joints)
- [Material zones](#material-zones)
- [Animation clip library](#animation-clip-library)
- [Imported variants — static-only](#imported-variants--static-only)
- [Visual regression — measuring improvement](#visual-regression--measuring-improvement)
- [Implementation phases](#implementation-phases)
- [Acceptance criteria](#acceptance-criteria)
- [Risks and open questions](#risks-and-open-questions)

---

## Goals and non-goals

**Goals**

- One canonical animated humanoid (`humanoid_full`) with no joint lumps and no
  joint gaps.
- An embedded skeleton rich enough to drive walk / run / jump / idle naturally.
- A stdlib clip library (`humanoid_walk`, `humanoid_run`, `humanoid_jump`,
  `humanoid_idle`) that the LLM can compose with the figure in one or two
  module uses.
- Texture pipeline keeps face / hands as `skin`, body as `cloth`, hair as
  `hair`, footwear as `boot` — already supported by per-material routing,
  needs no work beyond making sure the procedural path is the one being used.

**Non-goals (v1)**

- Re-baking the artist GLBs.
- Inverse kinematics, foot-locking, or contact-aware retargeting.
- Facial animation (jaw / eyes / brows). The face stays static.
- Cloth simulation. Cloth is rigid skin-bound geometry.
- Photoreal anatomy. The bar is "reads as a person at a glance," not "passes
  for a sculpt."

---

## Skeleton hierarchy

The current rig has 13 bones. The new rig has roughly 30, mostly to give the
spine and hands enough articulation for natural motion. All offsets stay
parent-relative and parameterised by `$height`.

```
rig (pos = [0, height * 0.55, 0])
└── hip                                   ← pelvis pivot
    ├── spine_lower                       ← lumbar
    │   └── spine_mid                     ← thoracic
    │       └── spine_chest               ← upper thoracic / clavicle origin
    │           ├── neck
    │           │   └── head              ← drives head_skin / face / hair
    │           ├── clavicle_l
    │           │   └── shoulder_l        ← A-pose offset (sin20°, -cos20°, 0)
    │           │       └── elbow_l
    │           │           └── wrist_l
    │           │               ├── thumb_l_1 → thumb_l_2
    │           │               ├── index_l_1 → index_l_2 → index_l_3
    │           │               ├── middle_l_1 → middle_l_2 → middle_l_3
    │           │               ├── ring_l_1 → ring_l_2 → ring_l_3
    │           │               └── pinky_l_1 → pinky_l_2 → pinky_l_3
    │           └── clavicle_r → … (mirrored)
    ├── hip_l → knee_l → ankle_l → toe_l
    └── hip_r → knee_r → ankle_r → toe_r
```

Notes:

- **Spine subdivision** (`spine_lower / mid / chest`) is what lets the torso
  bend, lean, and rotate independently of the hips during walk/run. With one
  spine bone the torso can only swing rigidly off the pelvis.
- **Clavicle bones** absorb shoulder lift during arm swing so the shoulder
  cap doesn't tear when the arm raises past horizontal.
- **Finger phalanges** are present in the rig but v1 clips don't drive them —
  they exist so a future `grip` clip and per-finger pose presets have
  something to hook into. Default rest pose has all phalanges identity, so
  hands look the same as today out of the box.
- **Toe bones** allow heel-off / toe-off in walk and the crouch in jump
  pre-launch. Without them the foot is a rigid plank and the walk cycle
  reads as shuffling.
- Envelope rule from current `humanoid_full` is preserved: each bone's
  envelope ≈ 0.7× its own length so adjacent bones overlap at shared joints,
  giving ~0.4 / ~0.4 weights on either side instead of one bone at 100%.

The 4-joints-per-vertex glTF cap (`JOINTS_0` / `WEIGHTS_0`) means the auto
weight-binding has to keep only the top 4 contributors per vertex. With 30
bones and overlapping envelopes this matters more than it does today;
`skin_lower.rs` will need a top-4 prune + renormalise step (likely already
present — confirm during Phase 1).

---

## Mesh topology — ball-and-shaft joints

The current limb code is two stacked capsules `smin`-blended together:

```
capsule upper (radius=R)  ──┐
                            ├─ smin → lumpy ovoid where the two capsule
capsule fore  (radius=0.85R)┘         hemispheres collide
```

Replace each limb with a **ball-and-shaft**:

```
icosphere joint_a (radius = 1.05 * shaft_R)
cylinder  shaft   (radius = shaft_R, between joint_a and joint_b)
icosphere joint_b (radius = 1.05 * shaft_R)
```

Why this works:

- The icosphere's radius is strictly greater than the shaft's, so at the
  seam the SDF of the ball dominates and the smin blend produces a clean
  sphere-shaped joint, not a "two cylinders meeting" lump.
- A real human elbow / knee / shoulder visually *is* a ball — anatomy
  matches the topology, so the silhouette reads correctly.
- Each ball is one bone target for skinning. When the elbow rotates, the
  ball stays fixed and only the forearm shaft swings — no shearing across
  the joint.

Concretely, the new arm becomes:

```mog
union "arm_l" (smooth=$height * 0.012, mat="skin", skin="rig") {
  icosphere "shoulder_l_ball" (pos=[0, 0, 0],                 radius=$height * 0.034)
  cylinder  "upper_l"         (pos=[0, $height * -0.08, 0],   radius=$height * 0.029, height=$height * 0.16)
  icosphere "elbow_l_ball"    (pos=[0, $height * -0.16, 0],   radius=$height * 0.030)
  cylinder  "fore_l"          (pos=[0, $height * -0.24, 0],   radius=$height * 0.025, height=$height * 0.16)
  icosphere "wrist_l_ball"    (pos=[0, $height * -0.32, 0],   radius=$height * 0.026)
  connector "shoulder" (at=[0, 0, 0],               dir=[0,  1, 0], tag=shoulder)
  connector "wrist"    (at=[0, $height * -0.32, 0], dir=[0, -1, 0], tag=wrist)
}
```

Same pattern for legs (hip_ball / thigh / knee_ball / shin / ankle_ball),
the spine (hip_ball / spine_lower / spine_mid_ball / spine_mid /
spine_chest_ball / spine_chest), and the neck. Hands keep the existing
palm + finger capsule arrangement — they read fine already.

The smooth radius drops from `0.018 → 0.012` because the ball/shaft seam
needs less blending to look continuous than a lumpy two-capsule junction.

---

## Material zones

The procedural path already has the right zones — keep them and lean on them:

| zone   | parts                                            |
|--------|--------------------------------------------------|
| `skin` | head_skin, face features, arms, hands            |
| `cloth`| torso, legs (or `skin` for tanktop variants)     |
| `hair` | hair_root                                        |
| `eye`  | eye_l, eye_r                                     |
| `mouth`| mouth                                            |
| `boot` | foot_l, foot_r                                   |

The texture generator (`mogen-llm/src/textures.rs`) already walks the AST
and emits one PNG per material, so the LLM gets distinct skin / shirt /
hair textures for free as soon as we stop using the imported single-material
meshes. No changes to the texture pipeline.

If we want clothing variants in the future (suit, dress, tank top), they
become outfit-specific overrides of the `cloth` shape — extra geometry
attached to the torso/leg slots, not a different humanoid.

---

## Animation clip library

Four stdlib modules, one clip each, all keyed off the `"rig"` skeleton's
bone names:

| module           | what it drives                                                                                                         |
|------------------|------------------------------------------------------------------------------------------------------------------------|
| `humanoid_idle`  | Slow breathing on `spine_chest`, weight shift on `hip`, faint hand bob.                                                |
| `humanoid_walk`  | Hip-yaw + opposite-arm swing, knee bend on stance/swing, heel→toe roll on `toe_l/r`, slight `spine_mid` counter-rotate.|
| `humanoid_run`   | Walk geometry but extended: bigger amplitudes, a brief airborne window where both feet are off the ground, torso lean. |
| `humanoid_jump`  | Crouch (knee + hip flex) → extend → tuck → land. Single-shot, not a loop.                                              |

Two open questions for the implementation:

1. **Where does the clip live?** Two options: (a) allow `clip` blocks
   inside `module { … }` and let the user write `use "humanoid_walk"`;
   or (b) add an `anim` builder DSL that resolves bone names to NodeIds
   at lower-time. The grammar is generic enough that (a) works today —
   `module.rs` lowering just needs to accept clip-shaped children. Prefer
   (a); it's a smaller change and keeps everything declarative.
2. **Track emission.** Add to `mogen-anim`:

   ```rust
   pub fn humanoid_walk(rig: &Skeleton, speed: f32, stride: f32) -> Clip
   pub fn humanoid_run(rig: &Skeleton, speed: f32) -> Clip
   pub fn humanoid_jump(rig: &Skeleton) -> Clip
   pub fn humanoid_idle(rig: &Skeleton) -> Clip
   ```

   Each takes the resolved skeleton (so it can map bone names like
   `"hip_l"` to NodeIds), and emits a `Clip` with one rotation track per
   driven bone. The DSL `clip` block in option (a) is the surface; these
   functions are what `anim_lower.rs` calls underneath.

Cycle parameters (rough cuts, tune from reference):

- **walk**: cycle = 1.0 s, hip yaw ±6°, shoulder swing ±25°, elbow flex 20°→55°,
  knee flex 5°→55° on swing, ankle flex 0°→25°, vertical bob 2 cm.
- **run**: cycle = 0.55 s, hip yaw ±10°, shoulder swing ±55°, elbow held 90°,
  knee flex 10°→90°, vertical bob 6 cm, airborne window 30% of cycle.
- **jump**: 1.2 s one-shot, crouch 0.0–0.25 s, extend 0.25–0.45 s, airborne
  + tuck 0.45–0.85 s, land 0.85–1.2 s.
- **idle**: 4.0 s loop, spine_chest bob ±1.5°, hip ±0.8°, hands ±2 mm Y.

---

## Imported variants — static-only

The imported `humanoid_<g>_<o>` modules stay in the stdlib but get
relabelled. They are useful when the user wants a particular silhouette
for a portrait shot and doesn't need motion.

Changes:

- **`docs/modules.md`**: add an explicit "Static portrait — no skeleton,
  no animation, do not use with clips" note next to the imported table.
  Pin `humanoid_full` as the default.
- **LLM system prompt** (`mogen-llm/src/prompt.rs`): when the request
  mentions motion/walking/running/jumping/animation, prefer
  `humanoid_full` + a `humanoid_*` clip; fall back to imported variants
  only for static composition.
- **`docs/imported-humanoid-pitfalls.md`** is replaced by this file but
  the "joint gaps" / "+Z baking" / "scale propagation" sections stay
  useful as an appendix here for callers who *do* use imported variants
  — fold them in under a single "Imported variants quirks" appendix
  rather than a separate doc.
- The 16 part-GLBs and their generators stay on disk; nothing is deleted.

We are not investing further in imported-variant fixes (no bone retrofit,
no re-bake). They work for what they are.

---

## Visual regression — measuring improvement

`mogen render <input.mog> --out <png> [--yaw <deg>] [--pitch <deg>] [--size <px>]`
rasterises a `.mog` to a square PNG using the same offscreen path Studio
uses for thumbnails. We use this as the regression metric — every phase
produces a comparable strip of PNGs, and "did joints stop being lumpy"
is a direct visual check, not a guess from the diff.

### Fixture

`examples/humanoid_rig_test.mog` is the canonical baseline: a barebones
`humanoid_full` at default height with the six required materials and
no gear, no clip. Keep this file pinned — changing it invalidates the
baseline strip.

For motion phases we also render a paired walk fixture once the clip
library lands (`examples/humanoid_walk_test.mog`).

### Render strip

Per phase, render the fixture from four yaws at the standard 28.6° pitch:

```sh
PHASE=baseline   # or phase1, phase2, …
OUT=docs/humanoid-rig/$PHASE
mkdir -p "$OUT"
for yaw in 0 45 90 180; do
  ./scripts/run-mogen.sh render examples/humanoid_rig_test.mog \
    --out "$OUT/rig_${yaw}.png" --yaw $yaw --size 768
done
```

- **yaw 0** — front view: face, chest, hands, knees facing camera.
  Catches material-zone bugs (skin vs cloth boundaries) and head/torso
  proportion changes.
- **yaw 45** — 3/4 view (Studio default): the canonical "is this a
  person" silhouette. Catches joint lumps on the visible side.
- **yaw 90** — pure side view: best for checking spine curvature, hip
  pivot, knee bend, foot/ankle alignment.
- **yaw 180** — back view: catches gear-on-the-wrong-side bugs (relevant
  later when phase 4 demotes imported variants), and any back-of-head /
  hair issues.

Save each strip into `docs/humanoid-rig/<phase>/` (gitignored binaries
are fine — these are reference renders, not goldens).

### What to look for, phase by phase

| phase    | regression check                                                                                    | improvement check                                                                |
|----------|-----------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------|
| baseline | n/a                                                                                                 | none — this is the "before" snapshot                                             |
| phase 1  | front/side silhouette unchanged from baseline (skeleton expansion is internal; geometry is the same) | `dump-scene --json` shows new bones                                              |
| phase 2  | no manifold panic; figure still upright; no missing limbs                                           | yaw 45 + yaw 90 show ball-shaped joints at shoulder / elbow / knee — not lumps   |
| phase 3  | rest-pose render of `humanoid_rig_test` matches phase 2                                             | walk fixture `humanoid_walk_test` shows distinct keyframes when sampled at t=0/0.25/0.5/0.75 |
| phase 4  | imported-variant render unchanged from baseline (their renders shouldn't move)                       | LLM bench produces `humanoid_full` (not imported) on motion prompts              |

### Comparison strip

Once two phases are captured, build a side-by-side strip with ImageMagick:

```sh
magick montage docs/humanoid-rig/baseline/rig_45.png \
               docs/humanoid-rig/phase2/rig_45.png \
       -tile 2x1 -geometry +4+4 docs/humanoid-rig/compare_45.png
```

A four-tile strip across `{baseline, phase1, phase2, phase3}` at yaw 45
is the canonical "did this work" image to drop into the PR description
for each phase.

### Pixel-diff gate (optional, later)

For now visual inspection is enough. If we want a CI gate, add a
`tests/visual.rs` that renders the fixture, opens the committed
baseline PNG, and asserts mean per-pixel L2 < threshold. The render
path is deterministic per scene, so this is straightforward — but not
worth the friction until the rig stabilises.

---

## Implementation phases

Each phase is its own PR. Do not bundle.

### Phase 1 — Skeleton expansion

- Add `spine_lower`, `spine_mid`, `spine_chest`, `clavicle_l/r`, finger
  phalanges, `toe_l/r` to `humanoid_full.mog`.
- Recompute bone offsets so existing world rest positions are preserved
  (the old single `spine` bone's envelope and offset are split across
  the three new spine bones; clavicle absorbs the existing
  shoulder offset's horizontal component).
- Verify `skin_lower.rs` keeps the top-4 weights per vertex and
  renormalises. If not, add it — this is the precondition for shipping
  more bones.
- Goldens update: `humanoid_full` golden GLB will change byte-for-byte;
  capture a new visual baseline.

**Done when**: `mogen build examples/hiker.mog` produces a figure that
visually matches the current output, but with the expanded skeleton
visible in `dump-scene --json`.

### Phase 2 — Ball-and-shaft topology

- Replace stacked-capsule limbs in `humanoid_full.mog` with the
  ball-and-shaft pattern. Same for spine and neck.
- Drop `smooth` from `0.018` to `0.012` (arms) and equivalently for legs.
- Update `humanoid_arm.mog` / `humanoid_leg.mog` stdlib modules to match
  (they're currently used standalone; should also benefit).
- Verify no CSG manifold panics across `height ∈ [1.4, 2.1]`.
- Visual diff: joints should read as discrete spheres on close inspection,
  not "two stretched ovoids meeting."

**Done when**: side-by-side renders of pre-/post-Phase-2 figures show
clean joint silhouettes, and the existing `examples/*.mog` builds pass.

### Phase 3 — Clip library

- Decide on (a) `clip` inside `module` vs (b) builder DSL. Default
  recommendation: (a).
- Add `humanoid_walk`, `humanoid_run`, `humanoid_jump`, `humanoid_idle`
  modules under `crates/mogen-dsl/stdlib/`.
- Add corresponding builder fns in `mogen-anim/src/lib.rs`.
- Wire bone-name → NodeId resolution from `anim_lower.rs` (it already
  does this for the existing hand-authored `track "hip_l"` form).
- Update `docs/modules.md` clip section.

**Done when**: a fresh user can write

```mog
scene {
  use "humanoid_full"
  use "humanoid_walk" (speed=1.0)
}
```

and get a walking GLB that plays cleanly in Godot 4 and the
Khronos sample viewer.

### Phase 4 — Imported variants demoted

- Update `docs/modules.md` to mark imported variants as static-only.
- Append the imported-variant quirks appendix to this file (the three
  patches from the old pitfalls doc).
- Delete `docs/imported-humanoid-pitfalls.md`.
- Update `mogen-llm/src/prompt.rs` to bias toward `humanoid_full` for
  any motion-related request.
- Add a `validate_ast` lint: using `clip` together with an imported
  variant emits a warning ("imported humanoid has no skeleton; clip will
  not animate the figure — use `humanoid_full` instead").

**Done when**: `bench --prompts benches/prompts.txt` produces walking
characters via `humanoid_full`, not imported variants, on motion prompts.

### Phase 5 — Goldens and validation

- Add goldens for `humanoid_walk`, `humanoid_run`, `humanoid_jump`,
  `humanoid_idle` against `humanoid_full`.
- Add an `examples/humanoid_walk.mog` that the LLM examples bundle picks
  up.
- Run the bench gate (≥80% success).

**Done when**: CI is green and the bench prompts that ask for motion
produce playable, looped clips end-to-end.

---

## Acceptance criteria

- A user (or LLM) writes two `use` lines and gets a rigged, animated
  humanoid with no manual tracks.
- Joint silhouettes read as balls, not lumps.
- No visible mesh gaps anywhere on the figure at the default height.
- Skin / cloth / hair / boot textures are visibly distinct on the
  generated PBR maps — no "skin everywhere" output.
- `mogen build` completes without manifold panics on
  `height ∈ [1.4, 2.1]`.
- Imported variants still build (no regression) but the docs and LLM
  prompt route motion prompts to the procedural path.

---

## Status — all phases shipped

| phase | landed | evidence |
|---|---|---|
| Phase 1 — skeleton expansion | ✓ | 21 bones (was 13). Rest-pose render byte-identical to baseline. |
| Phase 2 — ball-and-shaft topology | ✓ | Joints read as discrete balls. `docs/humanoid-rig/phase2/`. |
| Phase 3 — clip library | ✓ | `humanoid_idle`/`walk`/`run`/`jump` modules. GLBs export 14–16 channels each. |
| Phase 4 — imported variants demoted | ✓ | `docs/modules.md` updated, prompt biased to `humanoid_full`, lint W0801 fires. |
| Phase 5 — goldens + validation | ✓ | 5 fixture goldens (`humanoid_rig_test`, `humanoid_idle_test`, `humanoid_walk_test`, `humanoid_run_test`, `humanoid_jump_test`). All workspace tests pass. |

The 4-yaw comparison strip lives at `docs/humanoid-rig/compare_45.png`
(imported / baseline / phase 2 / final, left-to-right). Per-phase renders
are in `docs/humanoid-rig/{baseline,imported_reference,phase1,phase2,phase3,final}/`.

## Risks and open questions

- **JOINTS_0 cap**: glTF allows 4 weights per vertex. With the expanded
  rig some torso vertices will be near the influence of `spine_lower`,
  `spine_mid`, `spine_chest`, *and* `clavicle_l/r` — already 5 bones.
  Top-4 prune + renormalise is mandatory; verify the existing skinning
  path does it before Phase 1.
- **CSG cost**: each ball-and-shaft limb has 5 primitives where the old
  one had 2. Expect roughly 2× CSG time for the figure. If `mogen bench`
  regresses noticeably, switch the limbs to a single SDF-blended union
  inside `mogen-geom` instead of relying on per-primitive `smin`.
- **Default-height tuning**: `humanoid_full` has a comment noting the
  current height of 1.7 is tuned to avoid manifold panics. Topology
  changes will retune that envelope. Sweep `[1.4, 2.1]` in step 0.05
  during Phase 2 and pick a new default that gives the widest safe
  range.
- **Aesthetic gap**: the procedural figure will never beat a sculpted
  artist mesh for a hero shot. The imported variants are the answer for
  that case — keep them available.
- **Clip authoring effort**: getting walk and run to look natural is
  hand-tuning work. Budget for at least one round of iteration after
  Phase 3 lands; the first parameters in this doc are starting points,
  not finals.

---

## Appendix — imported-variant quirks

(Folded in from the previous `imported-humanoid-pitfalls.md`. The 16
imported variants are still in stdlib but no longer have an example
file — `humanoid_full` is the answer for any humanoid the user actually
wants to render. These notes are kept for callers who reach for an
imported variant anyway. Reference renders of the bare imported figure
live in `docs/humanoid-rig/imported_reference/`.)

The 16 imported variants are baked facing **+Z** instead of glTF's −Z,
so AABB-derived `back` / `front` connectors on the torso are flipped
relative to anatomy: attaching gear to `socket="back"` lands it on the
chest. Wrap the figure in a `group (rot=[0, 180, 0])` and use
`socket="front"` to mean "anatomical back."

`attach` preserves the child's local scale, so an accessory declared at
scene scope under a figure with `scale=0.25` shrinks to a quarter size
when reparented. Either move the scale onto a wrapping group (so the
inner module stays at scale=1 and accessories share the figure-local
frame) or pre-multiply accessory dimensions by `1 / scale`.

**T-pose, not A-pose.** The parts are baked with arms straight
horizontal. `humanoid_full` was retuned to A-pose in `4f6470b` and the
two paths no longer match silhouettes — accessory positions, sleeve
geometry, and any pose-sensitive attach offset designed against
`humanoid_full` will be wrong on imported variants. There is no fix
short of re-baking the source GLBs.

**Single material across the whole figure.** Each variant module
(`humanoid_male_casual`, `humanoid_female_dress`, …) wraps all 10 part
meshes in a group with one `mat="skin"`. Whatever clothing or footwear
detail was sculpted into the source — shirt, trousers, shoes — flattens
to the caller-supplied `skin` material. There is no per-region material
routing the way `humanoid_full` provides (`skin` / `cloth` / `hair` /
`eye` / `mouth` / `boot`). For multi-material output, use
`humanoid_full`.

**The 10 part GLBs were baked independently**, so gaps of 1–5 cm appear
at neck / wrist / hip / ankle / shoulder, plus thigh-to-hip seams that
read as black voids in renders. Hand-placed skin-coloured
cylinders/spheres at each joint, in the wrapping group's figure-local
frame, partially close the gaps — but these fillers **do not merge**
with the part meshes (no CSG between the imported `mesh` primitive
and authored primitives), so they sit as separate floating volumes
that plug the gap from common viewing angles but won't read as
continuous geometry under close inspection. The seams are visible in
`docs/humanoid-rig/imported_reference/rig_45.png`.

**Accessory connectors don't match anatomy.** The head's AABB extends
above the scalp (the part mesh includes a forehead/crown bump), so
hats attached via `socket="top"` float ~3–6 cm above the head — an
explicit `offset=-0.05` (or a hand-tuned `crown` connector inside the
wrapping group) is needed to seat them. Similarly, `hand_r`'s AABB
bottom in T-pose is *forward* of the hand rather than below it, so a
pole or hilt attached to `socket="bottom"` extends toward the ground
in front of the foot rather than being held by the fingers. None of
this comes up with `humanoid_full`, whose hands carry an explicit
`grip` connector at palm centre and whose head carries an explicit
`crown` connector at the actual hair-line.

None of these workarounds are necessary for `humanoid_full`, which is
the recommended path.
