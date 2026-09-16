# Sweep orientation (`frame_up`)

```mog
scene {
  sweep "flat_armrest" (profile=[[-0.10,-0.02],[0.10,-0.02],[0.10,0.02],[-0.10,0.02]],
    path=[[0,0,0],[0,0.05,0.5],[0,0,1]],frame_up=[0,1,0],roll=[0,0,0])
}
```

- `frame_up`: profile Y (height) points toward this hint, projected
  perpendicular to the start tangent. Profile X (width) = height × tangent.
  Transported along the path (stable near vertical). For a vertical path use
  e.g. `frame_up=[0,0,1]`.
- All inputs are primitive-local; parent/module transforms, deformation and
  anchoring apply afterward.
- `roll`/twist: degrees; positive rotates width toward height (right-hand rule
  about the tangent).
- Without `frame_up`, the older default transport is used (different
  orientation; not migrated).

## Closed sweeps (`closed=1`)

- Requires `frame_up` and ≥ 3 distinct controls (first point may be repeated
  at the end).
- Periodic Catmull–Rom; residual twist spread by arc length; no caps.
- Endpoint scales must match; total twist + endpoint roll difference must be
  whole turns.
- Varying modulation: repeat the first control's value at the end. Constant
  needs one value.

## Errors (E0130)

Zero/parallel hint, repeated adjacent points, undefined/reversing tangents,
non-finite input, incompatible closed-seam settings. Round sharp reversals with
a curved transition. Self-intersecting profiles are not fixed.

Example: `examples/features/explicit_sweep_frame.mog`.
