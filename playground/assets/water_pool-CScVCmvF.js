const e=`// Demonstrate the per-material \`shader = "water"\` preview override.
//
// In MoGen Studio the pool surface ripples and reflects the sky; in the
// exported \`.glb\` it falls back to standard PBR scalars, since glTF 2.0
// has no way to carry custom shader code.
//
// The water branch now reads the standard material settings:
//
//   color            absorbed body tint (looking straight down). Try
//                    [0.02, 0.05, 0.15] for deep ocean,
//                    [0.55, 0.85, 0.75] for a tropical lagoon.
//   uv_scale         ripple density. 1.0 ≈ pool-scale chop, 2.0 = small
//                    choppy pond, 0.4 = lazy ocean swells.
//   roughness        chop + reflection blur + sun-glint sharpness + foam.
//                    0.05 = glassy mirror, 0.4 = calm pool, 0.9 = ocean,
//                    1.0 = stormy with whitecaps.
//   metallic         0 = dielectric water (default). Push toward 1 for
//                    mercury / liquid metal — the body tint becomes the
//                    reflection colour at all angles.
//   transmission     0 = opaque body absorption (default). 1 = fully
//                    clear; the body colour recedes and the sky / what's
//                    behind the surface dominates. Combine with
//                    \`alpha_mode="blend"\` to actually see the pool floor.
//   alpha_mode       "blend" makes water translucent (with a Fresnel rim
//                    so the silhouette stays visible at grazing angles).
//   emissive         glowing water — lava, magic potion, bioluminescence.
//   emissive_strength HDR multiplier on the glow.
//   normal_strength  multiplier on the wave-slope (default 1.5). Bigger
//                    = more pronounced ripples without affecting chop.
//   normal_texture   blended into the procedural waves for extra
//                    high-frequency detail (caustic-like ripples, etc.).
//   base_color_texture  per-pixel body-tint variation (shallow/deep map).

material "pool_water" (
  color=[0.12, 0.55, 0.62],
  shader="water",
  uv_scale=1.0,
  roughness=0.4,
  transmission=0.6,
  alpha_mode="blend"
)
material "tile" (color=[0.78, 0.84, 0.86], roughness=0.6)

scene {
  group "pool" (pos=[0, 0, 0]) {
    box "floor"  (pos=[0,  0.0, 0], size=[4.0, 0.1, 4.0], mat="tile")
    box "wall_n" (pos=[0,  0.4, -1.95], size=[4.0, 0.7, 0.1], mat="tile")
    box "wall_s" (pos=[0,  0.4,  1.95], size=[4.0, 0.7, 0.1], mat="tile")
    box "wall_e" (pos=[ 1.95, 0.4, 0], size=[0.1, 0.7, 4.0], mat="tile")
    box "wall_w" (pos=[-1.95, 0.4, 0], size=[0.1, 0.7, 4.0], mat="tile")
    plane "surface" (pos=[0, 0.55, 0], size=[3.7, 0, 3.7], mat="pool_water", tags="floating")
  }
}
`;export{e as default};
