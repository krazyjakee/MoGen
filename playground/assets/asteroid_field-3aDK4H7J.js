const e=`meta (
  name = "asteroid_field",
  description = "Three rocks, a melted candle, and a bent metal rod showing the deformation modifiers in action.",
  tags = ["rock", "asteroid", "deform"], mogen_version = "0.1.2"
)

material "stone"  (color=[0.45, 0.42, 0.38], roughness=0.95)
material "wax"    (color=[0.92, 0.85, 0.65], roughness=0.55)
material "iron"   (color=[0.35, 0.30, 0.27], roughness=0.80, metallic=0.80)

scene {
  // A scattered field — every cluster is an isolated showcase, so the whole
  // group is \`tags="floating"\` to opt out of the connectivity validator.
  group "field" (tags="floating") {
    // Three rocks of varying size and seed. The \`rock\` look is \`noise\` (smooth
    // surface displacement) + \`jitter\` (per-vertex roughness) + \`faceted=1\`
    // (low-poly look). Distinct seeds make the silhouettes differ.
    icosphere "rock_a" (
      radius=0.40, noise=0.30, jitter=0.15, faceted=1, seed=1,
      mat="stone", pos=[-1.0, 0, 0]
    )
    icosphere "rock_b" (
      radius=0.55, noise=0.1, jitter=0.0, faceted=1, seed=2,
      mat="stone", pos=[ 0.0, 0, 0]
    )
    icosphere "rock_c" (
      radius=0.32, noise=0.30, jitter=0.15, faceted=1, seed=3,
      mat="stone", pos=[ 0.9, 0, 0]
    )

    // A melted wax pillar — \`droop\` sags the top under gravity, light \`noise\`
    // softens the surface so it doesn't read as a clean cylinder.
    cylinder "candle" (
      radius=0.18, height=0.7, droop=0.40, noise=0.05, seed=11,
      mat="wax", pos=[2.0, 0.35, 0]
    )

    // A weathered metal rod with a noticeable bend in the Z direction and a
    // pitted, faceted surface.
    cylinder "rod" (
      radius=0.04, height=1.2, bend_z=15, noise=0.08, faceted=1, seed=5,
      mat="iron", pos=[-2.2, 0.6, 0]
    )
  }
}
`;export{e as default};
