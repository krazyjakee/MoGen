# MoGen module catalog

Modules are parametric sub-graphs: reusable snippets of DSL that take scalar
parameters, expand to a tree of primitives, and can expose connectors for
downstream composition. The full language is documented in
[`dsl.md`](./dsl.md); this page is a catalog of the **known-good modules**
shipped in this repository, with their parameters, connectors, and intended
use.

- [Status of the stdlib](#status-of-the-stdlib)
- [How modules resolve](#how-modules-resolve)
- [Authoring a new module](#authoring-a-new-module)
- [Catalog](#catalog) — `leg`, `slab`, `arm_with_rotor`

---

## Status of the stdlib

The roadmap (§M5) reserves `crates/mogen-modules/stdlib/*.mog` for a shipped
library of common parts — `wall`, `door`, `window`, `limb`, `rotor`, `roof`,
`chassis` seeds, etc. That shared library is **not yet populated**; modules
currently live inline in the example files that use them.

Treat this page as the canonical list of module shapes that have been
validated in the test suite. Copy them verbatim into new `.mog` files until
the shared stdlib lands and resolution falls back to the per-file copy.

The `mogen generate` prompt assembly (`StdlibIndex`) looks modules up from a
shared `ModuleRegistry`, so the same module names will become globally
resolvable once a stdlib path is wired through.

---

## How modules resolve

Given a call `use "leg" (height=0.5)`:

1. Collect every top-level `module "name"` in the current file into a registry.
2. Look up `"leg"` in the registry. Unknown names fail at lowering.
3. Bind caller arguments (`height=0.5`) against the declared parameter list.
4. Fill in declared defaults for any parameter the caller omitted.
5. Expand the module body with `$param` references substituted to their bound values.
6. Recurse: module bodies may themselves call `use`, up to the recursion check.

Expansion happens **before** the scene graph is built — by the time
lowering runs, every `$name` has been replaced with a concrete number and
every `use` node has been replaced with its expanded body.

---

## Authoring a new module

Three rules cover almost everything:

1. **All parameters are scalars.** Numeric defaults are required (`height=0.5`, `count=4`); vec3 or string defaults are rejected. If you want a positioned pose, pass the three components as separate scalars.

2. **Reference parameters as `$name`** inside the body. They compose into expressions in any numeric attribute position — `pos=[0, $h * 0.5, 0]`, `radius=$r`, `height=$h + 0.1`, and so on.

3. **Expose connectors where the caller will join you.** A `leg` that the seat attaches on top of should emit `connector "top" (...)` inside its mesh node. Tagging them (`tag=leg_top`) lets downstream fitting logic pair compatible anchors without hard-coded positions.

A skeleton:

```
module "my_part" (width=1.0, height=1.0, depth=1.0) {
  box "body" (size=[$width, $height, $depth]) {
    connector "top"    (at=[0,  $height * 0.5, 0], dir=[0,  1, 0], tag=part_top)
    connector "bottom" (at=[0, -$height * 0.5, 0], dir=[0, -1, 0], tag=part_bottom)
  }
}
```

---

## Catalog

### `leg`

A cylindrical furniture leg with a connector at the top where a seat or
slab attaches.

| parameter | default | meaning |
|---|---|---|
| `height` | `0.5` | leg length along +Y, in meters |
| `radius` | `0.05` | cross-section radius |

**Connectors:** `top` at `[0, height/2, 0]`, dir `+Y`, tag `leg_top`.

**Source (from `examples/chair_module.mog`):**

```
module "leg" (height=0.5, radius=0.05) {
  cylinder "leg" (pos=[0, $height * 0.5, 0],
                  radius=$radius, height=$height, mat="wood", role="leg") {
    connector "top" (at=[0, $height * 0.5, 0], dir=[0, 1, 0], tag=leg_top)
  }
}
```

**Usage:** see `examples/chair_module.mog` (0.5 m leg, 4× via `array`) and
`examples/table.mog` (0.9 m leg, 4× via `array`).

---

### `slab`

A flat rectangular box — the workhorse for seats, table tops, shelves, and
back panels.

| parameter | default | meaning |
|---|---|---|
| `width` | `1.0` | extent along +X |
| `depth` | `1.0` | extent along +Z |
| `thickness` | `0.1` | extent along +Y |

**Connectors:** none; wrap a `slab` in a `group` to position it and attach
connectors to the group if needed.

**Source (from `examples/chair_module.mog`):**

```
module "slab" (width=1.0, depth=1.0, thickness=0.1) {
  box "slab" (size=[$width, $thickness, $depth])
}
```

**Usage:** `examples/chair_module.mog` uses one `slab` for the seat and
another (rotated) for the back.

---

### `arm_with_rotor`

A quadcopter-style arm: a thin horizontal beam along +X with a motor
cylinder and a rotor group at its tip. Designed to be placed under an
`array (count=4, around=y, start_angle=45)` so the four arms fan out at the
corners of the airframe.

| parameter | default | meaning |
|---|---|---|
| `length` | `0.35` | arm length along +X (meters) |
| `arm_thickness` | `0.02` | square cross-section of the arm |
| `motor_radius` | `0.04` | motor cylinder radius |

**Emits:**

- `arm` — long thin box sitting at `[length/2, 0, 0]`.
- `motor` — short cylinder at `[length, 0.02, 0]`.
- `rotor` — group at `[length, 0.045, 0]` wrapping two blades via `array (count=2, around=y)`.

**Connectors:** none. The `rotor` group can be targeted by name in a `spin`
template to drive all four propellers off a single array wrapper — see
`examples/drone.mog`.

**Source (from `examples/drone.mog`):**

```
module "arm_with_rotor" (length=0.35, arm_thickness=0.02, motor_radius=0.04) {
  box "arm" (pos=[$length * 0.5, 0, 0],
             size=[$length, $arm_thickness, $arm_thickness],
             mat="carbon", role="arm")

  cylinder "motor" (pos=[$length, 0.02, 0],
                    radius=$motor_radius, height=0.03,
                    mat="motor", role="motor")

  group "rotor" (pos=[$length, 0.045, 0], role="rotor") {
    array "blades" (count=2, around=y) {
      box "blade" (pos=[0.12, 0, 0], size=[0.24, 0.006, 0.02],
                   mat="blade", role="blade")
    }
  }
}
```

---

## Seeds for future stdlib modules

The examples imply a few more modules that have not been factored out yet
but would be useful once the shared stdlib lands. Listed so contributors
have a starting point:

| proposed | source pattern | parameters likely wanted |
|---|---|---|
| `wall` | `examples/simple_house.mog` (difference of box + door gap) | `width`, `height`, `thickness`, optional `door_width`, `door_height` |
| `roof_gable` | `examples/simple_house.mog` (two tilted pitches) | `span`, `depth`, `pitch_deg`, `thickness` |
| `door` | `examples/door_open.mog` + `simple_house.mog` | `width`, `height`, `thickness`; expose a `hinge` connector |
| `window` | `examples/simple_house.mog` | `width`, `height`, `pane_thickness` |
| `rotor` | `examples/windmill.mog` + `drone.mog` | `blade_count`, `blade_length`, `blade_thickness`, `hub_radius` |

The pattern for each: copy the snippet from the example, parameterize the
hard-coded numbers, and expose the connectors that make the part composable
with its natural neighbor (wall ↔ door, roof ↔ wall, rotor ↔ shaft).
