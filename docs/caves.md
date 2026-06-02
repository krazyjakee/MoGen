# Cave generation

`cave` is a top-level node kind that expands deterministically into a
traversable cave: a single watertight rock shell with hollow chambers linked by
walkable passages, optionally populated with stalagmites, stalactites, rock
piles, pools and lakes. It is to subterranean environments what `building` is to
architectural floorplates — one editable wrapper, with the entire subtree below
it stamped non-editable because the geometry is a pure function of `seed=` plus
the declared attrs. A rebuild regenerates it, so hand-edits below the wrapper
would not survive.

Like `building`, the surface is **empty caverns only**: props, lighting and
creatures compose via separate `use` calls placed inside the cave, not by the
generator.

---

## How it works

The cave is meshed as **`box − ⋃ carvers`**: an additive bounding block minus
the smooth union of every cavity. The field is evaluated on a voxel grid and
extracted with fast surface nets (the same implicit-field pipeline as `blob`),
giving one organic, watertight rock mesh.

The pipeline, all seeded from `seed=`:

1. **Layers.** Chambers are organised into `levels` **stacked horizontal
   layers** — floors — each occupying its own Y band, separated from the next by
   `level_gap` metres of solid rock. Chamber radius is auto-capped to fit a
   layer's height, so taller caves (or fewer `levels`) allow bigger chambers.

2. **Chamber placement.** Within a layer, `chambers` oblate ellipsoids are
   scattered (oblate `height = radius × chamber_flatten` so floors read gently
   curved). Each keeps a `margin` rock shell from the block faces, and the mix
   of separated vs merged rooms is set by `overlap`:

   - most chambers are rejection-sampled at least `spacing` metres apart, so
     they stay **distinct rooms** (crowded layers shrink a chamber rather than
     letting two merge); but
   - with probability `overlap` a chamber is placed deliberately overlapping a
     same-layer neighbour, so the two smooth-union into one **larger irregular
     cavern**.

   That gives "both overlaid and separated" in one cave. Chambers in *different*
   layers may share XZ (rooms stacked over rooms) — the `level_gap` keeps them
   apart vertically.

3. **Passages.** A minimum spanning tree (+ `loops`) within each layer makes
   every floor a connected, near-flat network. Then `level_links` vertical
   passages join each adjacent layer pair (clamped to ≥ 1 so upper floors are
   reachable), preferring chamber pairs offset enough for a single ≤ `max_slope`
   ramp. The union is **fully traversable** — every chamber reachable from every
   other across all floors.

4. **Slope cap.** Each passage is a capsule carver. Any passage whose direct
   line would exceed `max_slope` (default **45°**) is rebuilt as a **switchback
   ramp**: it zig-zags between the two chamber columns, climbing at most
   `horizontal_run × tan(max_slope)` per leg, so **no walkable surface ever
   exceeds the angle cap** — even when linking distant floors.

5. **Entrances.** `entrances` horizontal mouths are punched out through the
   nearest side face, hosted on the lowest chambers so they land at ground
   level. This is what makes the otherwise-enclosed block enterable.

6. **Roughening.** The shell is displaced along its normals by bounded,
   low-frequency value noise (`roughness`) for a natural stone finish. The
   magnitude is an absolute cap (≤ ~0.35 m), not AABB-relative, so the mesh
   stays watertight regardless of block size.

7. **Decorations.** Independent leaf meshes scattered onto chamber floors /
   ceilings — never carved into the field, so this stage is pure mesh
   construction with no CSG. Each feature's floor / ceiling is found by
   **marching the real carved rock field** (the same `box − ⋃ carvers` the
   shell is meshed from) and sinking the feature slightly into it, so drips and
   boulders sit flush on the surface instead of floating at the geometric
   chamber bound.

The rock shell gets a `Trimesh` collider so a game-engine importer gets working
physics for free; water surfaces are left collider-free so a player can wade in.

---

## DSL surface

```mog
cave "hollow" (
  seed=12,
  size=[34, 20, 34],     // outer rock block [width, height, depth] (m); base on y=0
  chambers=10,           // chambers carved
  levels=3,              // stacked horizontal layers (floors)
  level_gap=2,           // solid rock between layers (m)
  level_links=2,         // vertical ramps per adjacent layer pair
  chamber_min=2.2,       // chamber radius range (m); auto-capped to fit a layer
  chamber_max=3.2,
  spacing=2.5,           // min rock gap between same-layer rooms (m)
  overlap=0.4,           // ~40% of rooms merge into larger caverns; rest stay separate
  chamber_flatten=0.6,   // height = radius × this (< 1 = flatter floors)
  passage_radius=1.3,    // tunnel radius (m)
  loops=2,               // extra connections beyond the spanning tree
  max_slope=45,          // walkable slope cap (degrees)
  roughness=0.4,         // wall noise [0, 1]
  blend=1.5,             // smooth-union radius
  margin=2.5,            // rock thickness around the void (m)
  resolution=112,        // voxel grid (32–224); higher = finer + slower
  entrances=2,           // mouths punched to a side face
  mat="limestone",       // rock material (defaults to cave_rock)
  water_mat="water",     // pool/lake material (defaults to cave_water)

  // Decoration counts (scattered on chamber floors / ceilings):
  stalagmites=14,
  stalactites=10,
  rock_piles=3,
  pools=2,
  lakes=0,
) {
  // Optional per-kind tuning. `kind=` is required; `count` overrides the
  // top-level knob for that kind; min_size/max_size set the size range (m);
  // mat overrides the material.
  feature "spires" (kind=stalagmite, min_size=0.3, max_size=1.1)
  feature "drips"  (kind=stalactite, min_size=0.25, max_size=0.7)
}
```

