// A basic arming sword: blade (tapered via a 4-sided pyramid point + elongated
// box body), a rectangular crossguard, a wrapped grip, and a rounded pommel.

material "steel"   (color=[0.78, 0.80, 0.85], metallic=0.9, roughness=0.25)
material "leather" (color=[0.22, 0.12, 0.06], metallic=0.0, roughness=0.85)
material "brass"   (color=[0.78, 0.58, 0.18], metallic=0.8, roughness=0.35)

scene {
  group "sword" (role="weapon", tags="sword") {
    // Blade body: thin flat box along +Y.
    box "blade" (pos=[0, 0.6, 0], size=[0.05, 1.2, 0.012], mat="steel", role="blade") {
      connector "tip"  (at=[0,  0.6, 0], dir=[0,  1, 0], tag=blade_tip)
      connector "heel" (at=[0, -0.6, 0], dir=[0, -1, 0], tag=blade_heel)
    }

    // Blade tip: 4-sided pyramid sitting at the top of the blade, ~8 cm tall.
    pyramid "point" (pos=[0, 1.24, 0], radius=0.025, height=0.08, sides=4, mat="steel")

    // Crossguard: long flat box across X, sitting just below the blade heel.
    box "crossguard" (pos=[0, -0.02, 0], size=[0.28, 0.04, 0.04], mat="brass", role="crossguard")

    // Grip: short cylinder wrapped in leather, below the crossguard.
    cylinder "grip" (pos=[0, -0.15, 0], radius=0.018, height=0.22, mat="leather", role="grip")

    // Pommel: small sphere capping the grip.
    sphere "pommel" (pos=[0, -0.29, 0], radius=0.030, mat="brass", role="pommel")
  }
}
