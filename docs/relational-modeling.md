# Relational modeling (`relate`)

```mog
scene {
  box "seat" (pos=[0,0.6,0],size=[1,0.1,0.7])
  plane "floor" (size=[2,0,2],tags="floating")
  spline_tube "leg" (points=[[0,0,0],[0,0.3,0]],radius=0.025)
  box "foot" (size=[0.08,0.04,0.08])
}
relate (child="leg",target="seat",mode="endpoint",endpoint="end",socket="bottom",insertion=0.015)
relate (child="foot",target="floor",mode="ground",socket="top",clearance=0)
```

## Modes

- `endpoint`: move path `start` or `end` of a `sweep`/`spline_tube`/
  `spline_ribbon` to the target connector and retessellate. Geometry changes;
  the node transform and intermediate controls do not. No miter cut.
- `ground`: translate the child subtree so its lowest mesh vertex along the
  connector normal sits on the connector plane + `clearance`.
- `align`: translate child `plug` (default `bottom`) onto target `socket`
  (default `top`), keeping orientation. Use `attach` for rotation/reparenting.

## Attributes

- `offset=[x,y,z]`: target-local.
- `insertion` / `clearance`: nonnegative world lengths; use one. Destination =
  connector + offset + world normal × (clearance − insertion).
- `tolerance`: default 0.0001, must be > 0.

## Rules

- Resolves after `attach` and `conform`, before skin, colliders, physics.
  Atomic and deterministic.
- E0150: missing/ambiguous references, cycles, conflicting writes.
- A child may have two `endpoint` relations (one per end) or one rigid relation.
- Not allowed inside array/mirror/grid/stack; use explicit module instances.
- `endpoint` rejects closed, anchored, subdivided or deformed paths, conformed
  children, child geometry and prebound skins.
- A connector plane is a semantic location, not proof of contact; use `measure`.
- Changing a driver that alters a locked dependent fails the lock check.