### Attributes

| attribute | default | effect |
|---|---|---|
| `seed` | `1` | RNG seed; same seed = identical cave. |
| `size` | `[24, 10, 24]` | Outer block `[width, height, depth]` (m); base on `y=0`. |
| `chambers` | `6` | Number of chambers carved. |
| `levels` | `2` | Stacked horizontal layers (floors). |
| `level_gap` | `1.5` | Solid rock kept between layers (m). |
| `level_links` | `1` | Vertical ramps per adjacent layer pair (clamped ≥ 1 when `levels > 1`). |
| `chamber_min`, `chamber_max` | `2.5, 5.0` | Chamber radius range (m). Swapped if reversed; auto-capped to fit a layer's height. |
| `spacing` | `2.0` | Minimum rock gap between same-layer chamber surfaces (m). Crowded layers shrink chambers to keep the gap. |
| `overlap` | `0.35` | Probability `[0,1]` a chamber merges with a same-layer neighbour. `0` = all separate, `1` = all clustered. |
| `chamber_flatten` | `0.6` | Vertical squash (`height = radius × this`), clamped `[0.2, 1.0]`. |
| `passage_radius` | `1.1` | Tunnel radius (m). |
| `loops` | `1` | Extra connections beyond the spanning tree. |
| `max_slope` | `45` | Maximum walkable slope (degrees), clamped `[5, 89]`. |
| `roughness` | `0.35` | Wall-noise amount `[0, 1]`. |
| `blend` | `1.5` | Smooth-union radius (m). |
| `margin` | `2.0` | Rock thickness around the void (m). |
| `resolution` | `96` | Voxel-grid resolution, clamped `[32, 224]`. |
| `entrances` | `1` | Mouths punched out to a side face. |
| `mat` | `cave_rock` | Rock material (auto-stamped grey stone if undeclared). |
| `mat_style` | `""` | Free-text style hint forwarded to texture generation. |
| `water_mat` | `cave_water` | Pool / lake material. |
| `rock_piles`, `pools`, `lakes`, `stalagmites`, `stalactites` | `0` | Decoration counts. |
| `debug_hide_shell` | `0` | Debug cutaway (see below). |

### Debug

`debug_hide_shell=1` slices the front (+Z) half of the rock shell away so the
chamber network, passages and floors are visible in cross-section in the editor
— the cave analogue of `building`'s `debug_hide_roof`. Decorations that land in
the removed half are culled so they don't float in the opened section; the
remaining ones keep the exact positions they have with the shell shown, so the
flag is purely a viewing aid. Remove it before exporting a final asset.

### `feature` children

`cave` accepts only `feature` children. Each tunes one decoration kind:

```
feature "<name>" (kind=stalagmite|stalactite|rock_pile|pool|lake,
                  count=…, min_size=…, max_size=…, mat="<material>")
```

- `kind` (**required**) — which decoration this configures.
- `count` — overrides the matching top-level count knob for that kind.
- `min_size`, `max_size` — size range in metres (radius for spikes / piles,
  surface radius for water).
- `mat` — material override (else `cave_water` for pools/lakes, `cave_rock`
  otherwise).

A top-level count with no matching `feature` uses the kind's default size range.

---

## Output structure

```
hollow (editable wrapper, kind="cave")
├── rock              (mesh, role="cave_rock", Trimesh collider)
└── decorations       (group)
    ├── rock_piles    (group) → rock_pile_0 …
    ├── pools         (group) → pool_0 …       (water material, no collider)
    ├── lakes         (group) → lake_0 …
    ├── stalagmites   (group) → stalagmite_0 …
    └── stalactites   (group) → stalactite_0 …
```

Every node under the wrapper carries `tags=["cave", …]` and is non-editable.

---

## Validation

`mogen check` rejects, on the `cave` node:

- non-`feature` children (`E1201`),
- non-positive `chambers` / `levels` / `chamber_min` / `chamber_max` /
  `passage_radius` / `margin` / `resolution` (`E1207` / `E1208`),
- negative decoration / `loops` / `entrances` / `level_gap` / `level_links`
  counts (`E1202`),
- a `size` component that isn't finite and `> 0` (`E1203`),
- `max_slope` outside `(0, 90)` degrees (`E1204`).

Warnings: `chamber_min > chamber_max` (`W1205`, swapped at lowering),
`roughness` / `chamber_flatten` / `overlap` outside `[0, 1]` (`W1206`, clamped).

On a `feature`: missing or unknown `kind` (`E1211` / `E1212`), a body block
(`E1210`), negative `count` (`E1213`), non-positive `min_size` / `max_size`
(`E1214`).

---

## Notes & limits

- The rock shell is **closed** (a solid block hollowed from within). Entrances
  punch holes through the walls; there is no separate "open top". Place the cave
  in a level and walk in through a mouth.
- Floor *slope* is capped per passage; chamber floors are gently curved (oblate
  ellipsoids) rather than perfectly flat. Lower `chamber_flatten` for flatter
  floors.
- More `levels` means thinner layer bands, so chamber radius is auto-capped to
  fit — stacking many floors shrinks the rooms. For big chambers, raise `size`
  height, drop `levels`, or lower `level_gap`. A `W`-class warning isn't emitted;
  the cap is silent.
- Decorations are anchored to the real carved surface (field-marched) and sunk
  slightly so they sit flush, not floating. They are not collision-tested
  against each other or passages, so a stalactite can occasionally hang near a
  passage mouth. Re-seed if a placement reads badly.
- `resolution` drives both quality and cost — a 26 m cave at `resolution=112`
  is ~75k triangles. Keep it modest for previews, raise it for hero assets.
