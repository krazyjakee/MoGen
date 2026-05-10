const a=`scene {
  use "humanoid_full" (
    height=1.7,
    skin =[0.78, 0.68, 0.58],
    shirt=[0.20, 0.18, 0.45],
    pants=[0.15, 0.12, 0.30]
  )

  material "wizard_hat"     (color=[0.20, 0.15, 0.40], roughness=0.85)
  material "wizard_cape"    (color=[0.25, 0.20, 0.55], roughness=0.85)
  material "wizard_wood"    (color=[0.34, 0.22, 0.14], roughness=0.85)
  material "wizard_crystal" (color=[0.40, 0.70, 0.92], roughness=0.20,
                             emissive=[0.40, 0.70, 0.92], emissive_strength=0.8)

  // Brimmed hat — flat brim + chamfered crown, mounted to the head's crown slot.
  group "hat" {
    cylinder      "hat_brim" (pos=[0, 0.030, 0], radius=0.145, height=0.024, segments=16, mat="wizard_hat", faceted=1)
    chamfered_box "hat_dome" (pos=[0, 0.105, 0], size=[0.153, 0.102, 0.153], radius=0.020, mat="wizard_hat", faceted=1)
    connector "rim" (at=[0, 0, 0], dir=[0, -1, 0], tag=plug)
  }
  attach (parent="neck", child="hat", socket="slot_crown", plug="rim")

  // Cape — drapes from the upper back.
  chamfered_box "cape" (
    size=[0.510, 0.765, 0.026], radius=0.008,
    mat="wizard_cape", faceted=1
  ) {
    connector "neck_edge" (at=[0, 0.382, -0.012], dir=[0, 1, 0], tag=plug)
  }
  attach (parent="spine_chest", child="cape", socket="slot_chest_back", plug="neck_edge")

  // Staff — long shaft + glowing crystal finial, gripped in the right hand.
  group "staff" {
    cylinder      "staff_shaft"   (pos=[0, 0.238, 0], radius=0.024, height=1.190, segments=10, mat="wizard_wood", faceted=1)
    chamfered_box "staff_crystal" (pos=[0, 0.867, 0], size=[0.077, 0.102, 0.077], radius=0.020, mat="wizard_crystal", faceted=1)
    connector "grip" (at=[0, 0.085, 0], dir=[0, 1, 0], tag=plug)
  }
  attach (parent="wrist_r", child="staff", socket="slot_hand_r_grip", plug="grip")

  use "humanoid_idle" ()
}
`;export{a as default};
