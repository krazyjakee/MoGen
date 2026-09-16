# Surface guide curves and details

A `guide` samples a curve on a target surface; a `sweep` with `guide=` runs a
profile along it.

```mog
scene {
  superellipsoid "cushion" (size=[0.8,0.2,0.65],ew=3,ns=3)
  guide "welt_curve" (target="cushion",section="latitude",level=0,tolerance=0.001)
  sweep "welt" (guide="welt_curve",lift=0.002,
    profile=[[-0.003,-0.003],[0.003,-0.003],[0.003,0.003],[-0.003,0.003]])
}
```

## `guide` sections

- `section="latitude"` on `superellipsoid`: `level` = normalized local Y, strictly
  in (-1, 1); 0 is the equator. Requires finite positive size and `ew`/`ns` ≥ 0.5.
  Winds from +X toward +Z around Y.
- `section="profile_edge"` on an explicit-frame `sweep`: `edge` indexes an
  authored profile edge; the curve follows its midpoint along the path (including
  `relate`-resolved endpoints). Requires constant, unrolled profile and `frame_up`.
- `tolerance` (default 0.001, target-local): chord-deviation bound. Samples the
  analytic surface, not the mesh — raise target tessellation if the mesh must match.
- Max 4096 segments. Closed curves repeat the first sample; guided sweeps then
  have no caps and smooth normals across the seam.

## Guided `sweep`

- `lift`: offset along the surface normal in target-local units (parent scale
  applies). Use `lift`, not `pos`, for clearance.
- Profile Y = surface normal, X = width (right-handed).
- `reparent=1` (default): parent to target with identity transform.
  `reparent=0`: keep hierarchy, mesh converted into the child's local space.

## Constraints and errors

- E0160: degenerate, unresolved or self-contacting path/profile
  (self-contact tolerance = `tolerance/100`).
- Rejected: deformed/anchored/subdivided/conformed targets; guides inside
  array/mirror/grid/stack replicators (use explicit module instances).
- Resolves after `relate`, before skin/collider/physics.

Examples: `examples/furniture/guided_cushion.mog`,
`examples/features/guided_frame_trim.mog`.
