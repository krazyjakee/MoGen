const e=`// Heightfield primitive — terrain patches with controllable noise.
//
// \`heightfield\` builds a tessellated XZ grid and displaces each grid vertex
// along +Y by deterministic fractional-Brownian-motion value-noise. Pair
// with the \`wave\` deformer for moving water; pair with \`noise=\` deformers
// on a \`box\` underneath for layered terrain.

meta (
  name = "heightfield_terrain",
  description = "Terrain showcase — three heightfields with different noise tunings.",
  tags = ["heightfield", "terrain", "primitive"],
)

material "earth" (color=[0.42, 0.32, 0.18], roughness=0.92)
material "snow"  (color=[0.92, 0.94, 0.96], roughness=0.45)
material "lava"  (color=[0.85, 0.18, 0.05], emissive=[0.85, 0.18, 0.05], emissive_strength=2.0, roughness=0.7)

scene {
  group "field" (tags="floating") {
    // Rolling hills — broad, smooth, 3 octaves.
    heightfield "hills" (
      size=[6, 6], segments_u=64, segments_v=64,
      amplitude=0.7, octaves=3, frequency=0.4, persistence=0.5, seed=1,
      mat="earth", pos=[-7, 0, 0]
    )

    // Peaky alpine — higher amplitude, more octaves for craggy detail.
    heightfield "peaks" (
      size=[6, 6], segments_u=96, segments_v=96,
      amplitude=1.4, octaves=5, frequency=0.5, persistence=0.55, seed=42,
      mat="snow", pos=[0, 0, 0]
    )

    // Lava field — higher base frequency, jagged, glowing.
    heightfield "lava" (
      size=[6, 6], segments_u=96, segments_v=96,
      amplitude=0.5, octaves=6, frequency=1.2, persistence=0.6, seed=11,
      mat="lava", pos=[7, 0, 0]
    )
  }
}
`;export{e as default};
