# Physics

mogen does not run a physics simulation. What it *does* do is attach the data
an engine needs to **reconstruct** one — and, because it builds real watertight
geometry, compute the parts you'd otherwise author by hand: an object's weight
and its centre of gravity.

The surface is deliberately small: one reusable declaration (`physics`), one
attribute to reference it (`phys=`), and human words throughout — `weight`, not
"density"; `bounce`, not "restitution".

- [The `physics` block](#the-physics-block)
- [Referencing it: `phys=`](#referencing-it-phys)
- [Weight units](#weight-units)
- [Auto-computed weight and centre of gravity](#auto-computed-weight-and-centre-of-gravity)
- [What ends up in the glTF](#what-ends-up-in-the-gltf)
- [Validation](#validation)
- [Limitations and follow-ups](#limitations-and-follow-ups)

---

## The `physics` block

A named, reusable **substance**, declared next to `material` at the top of the
file or inside `scene { … }`:

```
physics "oak"    (weight=700kg/m3,  friction=0.55, bounce=0.15)
physics "steel"  (weight=7850kg/m3, friction=0.42, bounce=0.35)
physics "rubber" (weight=1100kg/m3, friction=1.0,  bounce=0.9)
```

| attribute | value | default | meaning |
|---|---|---|---|
| `weight` | weight **per cubic metre** (`700kg/m3`) | `1000kg/m3` | how heavy the substance is. This is density, spelled the way you read it out loud. Multiplied by an object's real volume to get its weight. |
| `friction` | `0`…`1`+ | `0.5` | surface grip. `0` is frictionless ice, `1` is grippy rubber; some engines accept `>1`. |
| `bounce` | `0`…`1` | `0.0` | bounciness (restitution). `0` is a dead thud, `1` is a superball. |

The name (`"oak"`) is just a label — it carries no built-in constants. Two
substances with the same properties but different names are distinct, and (like
materials) a substance is scoped to the file that declared it, so imported
`.mog` files keep their own.

`mat=` (the *look*) and `phys=` (the *feel*) are independent. A wood-textured
steel crate is `mat="wood", phys="steel"`.

## Referencing it: `phys=`

Any geometry node — or a `group` — takes one attribute:

```
scene {
  box "crate" (size=[1, 1, 1], mat="wood", phys="oak")   // → weighs 700 kg
  box "block" (size=2,          mat="iron", phys="steel")  // → weighs 62,800 kg
}
```

A per-node `weight=` is an optional **flat mass override** (a mass, not a
per-volume figure) for the rare object whose weight shouldn't follow from its
size — a hollow prop, a scripted "magic anvil":

```
box "prop" (size=[1,1,1], phys="oak", weight=5kg)   // exactly 5 kg, not 700
```

Physics does not inherit down the hierarchy in this version — put `phys=` on the
nodes that carry the mesh. (Inheritance from an ancestor, like `mat=`, is a
planned follow-up.)

## Weight units

Weights work exactly like [lengths](./dsl.md#length-units): append a suffix, and
everything normalises to a base unit at parse time (here, the **kilogram**). A
bare number is kilograms.

| suffix | unit | kg |
|---|---|---|
| `g` | gram | 0.001 |
| `kg` | kilogram | 1.0 |
| `t` | tonne | 1000 |
| `lb` | pound | 0.4536 |
| `oz` | ounce | 0.02835 |
| `st` | stone | 6.350 |

The **per-cubic-metre** form appends `/m3` (or `/m³`): `700kg/m3`, `0.7t/m3`,
`62lb/m3`. That is the value a `physics` block's `weight=` expects. A bare
`weight=` on a *node* is a flat mass (`5kg`, `180lb`).

Weight and length suffixes are disjoint, so a literal is never ambiguous. Units
are typed by dimension: a mass literal is only meaningful in a `weight=`, and
writing one where a length belongs (`size=[5kg, 1, 1]`) is a mistake — full
dimensional-mismatch diagnostics are a planned follow-up.

## Auto-computed weight and centre of gravity

You never author a weight number for the common case. From `phys="oak"` and the
node's real mesh, mogen computes:

- **weight** = `weight_per_m3 × volume`, where volume is the true enclosed
  volume of the watertight mesh (divergence theorem over its triangles), scaled
  by the node's world transform — so `scale=2` on a 1 m³ box weighs 8×.
- **centre of gravity** = the mesh's volume centroid (centre of mass for a
  uniform-density solid), in the node's local space.

Both are deterministic functions of the geometry: same source ⇒ same numbers.
A node with `phys=` but no mesh (e.g. on a bare `group`) still exports the
substance's feel; it just carries no computed weight or centre of gravity.

## What ends up in the glTF

Each physics-bearing node gets a `physics` object under glTF `node.extras`,
sitting alongside the existing `role` / `tags` / `collider` metadata:

```json
"extras": {
  "physics": {
    "material": "oak",
    "weight_per_m3": 700.0,
    "friction": 0.55,
    "bounce": 0.15,
    "weight": 700.0,
    "center_of_gravity": [0.0, 0.0, 0.0]
  }
}
```

`weight` and `center_of_gravity` are omitted when the node has no mesh to weigh.
A downstream importer (the companion **godot-mog**) reads this block to build a
`RigidBody3D` + `PhysicsMaterial` with the mass and centre of mass already set —
no hand-authoring. mogen itself stays a plain-glTF producer; the reconstruction
lives engine-side.

Pair `phys=` with `collider="aabb"` (or the auto-stamped colliders on
`building` / `dungeon` / `terrain`) to give the engine both the shape *and* the
mass properties.

## Validation

- **E0105** — `phys="…"` references an undeclared substance.
- **E0210** — a `physics` block without a name.
- **W0211 / W0212 / W0213** — non-positive `weight`, negative `friction`, or
  `bounce` outside `[0, 1]`.
- **W0102** — an unknown attribute on a `physics` block. In particular
  `density=` is *not* accepted — it's the jargon `weight=` replaces.

## Limitations and follow-ups

- **No inheritance yet.** `phys=` binds only the node it's written on.
- **Per-mesh, not compound.** Each mesh node computes its own weight and centre
  of gravity. A group-level *combined* (mass-weighted) centre of mass across a
  multi-part body is a natural next step.
- **Mesh-merge drops it.** The optional export merge pass
  (`merge_sibling_meshes`) that CSG-unions same-material siblings produces a new
  node without physics — the same way it drops per-vertex UVs. Unmerged nodes
  keep their physics.
- **No dimensional-mismatch errors yet.** Mass units are accepted anywhere a
  number is; using one outside `weight=` is nonsensical but not yet flagged.
