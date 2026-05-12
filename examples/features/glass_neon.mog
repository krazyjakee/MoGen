// Exercise the translucent and fluorescent material paths.
//
// - `glass` uses KHR_materials_transmission for true see-through glass
//   without blowing out the PBR highlights.
// - `tinted_gel` uses alpha+blend for a coloured film (like a stage gel).
// - `neon` uses an HDR emissive via KHR_materials_emissive_strength; values
//   above 1.0 drive bloom in renderers that honour the extension.
// - `foliage` uses alpha_mode="mask" for 1-bit cutouts (leaves/hair).
material "glass"      (color=[0.95, 0.97, 1.0], alpha=0.15, roughness=0.02, transmission=0.95)
material "tinted_gel" (color=[0.2, 0.6, 1.0],  alpha=0.35, roughness=0.4)
material "neon"       (color=[1.0, 0.15, 0.85], emissive=[1.0, 0.15, 0.85], emissive_strength=8.0, roughness=0.4)
material "foliage"    (color=[0.25, 0.6, 0.2], alpha_mode="mask", alpha_cutoff=0.5, roughness=0.9)
material "base"       (color=[0.2, 0.2, 0.22], roughness=0.6)

scene {
  group "display" (pos=[0, 0, 0]) {
    box "plinth"    (pos=[0, 0.1, 0], size=[3.0, 0.2, 1.5], mat="base")
    sphere "orb"    (pos=[-1.0, 0.5, 0], radius=0.3, mat="glass")
    box "gel_pane"  (pos=[0, 0.6, 0], size=[0.6, 0.8, 0.02], mat="tinted_gel")
    box "neon_bar"  (pos=[1.0, 0.5, 0], size=[0.1, 0.6, 0.1], mat="neon")
    plane "leaf"    (pos=[0, 0.2, 0.5], size=[0.4, 0.0, 0.4], mat="foliage")
  }
}
