const e=`// Wave deformer showcase — periodic ripples along a chosen axis.
//
// \`wave=\` displaces vertices sinusoidally along their normal. Combined with
// a dense surface (here a \`curved_plane\` with a high segment count) it
// produces water, jelly, ribbed metal, and similar surfaces.

meta (
  name = "wave_water",
  description = "Three rippling planes — a calm pond, a beach surf, and a ribbed panel — showing the wave deformer.",
  tags = ["wave", "deform", "water", "primitive"],
)

material "water" (
  color=[0.18, 0.40, 0.55], roughness=0.20, metallic=0.0,
  alpha=0.85, transmission=0.6,
)
material "jelly" (color=[0.25, 0.85, 0.55], roughness=0.30, alpha=0.85)
material "metal" (color=[0.55, 0.56, 0.60], roughness=0.45, metallic=0.95)

scene {
  group "field" (tags="floating") {
    // Calm pond — low amplitude, low frequency, large area.
    curved_plane "pond" (
      size=[3, 3], segments_u=64, segments_v=64,
      wave=0.06, wave_frequency=0.4, wave_axis="x",
      mat="water", pos=[-3.5, 0, 0]
    )

    // Surf strip — wave only at the front half via wave_range.
    curved_plane "surf" (
      size=[3, 3], segments_u=64, segments_v=64,
      wave=0.18, wave_frequency=0.7, wave_axis="z",
      wave_range=[0.55, 1.0],
      mat="jelly", pos=[0, 0, 0]
    )

    // Ribbed panel — high frequency, shallow amplitude, along Z.
    curved_plane "ribs" (
      size=[3, 1.5], segments_u=128, segments_v=16,
      wave=0.04, wave_frequency=4.0, wave_axis="z", wave_phase=1.5708,
      mat="metal", pos=[3.5, 0, 0]
    )
  }
}
`;export{e as default};
