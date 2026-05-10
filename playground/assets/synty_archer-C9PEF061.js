const a=`meta (name = "archer_character", description = "a low-poly archer character with hat, backpack, and belt", tags = ["character", "humanoid", "archer", "low-poly"], seed = "1778406416603349710", thinking = "high", prompt = "repair validation errors", mogen_version = "0.1.4", style = "low_poly")

material "archer_hat"    (color=[0.30, 0.45, 0.25], roughness=0.85)
material "archer_pack"   (color=[0.50, 0.32, 0.20], roughness=0.80)
material "archer_belt"   (color=[0.18, 0.12, 0.08], roughness=0.75)
material "archer_buckle" (color=[0.80, 0.65, 0.20], metallic=0.6, roughness=0.40)

scene {
  use "humanoid_full" (
    height=1.7,
    skin =[0.82, 0.62, 0.50],
    shirt=[0.30, 0.45, 0.25],
    pants=[0.22, 0.28, 0.18],
    boot =[0.12, 0.10, 0.08],
    hair =[0.30, 0.20, 0.10]
  )

  // Brimmed traveller's hat — attaches to the head crown.
  group "hat" {
    cylinder      "hat_brim" (pos=[0, 0.030, 0], radius=0.145, height=0.024, segments=16, mat="archer_hat", faceted=1)
    chamfered_box "hat_dome" (pos=[0, 0.082, 0], size=[0.153, 0.102, 0.153], radius=0.020, mat="archer_hat", faceted=1)
    connector "rim" (at=[0, 0.018, 0], dir=[0, -1, 0], tag=plug)
  }
  attach (parent="neck", child="hat", socket="slot_crown", plug="rim")

  // Backpack — boxy main body + over-shoulder straps. Mounts on the lower back.
  group "pack" {
    chamfered_box "pack_body"   (pos=[0, 0.0,  0.077], size=[0.306, 0.425, 0.153], radius=0.020, mat="archer_pack", faceted=1)
    chamfered_box "pack_strap_l" (pos=[ 0.090, 0.155, -0.010], rot=[20, 0,  8], size=[0.037, 0.306, 0.020], radius=0.005, mat="archer_pack", faceted=1)
    chamfered_box "pack_strap_r" (pos=[-0.090, 0.155, -0.010], rot=[20, 0, -8], size=[0.037, 0.306, 0.020], radius=0.005, mat="archer_pack", faceted=1)
    connector "front" (at=[0, 0, 0], dir=[0, 0, -1], tag=plug)
  }
  attach (parent="spine_chest", child="pack", socket="slot_back_lower", plug="front")

  // Belt with buckle — wraps the waist.
  group "belt" {
    chamfered_box "belt_band"   (size=[0.364, 0.043, 0.211], radius=0.007, mat="archer_belt",   faceted=1)
    chamfered_box "belt_buckle" (pos=[0, 0, -0.108], size=[0.065, 0.051, 0.020], radius=0.003, mat="archer_buckle", faceted=1)
    connector "top" (at=[0, 0.022, 0], dir=[0, 1, 0], tag=plug)
  }
  attach (parent="hip", child="belt", socket="slot_waist_front", plug="top", offset=-0.040)

  use "humanoid_run" ()
}`;export{a as default};
