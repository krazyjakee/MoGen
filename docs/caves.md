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

1. **Chamber placement.** `chambers` oblate ellipsoids are scattered inside the
   block, distributed across `levels` vertical bands. Oblate (`height =
   radius × chamber_flatten`) so each chamber floor reads as gently curved
   rather than a deep bowl. Every chamber keeps a `margin` rock shell away from
   the block faces. Because bands overlap, cavities at different heights merge
   into **multiple connected floors**.

2. **Passages.** A minimum spanning tree over the chamber centres guarantees the
   whole cave is **traversable** — every chamber reachable from every other.
   `loops` extra short edges add cycles so the layout isn't a pure tree.

3. **Slope cap.** Each passage is a capsule carver. Any passage whose direct
   line would exceed `max_slope` (default **45°**) is rebuilt as a **switchback
   ramp**: it zig-zags between the two chamber columns, climbing at most
   `horizontal_run × tan(max_slope)` per leg, so **no walkable surface ever
   exceeds the angle cap** — even when linking distant floors.

4. **Entrances.** `entrances` horizontal mouths are punched out through the
   nearest side face, hosted on the lowest chambers so they land at ground
   level. This is what makes the otherwise-enclosed block enterable.

5. **Roughening.** The shell is displaced along its normals by bounded,
   low-frequency value noise (`roughness`) for a natural stone finish. The
   magnitude is an absolute cap (≤ ~0.35 m), not AABB-relative, so the mesh
   stays watertight regardless of block size.

6. **Decorations.** Independent leaf meshes scattered onto chamber floors /
   ceilings — never carved into the field, so this stage is pure mesh
   construction with no CSG.

The rock shell gets a `Trimesh` collider so a game-engine importer gets working
physics for free; water surfaces are left collider-free so a player can wade in.

---

## DSL surface

```mog
cave "hollow" (
  seed=12,
  size=[26, 12, 26],     // outer rock block [width, height, depth] (m); base on y=0
  chambers=7,            // chambers carved
  levels=2,              // vertical bands → overlapping floors
  chamber_min=3,         // chamber radius range (m)
  chamber_max=5.5,
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
| `levels` | `2` | Vertical bands chambers are spread across. |
| `chamber_min`, `chamber_max` | `2.5, 5.0` | Chamber radius range (m). Swapped if reversed. |
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
- negative decoration / `loops` / `entrances` counts (`E1202`),
- a `size` component that isn't finite and `> 0` (`E1203`),
- `max_slope` outside `(0, 90)` degrees (`E1204`).

Warnings: `chamber_min > chamber_max` (`W1205`, swapped at lowering),
`roughness` / `chamber_flatten` outside `[0, 1]` (`W1206`, clamped).

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
- Decorations are placed on chamber floor/ceiling discs; they are not
  collision-tested against passages, so a stalactite can occasionally hang near
  a passage mouth. Re-seed if a placement reads badly.
- `resolution` drives both quality and cost — a 26 m cave at `resolution=112`
  is ~75k triangles. Keep it modest for previews, raise it for hero assets.
