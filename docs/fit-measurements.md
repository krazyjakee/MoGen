# World-space fit measurements

`inspect` on a `.mog` returns JSON with, per named part (including descendants):
tight world bounds, dimensions and signed ground clearance. Metres; ground is
world Y=0. Uses transformed rendered vertices of static tessellated geometry
(no animation/skinning). Empty parts have no measurement. `.glb` inspection
returns the container summary. E1101 (connectivity) does not block inspection;
other compile errors do.

## Surface distance

```json
{"tool":"measure","revision":"revision from inspect","first":"foot","second":"leg","tolerance":0.002,"max_work":100000}
```

```sh
mogen inspect examples/features/joint_measurements.mog
mogen measure examples/features/joint_measurements.mog --first foot --second leg --tolerance 0.002
```

- `first`/`second` must resolve uniquely to distinct subtrees (not ancestor/descendant).
- `tolerance` default 0.002. `max_work` default 100,000, range 1–1,000,000
  (tree visits + triangle tests). Each part ≤ 100,000 triangles, else error.
- Result: part names, revision, world bounds, dimensions, ground clearance,
  closest points, `direction_first_to_second` (null at zero distance), unsigned
  `distance`, `tolerance`, `lower_bound`, `exact`, `status`, work counts.
- `status`:
  - `within_tolerance` — real surface points are within tolerance (not proof of
    a sound joint).
  - `separated` — `lower_bound` exceeds tolerance.
  - `inconclusive_budget` — budget ran out first.
- `exact=true`: minimum resolved for the current tessellation (not the analytic
  shape). `exact=false`: distance is an upper bound, `lower_bound` brackets it.
- Unsigned: crossing surfaces give 0; a contained solid can give >0. No
  penetration depth, containment, wall thickness or manifoldness. Overlap never
  fails a fit check. AABB overlap is never treated as contact.
- `tags="floating"` opts a prop out of connectivity diagnostics; it can still
  be measured.

## Relationship checks

Inspection rechecks `relate` constraints at current world transforms and
reports intended vs measured offset, residual, tolerance and satisfaction
(insertion = negative offset, clearance = positive).

- `endpoint`/`align`: compare anchors to target socket + offset. Use `measure`
  for actual surface proximity.
- `ground`: project rendered subtree vertices onto the connector plane.
- Failures are reported, not repaired.

## Revisions

Not cached. Source and dependency revisions are checked before and after; any
edit invalidates prior results.

Quality tasks may list up to 16 `measure_pairs` (`["first","second"]`).
Violated relationships fail `constraints_ok`; distances are evidence only.

Example: `examples/features/joint_measurements.mog` (0.01 m gap, touching
blocks, insertion, separated spheres with overlapping AABBs).
