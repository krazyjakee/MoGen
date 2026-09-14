# World-space fit measurements

`inspect` now reports tight world-space bounds, dimensions and signed ground
clearance for each named part and its descendants. Values are metres; the
implicit ground plane is world Y=0. Bounds use transformed rendered vertices,
not rotated local bounding-box corners. Empty surfaces have no measurement.
These describe static tessellated geometry, before animation or runtime skinning.

For an actual surface gap, request a pair:

```json
{"tool":"measure","revision":"revision from inspect","first":"foot","second":"leg","tolerance":0.002,"max_work":100000}
```

The same operations are available to humans and MCP clients:

```sh
mogen inspect examples/features/joint_measurements.mog
mogen measure examples/features/joint_measurements.mog --first foot --second leg --tolerance 0.002
```

Names must resolve uniquely and identify distinct subtrees. A parent and its
own descendant cannot form a pair. A `.mog` inspection returns structured JSON;
`.glb` inspection retains its container summary. Connectivity diagnostic E1101
is included in inspection but does not prevent measurement of the broken joint.
Other compilation errors and invalid renderable geometry still block inspection.

## Evidence and accuracy

The result carries part names, current source/dependency revision, world bounds,
dimensions, ground clearance, closest points, direction from first to second,
unsigned distance, tolerance, lower bound, `exact`, status, and work counts.
`direction_first_to_second` is null when the measured distance is zero.

AABB distance only prunes the search. The narrow phase measures triangle
vertex/face and edge/edge distances, including edge/face intersection. It handles
separated curved parts whose AABBs overlap. No AABB overlap is called contact.

- `within_tolerance`: actual points on the two triangle surfaces are within the
  requested tolerance. This does not establish a mechanically sound joint.
- `separated`: the lower bound exceeds tolerance, so the two surfaces are separated.
- `inconclusive_budget`: the budget ran out before either claim could be proved.

`exact=true` means the minimum was resolved for the current tessellation, subject
to floating-point precision. Region tests and distance calculations use f64;
scene vertices and returned points use f32. It does not promise distance to the
underlying analytic shape. With `exact=false`, distance is a measured upper bound
and `lower_bound` brackets the true minimum. A result can prove that surfaces are
within tolerance without resolving the global minimum.

This is an **unsigned surface distance**. Two crossing surfaces have zero
distance. A completely contained solid can have positive boundary distance.
Neither result reports signed penetration, containment, wall thickness or
manifoldness. Intentional joinery and layered upholstery are allowed; overlap
alone never fails a fit check. Independent props may use `tags="floating"` to
opt out of connectivity diagnostics. Measurement is selectable even for such props.

## Authored intentions

Inspection rechecks `relate` constraints using current world transforms:

- Endpoint and alignment checks compare authored semantic anchors to the target
  socket plus intended clearance or insertion. These checks establish anchor
  placement; request surface measurement to establish actual triangle proximity.
- Ground checks project the actual rendered subtree vertices onto the authored
  connector plane. This plane can be transformed and need not be world Y=0.
- Insertion is a negative signed offset, clearance a positive one. The result
  reports intended and measured offsets, residual, tolerance, and satisfaction.

Parameter edits recompile and resolve relationships before measurement. Changing
parent transforms is reflected in newly computed points and normals. Failed
constraints are reported as evidence, not repaired by the measurement operation.

## Runtime and revisions

Measurements run only when requested, with no persistent cache. A median-split
bounding-volume tree prunes distant triangle pairs. Each part is limited to
100,000 triangles; exceeding this fails with a request to select a smaller part.
The default search budget is 100,000 tree visits plus triangle tests; callers may
choose 1–1,000,000. Compilation and linear world-bound extraction are outside
that search budget. Tree construction is bounded by the triangle cap. Exhausted
searches return the remaining lower bound and never invent an exact result.

Source and imported-dependency revisions are checked before and after the
operation. Edits invalidate prior results; tool JSON serialization retains the
revision. Inspection does not silently reuse measurements from an older graph.

The existing quality evaluator records world measurements and relationship
checks. A task can select up to 16 `measure_pairs`, each `["first","second"]`;
results carry the source/dependency revision. Surface distances are evidence,
not aesthetic thresholds. Violated authored relationships fail `constraints_ok`.

## Review fixtures

`examples/features/joint_measurements.mog` includes a 0.01m foot-to-leg gap,
touching blocks, intended insertion, and separated spheres with overlapping
AABBs. Geometric tests cover piercing intersections and bounded search. Session
regressions cover JSON, revisions, modules/parent transforms, and constraint
remeasurement. Execute these and inspect the fixtures during review; broad
builds and rendered acceptance comparisons were deferred during implementation.
