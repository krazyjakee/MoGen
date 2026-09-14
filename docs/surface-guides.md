# Surface-derived guide curves and details

A named `guide` samples a semantic location on an authored surface. A `sweep`
can reuse that curve with an independent profile, material and editable name.

```mog
scene {
  superellipsoid "cushion" (size=[0.8,0.2,0.65],ew=3,ns=3)
  guide "welt_curve" (target="cushion",section="latitude",level=0,tolerance=0.001)
  sweep "welt" (guide="welt_curve",lift=0.002,
    profile=[[-0.003,-0.003],[0.003,-0.003],[0.003,0.003],[-0.003,0.003]])
}
```

## Supported semantic references

- Superellipsoid `latitude`: `level` is normalized target-local Y, strictly
  between -1 and 1. Zero is the equator. Dimensions and `ew`/`ns` come from the
  same resolved source that builds the cushion. Version 1 supports finite
  positive dimensions and boxiness values at least 0.5.
- Explicit-frame sweep `profile_edge`: `edge` selects an **authored profile
  edge**, not a generated triangle. The guide follows its midpoint along the
  curved frame. Path endpoints resolved by `relate` are included. Version 1
  requires a constant, unrolled profile with explicit `frame_up`.

Points and outward normals are target-local. Latitude winding advances from
+X toward +Z around Y. Profile-edge curves follow the target path's direction.
Periodic curves duplicate their first sample at the end; guided sweeps emit
no duplicate cap and share lighting normals across that UV seam.

`lift` offsets each sampled center along the analytic surface normal in
**target-local units**, matching conform's offset model. Parent scaling scales
that distance; use target dimensions when a fixed world distance is needed.
The sweep uses the shared conform `PathFrame` representation, with height
along the surface normal and width chosen for a right-handed profile.
`reparent=1` (default) follows conform: parent the detail to the target with
identity transform. `reparent=0` preserves its hierarchy and transforms the
mesh into the child's local space. Placement is determined by the guide/lift;
use `lift` instead of child `pos` to specify surface clearance.

## Tolerance, scope and diagnostics

Guides sample their authored analytic definition independently of target
mesh tessellation. `tolerance` (default 0.001 target-local units) bounds the
sampled chord-deviation checks; latitude checks quarter/mid/three-quarter
points, and profile-edge sampling compares successive refinement levels.
At guide samples, the offset is applied directly to the analytic normal;
between samples the swept strip is piecewise linear. This is an analytic
surface contract, not a guarantee that a coarse target triangle mesh coincides
with that surface. Increase target tessellation for rendered comparisons.

Sampling is capped at 4096 segments. Degenerate, unresolved and self-contacting
paths/profiles produce E0160; self-contact tolerance is `tolerance/100`.
Deformed/anchored/subdivided/conformed targets and guide use inside replicators
are rejected in v1. Use explicit module instances for repeated details.
The pass resolves after relationships and before skin, collider and physics
passes, on a temporary graph. Existing compiled locks compare the resulting
geometry and transforms, including details reparented under a locked target.

Fixtures: [guided_cushion.mog](../examples/furniture/guided_cushion.mog) and
[guided_frame_trim.mog](../examples/features/guided_frame_trim.mog). Resize and
change cushion roundness, then compare front/side/three-quarter views with
matched cameras. Review: `cargo test -p mogen-dsl --test surface_guides`.
Test execution and rendered regression captures are deferred to review.
