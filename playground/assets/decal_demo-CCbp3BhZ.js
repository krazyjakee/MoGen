const e=`// Decal demo: a logo on a shirt panel + a handwritten note on a paper card.
// Decals lower to thin double-sided quads with auto-synthesized
// alpha_mode="blend" materials. Run \`mogen textures examples/decal_demo.mog\`
// to fill in the transparent images via Gemini and splice the resolved
// paths back into this file.

material "shirt" (color=[0.10, 0.18, 0.55], roughness=0.85)
material "paper" (color=[0.96, 0.94, 0.88], roughness=0.95)

scene {
  // Front of a "shirt" — the decal lives as a child of the panel so it
  // moves with it. Its local +Z = the panel's front face.
  box "shirt_panel" (size=[0.6, 0.8, 0.2], mat="shirt") {
    decal "shirt_logo" (
      pos    = [0, 0.10, 0.101],
      size   = [0.25, 0.12],
      prompt = "embroidered MoGen logo: a small mountain glyph stitched in
                cream-white thread on a navy fabric"
    )
  }

  // A paper card with a handwritten note. The decal hugs the card's front
  // face via the panel's +Z anchor (panels default to anchor=back, so
  // local +Z is at the front).
  panel "card" (size=[0.4, 0.3, 0.012], mat="paper", right_of="shirt_panel", gap=0.25) {
    decal "thank_you_note" (
      pos    = [0, 0.05, 0.012],
      size   = [0.30, 0.20],
      prompt = "handwritten cursive note saying 'thanks!' in dark blue ink"
    )
  }
}
`;export{e as default};
