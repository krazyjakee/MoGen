const n=`// Three-point lighting on a simple stage. All three glTF light kinds are
// punctual lights via KHR_lights_punctual: directional uses lux, point/spot
// use candela. Direction is the node's local -Z, derived from \`dir=\` here.

material "stone" (color=[0.55, 0.55, 0.6], roughness=0.8)
material "wood"  (color=[0.45, 0.28, 0.15], roughness=0.6)

scene {
  slab "floor" (size=[6, 0.2, 6], mat="stone")
  box "pedestal" (size=[1, 1, 1], mat="wood", above="floor")

  // Key: warm sun-coloured directional, raked from front-right.
  light "key" (
    kind=directional,
    color=[1.0, 0.95, 0.85],
    intensity=3,
    dir=[-0.4, -1.0, -0.3]
  )

  // Fill: cool point light off to the left to soften shadows.
  light "fill" (
    kind=point,
    pos=[-2, 2.5, 1.5],
    color=[0.7, 0.8, 1.0],
    intensity=15,
    range=8
  )

  // Rim: tight spot coming from behind, picking out the silhouette.
  light "rim" (
    kind=spot,
    pos=[0, 3, -2.5],
    dir=[0, -0.6, 1],
    color=[1, 1, 1],
    intensity=25,
    range=10,
    inner_cone=15,
    outer_cone=30
  )
}
`;export{n as default};
