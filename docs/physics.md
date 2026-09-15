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

### Inheritance

`phys=` inherits down the hierarchy exactly like `mat=`. Set it on a `group` and
every descendant that doesn't declare its own inherits that substance and weighs
itself; a descendant with its own `phys=` overrides:

```
group "chair" (phys="oak") {         // frame is oak…
  box "seat" (…, phys="cushion")     // …but the seat overrides to foam
  box "back" (…)                     // inherits oak, weighs itself
  cylinder "leg_fl" (…)              // inherits oak
}
```

### Compound bodies

A node that carries a substance but **no mesh of its own** — a `group phys=…`,
whether set directly or inherited — reports the *combined* weight and the
**mass-weighted centre of gravity** of every mesh-bearing descendant, in its own
local frame. So the `chair` group above exports one body whose `weight` is the
whole chair's and whose `center_of_gravity` is the real balance point (pulled up
and back toward the heavy backrest). An engine can treat the group as a single
`RigidBody3D` and the children as its collision shapes. Nested groups each sum
their own subtree without double-counting the shared leaves.

An explicit group `weight=` overrides its computed total, including when the
override is zero. Its centre of gravity still comes from its descendants'
masses and positions. For example:

```mog
physics "oak" (weight=700kg/m3)
scene {
  group "assembly" (phys="oak", weight=5kg, pos=[10,20,30], rot=[0,0,90], scale=2) {
    group "parts" {
      box "light" (size=[4,1,1], x=-1, weight=1kg)
      box "heavy" (size=[4,1,1], x=3, weight=3kg)
    }
  }
}
```

`assembly` weighs exactly 5 kg and has local centre of gravity `[2, 0, 0]`:
`(-1 × 1 + 3 × 3) / (1 + 3) = 2` on X. Its placement, rotation, and scale do
not change that local point. The inner `parts` group still weighs 4 kg. Weight
overrides do not propagate to children, and an enclosing compound sums the
mesh-bearing descendants rather than a nested group's overridden weight.

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
are **typed by dimension**: a mass/weight-per-volume literal is only valid on a
`weight=` attribute. Writing one where a length belongs (`size=[5kg, 1, 1]`,
`rot=[90kg, 0, 0]`) is a dimensional mistake and is rejected at parse time
rather than silently treating the kilograms as metres.

## Auto-computed weight and centre of gravity

You never author a weight number for the common case. From `phys="oak"` and the
node's real mesh, mogen computes:

- **weight** = `weight_per_m3 × volume`, where volume is the true enclosed
  volume of the watertight mesh (divergence theorem over its triangles), scaled
  by the node's world transform — so `scale=2` on a 1 m³ box weighs 8×.
- **centre of gravity** = the mesh's volume centroid (centre of mass for a
  uniform-density solid), in the node's local space.

These calculations run after attach, conform, and skin binding, using the
final geometry and transforms. An explicit weight replaces the calculated mass;
the centre of gravity remains geometry-derived.

A node without its own mesh uses the [compound-body rules](#compound-bodies).
If it has no contributing descendants with a positive total mass, it has no
computed mass or centre of gravity. It still exports its substance properties
and any explicit weight override.

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

`weight` is emitted whenever a computed mass or an explicit override exists;
`center_of_gravity` is emitted whenever a centroid was computed. This includes
meshless compound groups. An empty group with `phys="oak", weight=5kg` exports
its substance and `weight: 5`, but no `center_of_gravity`.
A downstream importer (the companion **godot-mog**) reads this block to build a
`RigidBody3D` + `PhysicsMaterial` with the mass and centre of mass already set —
no hand-authoring. mogen itself stays a plain-glTF producer; the reconstruction
lives engine-side.

Pair `phys=` with `collider="aabb"` (or the auto-stamped colliders on
`building` / `dungeon` / `terrain`) to give the engine both the shape *and* the
mass properties.

## Validation

- **E0105** — `phys="…"` references an undeclared substance.
- **E0214** — a `physics` block without a name.
- **W0211 / W0212 / W0213** — non-positive `weight`, negative `friction`, or
  `bounce` outside `[0, 1]`.
- **W0102** — an unknown attribute on a `physics` block. In particular
  `density=` is *not* accepted — it's the jargon `weight=` replaces.
- **W0215** — a node's flat `weight=` override with no own or inherited
  `phys=`. Without a substance, no physics body is created and the override
  has no effect.

## Mesh-merge

The optional export merge pass (`merge_sibling_meshes` /
`ExportOptions::merge_sibling_meshes`) CSG-unions same-material sibling leaves
into one node. It now **carries a combined physics body** when every merged leaf
shares the same substance: the merged node's weight is the sum and its centre of
gravity the mass-weighted mean, so the union simulates like the parts did. If
the merged leaves have *different* substances (a mix of densities that can't be
one uniform body), physics drops — the same way UVs drop on a mixed-UV merge.

## Limitations and follow-ups

- **Compound bodies read own-mesh descendants.** A group's aggregate sums its
  mesh-bearing descendants. Overrides on meshless groups are retained on
  those groups but do not contribute to an ancestor's aggregate.
- **Inertia tensor is not emitted.** Weight and centre of gravity are computed;
  a full inertia tensor for angular dynamics is a natural next step.
- **Collision shape is separate.** Physics carries mass properties; the
  collision shape still comes from `collider=` / the procedural auto-colliders.
