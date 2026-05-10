const n=`
meta (
  name = "chair",
  version = "1.0",
  mogen_version = "0.1.1",
  description = "A simple four-legged dining chair.",
  tags = ["furniture", "chair", "wood"], seed = "1777210527637284168", thinking = "high", prompt = "A simple four-legged chair."
)

material "wood" (color=[0.55, 0.35, 0.18], roughness=0.8)

scene {
  rounded_box "seat" (size=[0.45, 0.04, 0.45], radius=0.01, mat="wood") {
    connector "c_fl" (at=[-0.18, -0.02, -0.18], dir=[0, -1, 0])
    connector "c_fr" (at=[ 0.18, -0.02, -0.18], dir=[0, -1, 0])
    connector "c_bl" (at=[-0.18, -0.02,  0.18], dir=[0, -1, 0])
    connector "c_br" (at=[ 0.18, -0.02,  0.18], dir=[0, -1, 0])
    connector "c_back" (at=[0, 0.02, 0.205], dir=[0, 1, 0])
  }

  cylinder "leg_fl" (radius=0.02, height=0.45, mat="wood")
  cylinder "leg_fr" (radius=0.02, height=0.45, mat="wood")
  cylinder "leg_bl" (radius=0.02, height=0.45, mat="wood")
  cylinder "leg_br" (radius=0.02, height=0.45, mat="wood")
  
  rounded_box "backrest" (size=[0.45, 0.45, 0.04], radius=0.01, mat="wood")

  attach (parent="seat", child="leg_fl", socket="c_fl", plug="top")
  attach (parent="seat", child="leg_fr", socket="c_fr", plug="top")
  attach (parent="seat", child="leg_bl", socket="c_bl", plug="top")
  attach (parent="seat", child="leg_br", socket="c_br", plug="top")
  attach (parent="seat", child="backrest", socket="c_back", plug="bottom")
}`;export{n as default};
