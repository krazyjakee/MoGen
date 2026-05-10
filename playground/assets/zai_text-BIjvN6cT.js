const e=`meta (name = "three_leg_stool", description = "a small wooden stool with three splayed legs", tags = ["furniture", "stool", "wood"], seed = "1778142576205448400", thinking = "low", prompt = "a small wooden stool with three legs", mogen_version = "0.1.3")

material "wood" (color=[0.55, 0.35, 0.18], roughness=0.8)

scene {
  cylinder "seat" (radius=0.17, height=0.035, mat="wood") {
    connector "leg_0" (at=[0, -0.0175, -0.11], dir=[0, -1, -0.12])
    connector "leg_1" (at=[0.095, -0.0175, 0.055], dir=[0.0865, -1, 0.05])
    connector "leg_2" (at=[-0.095, -0.0175, 0.055], dir=[-0.0865, -1, 0.05])
  }
  cylinder "leg_0" (radius=0.022, height=0.38, mat="wood")
  cylinder "leg_1" (radius=0.022, height=0.38, mat="wood")
  cylinder "leg_2" (radius=0.022, height=0.38, mat="wood")
  attach (parent="seat", child="leg_0", socket="leg_0", plug="top")
  attach (parent="seat", child="leg_1", socket="leg_1", plug="top")
  attach (parent="seat", child="leg_2", socket="leg_2", plug="top")
}`;export{e as default};
