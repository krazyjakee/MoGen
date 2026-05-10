const e=`meta (
  name = "knight_in_armor",
  description = "a low-poly knight in steel helmet with cape and sword, attached via humanoid slot connectors",
  tags = ["character", "knight", "armor"], seed = "1778402020222785000", thinking = "high", prompt = "repair validation errors", mogen_version = "0.1.4")

scene {
  use "humanoid_full" (
    height=1.7,
    skin =[0.85, 0.65, 0.55],
    shirt=[0.50, 0.52, 0.55],
    pants=[0.30, 0.30, 0.32],
    boot =[0.10, 0.10, 0.10]
  )

  material "knight_steel"   (color=[0.65, 0.66, 0.70], metallic=0.7, roughness=0.45)
  material "knight_visor"   (color=[0.32, 0.34, 0.36], metallic=0.7, roughness=0.40)
  material "knight_cape"    (color=[0.62, 0.18, 0.18], roughness=0.85)
  material "knight_belt"    (color=[0.18, 0.12, 0.08], roughness=0.75)
  material "knight_buckle"  (color=[0.80, 0.65, 0.20], metallic=0.6, roughness=0.40)
  material "knight_leather" (color=[0.32, 0.20, 0.10], roughness=0.80)
  material "knight_wood"    (color=[0.45, 0.28, 0.16], roughness=0.75)

  group "helmet" {
    chamfered_box "helmet_cap" (
      pos=[0, 0.075, 0],
      size=[0.20, 0.150, 0.20], radius=0.025,
      mat="knight_steel", faceted=1
    )
    chamfered_box "helmet_band" (
      pos=[0, 0.040, -0.092],
      size=[0.187, 0.042, 0.024], radius=0.007,
      mat="knight_visor", faceted=1
    )
    connector "rim" (at=[0, 0, 0], dir=[0, -1, 0], tag=plug)
  }
  attach (parent="neck", child="helmet", socket="slot_crown", plug="rim")

  chamfered_box "cape" (
    size=[0.510, 0.765, 0.026], radius=0.008,
    mat="knight_cape", faceted=1
  ) {
    connector "neck_edge" (at=[0, 0.382, -0.012], dir=[0, 1, 0], tag=plug)
  }
  attach (parent="spine_chest", child="cape", socket="slot_chest_back", plug="neck_edge")

  group "belt" {
    chamfered_box "belt_band"   (size=[0.364, 0.043, 0.211], radius=0.007, mat="knight_belt",   faceted=1)
    chamfered_box "belt_buckle" (pos=[0, 0, -0.108], size=[0.065, 0.051, 0.020], radius=0.003, mat="knight_buckle", faceted=1)
    connector "top" (at=[0, 0.022, 0], dir=[0, 1, 0], tag=plug)
  }
  attach (parent="hip", child="belt", socket="slot_waist_front", plug="top", offset=-0.040)

  group "sword" (tags="floating") {
    sphere        "sword_pommel" (pos=[0,  0.051, 0], radius=0.024, rings=4, segments=8, mat="knight_leather", faceted=1)
    cylinder      "sword_hilt"   (pos=[0,  0.000, 0], radius=0.019, height=0.102, segments=10, mat="knight_leather", faceted=1)
    chamfered_box "sword_guard"  (pos=[0, -0.061, 0], size=[0.102, 0.020, 0.030], radius=0.003, mat="knight_steel", faceted=1)
    chamfered_box "sword_blade"  (pos=[0, -0.360, 0], size=[0.037, 0.578, 0.010], radius=0.003, mat="knight_steel", faceted=1)
    connector "grip" (at=[0, 0, 0], dir=[0, 1, 0], tag=plug)
  }
  attach (parent="wrist_r", child="sword", socket="slot_hand_r_grip", plug="grip")

  group "shield" {
    chamfered_box "shield_body" (pos=[0, 0, 0], size=[0.323, 0.391, 0.034], radius=0.030, mat="knight_wood",  faceted=1)
    cylinder      "shield_boss" (pos=[0, 0, -0.017], rot=[90, 0, 0], radius=0.037, height=0.020, segments=12, mat="knight_steel", faceted=1)
    connector "grip" (at=[0, 0, 0], dir=[0, 1, 0], tag=plug)
  }
  attach (parent="wrist_l", child="shield", socket="slot_hand_l_grip", plug="grip")

  use "humanoid_walk" ()
}`;export{e as default};
