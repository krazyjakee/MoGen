# Sweep orientation and path frames

Use `frame_up=[0,1,0]` to make profile Y (height) point toward local +Y.
The hint is projected perpendicular to the starting tangent. Profile X
(width) is height × tangent, so width × height = tangent. All inputs are
in the primitive's authored local space; module and parent transforms apply
to the whole resulting mesh. Deformation and anchoring run afterward.

```mog
scene {
  sweep "flat_armrest" (profile=[[-0.10,-0.02],[0.10,-0.02],[0.10,0.02],[-0.10,0.02]],
    path=[[0,0,0],[0,0.05,0.5],[0,0,1]],frame_up=[0,1,0],roll=[0,0,0])
}
```

Positive `roll` rotates width toward height about the tangent using the
right-hand rule. DSL roll/twist values are degrees; geometry APIs use radians.
Parallel transport carries height along the path without switching reference
axes near vertical. A vertical path can use `frame_up=[0,0,1]`.

`closed=1` requires `frame_up`. Supply at least three distinct controls, with
an optional repeated first point at the end. Periodic Catmull–Rom sampling
avoids an endpoint tangent break. Residual transported twist is distributed
by arc length, so the loop's final frame matches its first. Closed sweeps omit
caps and retain duplicated UV seam vertices with matching lighting normals.
The endpoint scales must agree and total twist plus endpoint roll difference
must be a whole number of turns; otherwise E0130 explains the correction.
For varying closed modulation, repeat the first control and its modulation
at the end. Constant modulation needs only one value.

Zero/parallel hints, repeated adjacent points, undefined/reversing tangents,
non-finite input and incompatible seam settings fail with E0130. Round a
sharp reversal by adding a curved transition. This is a frame contract, not
a self-intersection solver; profiles that cross themselves still need correction.

## Legacy compatibility and conform

Omitting `frame_up` retains version 0's existing sample transport, profile-X
orientation and appearance. There is no automatic migration. The explicit
contract is version 1 and has a distinct geometry-cache identity. Both modes
are identified by session inspection and Studio's construction-frame panel.
The displayed basis is before roll, deformation and anchoring, with the
scene's world transform separately available to inspection.

The shared `PathFrame` representation comes from `conform`: `normal` is the
surface-normal/height direction, and `binormal = tangent × normal`. Explicit
sweeps map width to **minus** that binormal to form a right-handed profile.
Conform's existing axis-map behavior remains unchanged; this documented mapping
avoids an implicit axis swap when deriving surface details.

Review: run `cargo test -p mogen-geom --test path_frames` and render
[`explicit_sweep_frame.mog`](../examples/features/explicit_sweep_frame.mog)
from front/side/three-quarter using matched lighting. Parent-transform and
legacy-appearance regressions live in the DSL test target. Test execution and
render generation are deferred to review.
