// A simple T-pose biped, assembled with `attach`.
//
// Every part is built at the origin in its canonical orientation. `attach`
// joins them by named connectors — no `pos=` arithmetic on any part. To make
// it taller, wider, or blockier, change a `size`/`radius`/`height` and every
// join still lands correctly.

material "skin"  (color=[0.90, 0.75, 0.62], roughness=0.7)
material "cloth" (color=[0.25, 0.35, 0.60], roughness=0.9)

scene {
  // Torso has custom hip connectors on its bottom face so the two legs
  // land on opposite sides instead of stacking on one point.
  box "body" (size=[0.6, 1.0, 0.3], mat="cloth") {
    connector "hip_l" (at=[-0.15, -0.5, 0], dir=[0, -1, 0])
    connector "hip_r" (at=[ 0.15, -0.5, 0], dir=[0, -1, 0])
  }

  sphere   "head"  (radius=0.25, mat="skin")
  cylinder "arm_l" (radius=0.08, height=0.8, mat="skin")
  cylinder "arm_r" (radius=0.08, height=0.8, mat="skin")
  cylinder "leg_l" (radius=0.11, height=0.9, mat="cloth")
  cylinder "leg_r" (radius=0.11, height=0.9, mat="cloth")

  // Head tops the body (default socket=top, plug=bottom).
  attach (parent="body", child="head")

  // Arms stick out horizontally in T-pose. `left` socket points -X, so the
  // arm's length axis ends up along -X; the arm's `top` plug meets the
  // socket and the rest of the cylinder extends outward.
  attach (parent="body", child="arm_l", socket="left",  plug="top")
  attach (parent="body", child="arm_r", socket="right", plug="top")

  // Legs hang down from the custom hip connectors.
  attach (parent="body", child="leg_l", socket="hip_l", plug="top")
  attach (parent="body", child="leg_r", socket="hip_r", plug="top")
}
