# Part Connection Ontology

Design note for a grid-based procedural **part system**: a catalog of blocky ("Mojang-style")
parts, each authored as a mogen **module** whose oriented **connectors** let an algorithm snap
parts together into good-looking models. This is *not* a Lego reproduction — the geometry is our
own, coarse and voxel-pitched. What we borrow is the **connection vocabulary**: the finite,
free-to-use set of *ways two parts can mate*.

The vocabulary is grounded in the real LDraw/LDCad connectivity metadata (the LDCad *shadow
library*), then re-expressed against `mogen-core::Connector` (`pos + Quat + tag + optional
radius`).

---

## 1. What the real data actually does (and why it's simpler than expected)

LDCad describes every official Lego connection with just **five** snap metas — not the ~8 "part
types" you'd guess. Most connections collapse into one parametric cylinder:

| LDCad meta  | Describes                        | Gender      | Key parameters                                  | Mates with                         |
|-------------|----------------------------------|-------------|-------------------------------------------------|------------------------------------|
| `SNAP_CYL`  | holes & pegs (the workhorse)     | male/female | radius, length, **cross-section** R/A/S, caps   | opposite-gender `SNAP_CYL`         |
| `SNAP_CLP`  | clip-like shapes                 | always female | inner radius, length                          | male `SNAP_CYL`                    |
| `SNAP_FGR`  | interlocking hinge fingers       | M/F ordering | finger widths, outer radius                    | other `SNAP_FGR` only              |
| `SNAP_SPH`  | ball joints                      | male/female | radius                                          | opposite-gender `SNAP_SPH`         |
| `SNAP_GEN`  | odd one-offs (plugs, glass)      | male/female | bounding shape + **group name**                 | same-group `SNAP_GEN` only         |

**The load-bearing insight:** studs, anti-studs, pins, and axles are *all* `SNAP_CYL`. They differ
only by **radius** and **cross-section profile** (`R` round, `A` axle-cross, `S` square) and
**gender** (peg vs. hole). Lego's apparent variety is one primitive with parameters.

This is why our `Connector` — `pos + Quat + tag + optional radius` — is already the right shape:
the `radius` field *is* the `SNAP_CYL` radius; the `tag` carries kind + profile + gender; the
`Quat` gives the mating axis the attach solver aligns.

---

## 2. The connector tag scheme

A connector's `tag` is a structured, colon-delimited string:

```
<kind>:<profile>:<gender>[:<key>]
```

- **`kind`** — connection family: `stud` `pin` `axle` `clip` `bar` `hinge` `ball` `rail` `gear` `gen`
- **`profile`** — cross-section / shape discriminator: `round` `cross` `square` (only meaningful for
  cylinder-family kinds; use `-` otherwise)
- **`gender`** — `m` (male / protruding / peg) or `f` (female / receiving / hole); `n` (neutral) for
  symmetric mates like gears
- **`key`** — optional group name for `gen` connectors and for size-class opt-in (see §4)

The `radius` field on the `Connector` carries the physical size; the tag carries the *type*. Both
participate in the mate test.

Examples:

| Connector                       | Meaning                                         |
|---------------------------------|-------------------------------------------------|
| `stud:round:m`  (radius = R_stud) | a stud on top of a brick                        |
| `stud:round:f`  (radius = R_stud) | an anti-stud / tube socket underneath a brick   |
| `pin:round:m`                    | a friction pin peg                              |
| `axle:cross:f`                   | an axle hole (transmits rotation)               |
| `clip:-:f`                       | a clip (grips a bar)                            |
| `bar:round:m`                    | a bar (gripped by clips, or entered by cylinders)|
| `hinge:-:m`                      | a hinge knuckle, male finger ordering           |
| `ball:-:m` / `socket:-:f`        | ball-and-socket pair                            |
| `gen:-:m:usb`                    | a keyed generic plug in group `usb`             |

---

