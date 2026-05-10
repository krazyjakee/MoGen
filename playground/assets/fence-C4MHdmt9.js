const e=`meta (
  description = "Wooden picket fence — demonstrates Phase C3 control flow: a for loop emits N posts at regular spacing, with an if/else branch toggling between pointed pickets and flat-top pickets per post.",
  tags = ["fence", "wood", "outdoor", "demo", "control-flow"],
)

material "post"   (color=[0.45, 0.30, 0.18], roughness=0.85)
material "rail"   (color=[0.50, 0.32, 0.20], roughness=0.85)
material "picket" (color=[0.62, 0.42, 0.28], roughness=0.80)

// One picket: a vertical board capped by either a triangular point or a flat
// top, chosen by the caller. The \`if (cond=$pointed)\` branch shows the
// either/or use of \`if\`/\`else\` inside a module body.
module "picket" (pointed=1) {
  box "shaft" (size=[0.04, 0.55, 0.014], y=0.275, mat="picket")
  if (cond=$pointed) {
    pyramid "tip" (radius=0.03, height=0.06, sides=4, y=0.58, rot=[0, 45, 0], mat="picket")
  }
  else {
    box "cap" (size=[0.05, 0.012, 0.022], y=0.556, mat="picket")
  }
}

scene {
  group "fence" {
    // Five evenly-spaced posts. \`for\` emits the body once per integer step
    // in [0, 5); the loop variable \`$i\` drives both the spacing and the
    // unique node name via "post_$i" interpolation.
    for (var="i", from=0, to=5) {
      box "post_$i" (size=[0.06, 0.85, 0.06],
                     pos=[$i * 0.30, 0.425, 0],
                     mat="post")
    }

    // Top and bottom horizontal rails, sized to span all 5 posts.
    box "rail_top" (size=[1.20, 0.04, 0.025], pos=[0.60, 0.70, 0], mat="rail")
    box "rail_bot" (size=[1.20, 0.04, 0.025], pos=[0.60, 0.20, 0], mat="rail")

    // Pickets between the posts. Five posts → four bays of three pickets
    // each. Outer bays (i==0) get flat-top pickets so the runs end with
    // a different shape than the pointed inner bays — \`cond=$bay > 0\`
    // demonstrates a comparison flowing into the picket module.
    for (var="bay", from=0, to=4) {
      for (var="p", from=0, to=3) {
        use "picket" (
          x = $bay * 0.30 + $p * 0.075 + 0.075,
          y = 0,
          z = 0.005,
          pointed = $bay > 0
        )
      }
    }
  }
}
`;export{e as default};
