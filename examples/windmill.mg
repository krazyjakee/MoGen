// Windmill: a tower with a rotating blade assembly driven by the `spin`
// procedural animation template. The `rotor` group is the animated node —
// its four blades ride along as children.

material "wood"  (color=[0.55, 0.35, 0.18], metallic=0.0, roughness=0.8)
material "white" (color=[0.92, 0.92, 0.92], metallic=0.0, roughness=0.4)

scene {
  // Static tower.
  cylinder "tower" (pos=[0, 1.5, 0], radius=0.25, height=3.0, mat="wood", role="tower")

  // The rotor pivots around +Z at the top of the tower; blades are children
  // so they follow the rotation.
  group "rotor" (pos=[0, 3.0, 0.3], role="rotor") {
    // Hub: a short cylinder oriented along +Z.
    cylinder "hub" (rot=[90, 0, 0], radius=0.1, height=0.15, mat="wood")

    // Four blades, 90° apart around the rotor's local Z axis.
    array "blades" (count=4, around=z) {
      box "blade" (pos=[0, 0.6, 0], size=[0.1, 1.2, 0.03], mat="white")
    }
  }
}

// Drive the rotor at 30 RPM around the +Z axis. The template builds a
// 2-second clip (60 / 30) with 5 keyframes so slerp traces a full revolution.
spin "windmill_spin" (target="rotor", axis=[0, 0, 1], rpm=30)