## 3. The mate predicate

Two connectors **A** and **B** may snap iff **all** hold:

1. **Kind compatibility** — `kind(A)` and `kind(B)` are a legal pair (table below).
2. **Gender opposition** — `{m, f}` for cylinder/ball/clip families; `{n, n}` allowed for gears.
3. **Profile match** — for cylinder-family kinds, `profile(A) == profile(B)`
   (a round pin does not enter an axle-cross hole).
4. **Size match** — `|radius(A) − radius(B)| ≤ ε`, *or* both carry the same size-class `key`.
5. **Frame opposition** — the connector local **+Z axes point into each other** (anti-parallel);
   the attach solver already computes the rigid transform that makes this true.

### Legal kind pairs

| kind A  | mates with kind B      | notes                                                        |
|---------|------------------------|-------------------------------------------------------------|
| `stud`  | `stud` (opp. gender)   | the primary building grid; peg-into-tube                    |
| `pin`   | `pin`                  | friction connector; round profile                           |
| `axle`  | `axle`                 | cross profile; the mate can transmit rotation               |
| `clip`  | `bar`, `pin`, `stud`   | clip is always `f`; grips any male round cylinder           |
| `bar`   | `clip`                 | bar is `m`; also enterable by female round cylinders        |
| `hinge` | `hinge`                | interleaved fingers; alternating M/F ordering, mate in-plane|
| `ball`  | `socket`               | 3-DOF rotation retained after mate                          |
| `rail`  | `rail`                 | opp. gender; 1-DOF slide retained after mate                |
| `gear`  | `gear`                 | neutral gender; mate = teeth mesh at pitch-radius distance  |
| `gen`   | `gen` (same `key`)     | matched by group name only; fully custom pairs              |

Everything not in this table is **incompatible** — the generator never proposes it.

---

## 4. The grid — the one constraint that makes it look right

Lego's visual coherence comes from a fixed ratio baked into every part: **stud pitch : plate
height = 5 : 2** (in LDraw units, 20 LDU wide × 8 LDU tall per plate). Everything is a multiple.

We adopt the same discipline with our own unit. Define:

- **`U`** — the grid pitch (horizontal stud spacing). One "cell" is `U × U`.
- **`H`** — the layer height. Fix `H / U = 2 / 5` to inherit Lego's proportions, or pick our own
  ratio — but **pick exactly one and make every part a multiple of it.**

Rules that follow:

- Every connector sits at a grid coordinate: `pos = (i·U, j·H, k·U)` for integers `i, j, k`.
- Every part's bounding footprint is an integer number of cells.
- **Size classes** (the `key` field) are named radii on the grid — e.g. `stud` radius is a fixed
  fraction of `U`, `pin`/`axle` another. Two connectors with the same size-class `key` match in
  step 4 without a floating-point radius compare.

On-grid connectors are what make both **snapping** deterministic *and* **greedy meshing** possible
(next section). The grid is not decoration — it is the precondition for the whole pipeline.

---

## 5. Two layers: semantic parts vs. merged geometry

The part graph and the shipped geometry are **different layers** and must stay decoupled:

- **Semantic layer** — parts + connectors. One node per part. This is where snapping, the "part 253
  connects to part 231" structure, and editing live. Legible, algorithm-friendly.
- **Geometry layer** — what actually lands in the `.glb`. Minimal triangles; adjacent same-material
  parts fused; interior faces gone.

Part count is a *generation-time* abstraction; merging is a *compile-time* optimization that
discards the abstraction once it has done its job. Two collapse strategies, both watertight
(required — we never ship holes):

1. **Greedy meshing** *(preferred for grid-aligned flat/box runs)* — the canonical voxel-world
   technique: sweep the grid and coalesce runs of identical adjacent faces into the largest
   rectangle possible. A 10×10 tile field becomes **one quad**, not ten unioned boxes. Crucially it
   **preserves tiling UVs**, so textured parts survive the merge. Viable *because* parts are
   grid-aligned — the §4 grid pays off twice.
