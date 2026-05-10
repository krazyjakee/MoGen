const e=`meta (
  description = "Industrial control panel — chamfered enclosure with three recessed switch wells, demonstrating chamfered_box and inset_box primitives.",
  tags = ["panel", "industrial", "control", "switches", "ui"],
)

material "panel_steel"   (color=[0.32, 0.34, 0.38], metallic=1.0, roughness=0.42)
material "switch_well"   (color=[0.10, 0.10, 0.12], metallic=0.4, roughness=0.65)
material "switch_red"    (color=[0.78, 0.10, 0.08], metallic=0.2, roughness=0.30)
material "switch_amber"  (color=[0.88, 0.55, 0.10], metallic=0.2, roughness=0.30)
material "switch_green"  (color=[0.20, 0.65, 0.25], metallic=0.2, roughness=0.30)

scene {
  // Outer enclosure: a chamfered_box reads as machined / industrial in a
  // way that a sharp \`box\` or fully-rounded \`rounded_box\` doesn't. The
  // 45° bevels on every edge catch light and match real CNC-milled panels.
  chamfered_box "case" (
    size=[0.80, 0.10, 0.50],
    radius=0.012,
    mat="panel_steel"
  )

  // Three switch wells across the top face. Each one is an inset_box
  // sitting on top of the case — its +Y face sinks 4 mm to recess the
  // switch label/cap, which would float at the well's bottom plane.
  group "switch_left" (above="case", x=-0.24, y=-0.08) {
    inset_box "well" (
      size=[0.18, 0.04, 0.18],
      face="+y",
      amount=0.025,
      depth=0.012,
      mat="switch_well"
    )
    cylinder "cap" (
      radius=0.045,
      height=0.018,
      pos=[0, 0.02, 0],
      mat="switch_red"
    )
  }

  group "switch_mid" (above="case", y=-0.08) {
    inset_box "well" (
      size=[0.18, 0.04, 0.18],
      face="+y",
      amount=0.025,
      depth=0.012,
      mat="switch_well"
    )
    cylinder "cap" (
      radius=0.045,
      height=0.018,
      pos=[0, 0.02, 0],
      mat="switch_amber"
    )
  }

  group "switch_right" (above="case", x=0.24, y=-0.08) {
    inset_box "well" (
      size=[0.18, 0.04, 0.18],
      face="+y",
      amount=0.025,
      depth=0.012,
      mat="switch_well"
    )
    cylinder "cap" (
      radius=0.045,
      height=0.018,
      pos=[0, 0.02, 0],
      mat="switch_green"
    )
  }
}
`;export{e as default};