2. **CSG union** *(fallback for non-grid or overlapping bits)* — this already exists as
   `mogen-export/merge.rs`: CSG-unions same-material, non-skinned sibling leaf meshes into one
   watertight node. General-purpose, but drops per-vertex UVs on merged groups.

Because the aesthetic wants flat per-face materials anyway, nothing of value is lost in the
collapse.

### Verified behaviour + the deep solid merge (2026-07-09)

Tested against `examples/parts/lego_bricks.mog`. Note the `mogen build` *summary line* reports
pre-merge scene counts — always `inspect` the GLB to see the merged result.

**The original gap:** `is_mergeable` (in `merge.rs`) rejects any node with children, and a `use`
always wraps its geometry in a group (the module-body group *plus* `use`'s implicit transform
group). So module-placed parts were never *direct leaf children* of the `solid`, and the shallow
merge couldn't reach them — the demo built to 30 separate nodes.

**Resolved — deep solid merge** (`merge.rs`, `merge_solid_groups`): when a `solid`'s *entire*
subtree is mergeable leaves (no skins / colliders / slots / protected nodes / non-manifold meshes /
`cast_shadow=false` opt-outs), the pass now collapses **all same-material descendant leaves** — not
just direct children — into one CSG-unioned mesh per material, baking each leaf's transform relative
to the solid and flattening the intermediate groups away. Any keeper in the subtree disqualifies the
solid, which falls back to the safe shallow direct-child merge. Watertight by construction (same
`try_union_many` → `clean_csg_output` path as in-DSL CSG).

Result on the demo: **30 nodes / 5 meshes → 8 nodes / 3 meshes** — each `solid` course collapses to
one `merged_<material>` leaf; the standalone tile (not in a `solid`) stays separate, as intended.

**Still ahead:** **greedy meshing** as a grid-aware pass — the UV-preserving win for large flat
same-material fields, where CSG union drops per-vertex UVs. The deep merge unblocks the part model
today; greedy meshing is the later optimisation for textured flat runs.

---

## 6. How this lands in mogen

- **A part is a `module`** (`module "name" (p=default) { … }`) whose declared `connector`s use the
  §2 tag scheme and sit at §4 grid coordinates.
- **A part catalog is a stdlib of such modules** — the algorithm's building blocks.
- **The generator's job reduces to**: pick parts → find compatible connector pairs (the §3
  predicate) → `attach` (the existing connector-frame solver aligns the frames) → repeat.
- **Export merges** via greedy-mesh + CSG-union fallback, leaving the semantic graph intact for
  re-editing.

No new subsystem — this is an extension of modules + connectors + attach + merge, all of which
already exist.

---

## 7. Open decisions

- Exact value of `U` and the `H/U` ratio (adopt 2/5, or choose our own).
- The named size-class set (how many distinct connector radii the catalog uses).
- Whether `gear` and `rail` ship in v1 (they retain a DOF after mating — only meaningful for
  animated/mechanical models; static scenery may not need them).
- Whether greedy meshing lands as a new export pass or an upgrade to `merge.rs` (the UV-preserving
  win for flat same-material fields; the deep solid merge in §5 already handles the part-collapse
  case, but via CSG union which drops per-vertex UVs).

---

## Sources

- [LDCad metas](https://www.melkert.net/LDCad/tech/meta) — authoritative snap-meta reference.
- [LDCad shadow library](https://www.melkert.net/LDCad/tech/shadowLib) — how connectivity metadata
  overlays LDraw parts.
- [Part Snapping Language Extension — LDraw.org Wiki](https://wiki.ldraw.org/wiki/Part_Snapping_Language_Extension)
- [LDCadShadowLibrary (GitHub)](https://github.com/RolandMelkert/LDCadShadowLibrary) — the real
  connectivity data set.
