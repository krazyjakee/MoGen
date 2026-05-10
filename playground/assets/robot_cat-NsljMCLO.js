const e=`meta (
  seed = "1777326724913034647",
  thinking = "high",
  prompt = "cute robot cat",
)

material "shell" (color=[0.95, 0.96, 0.98], roughness=0.2, metallic=0.1)
material "screen" (color=[0.05, 0.05, 0.08], roughness=0.1, metallic=0.8)
material "eye_glow" (color=[0.2, 0.8, 1.0], emissive=[0.2, 0.8, 1.0], emissive_strength=4.0)
material "accent" (color=[1.0, 0.4, 0.6], roughness=0.3, metallic=0.2)
material "joint" (color=[0.3, 0.3, 0.35], roughness=0.6, metallic=0.6)

scene {
  rounded_box "body" (pos=[0, 0.15, 0], size=[0.16, 0.14, 0.22], radius=0.04, mat="shell") {
    connector "neck" (at=[0, 0.02, -0.11], dir=[0, 0.2, -1])
    connector "tail_sock" (at=[0, 0.02, 0.11], dir=[0, 0.5, 1])
    connector "leg_fl_sock" (at=[-0.05, -0.07, -0.07], dir=[0, -1, 0])
    connector "leg_fr_sock" (at=[ 0.05, -0.07, -0.07], dir=[0, -1, 0])
    connector "leg_bl_sock" (at=[-0.05, -0.07,  0.07], dir=[0, -1, 0])
    connector "leg_br_sock" (at=[ 0.05, -0.07,  0.07], dir=[0, -1, 0])
  }

  rounded_box "head" (size=[0.24, 0.18, 0.18], radius=0.04, mat="shell", rot=[83.1726, 0, -0], pos=[0, 0.0363, 0]) {
    connector "face_sock" (at=[0, 0, -0.09], dir=[0, 0, -1])
    connector "ear_l_sock" (at=[-0.07, 0.09, 0], dir=[0, 1, 0])
    connector "ear_r_sock" (at=[ 0.07, 0.09, 0], dir=[0, 1, 0])
    connector "antenna_sock" (at=[0, 0.09, -0.05], dir=[0, 1, 0])
  }

  rounded_box "face" (size=[0.20, 0.12, 0.02], radius=0.01, mat="screen") {
    connector "eye_l_sock" (at=[-0.04, 0.01, -0.01], dir=[0, 0, -1])
    connector "eye_r_sock" (at=[ 0.04, 0.01, -0.01], dir=[0, 0, -1])
    connector "nose_sock" (at=[0, -0.02, -0.01], dir=[0, 0, -1])
  }

  rounded_box "eye_l" (size=[0.024, 0.04, 0.01], radius=0.004, mat="eye_glow")
  rounded_box "eye_r" (size=[0.024, 0.04, 0.01], radius=0.004, mat="eye_glow")
  sphere "nose" (radius=0.008, mat="accent")

  prism "ear_l" (size=[0.05, 0.05, 0.02], mat="accent")
  prism "ear_r" (size=[0.05, 0.05, 0.02], mat="accent")

  cylinder "antenna_base" (radius=0.004, height=0.04, mat="joint")
  sphere "antenna_tip" (radius=0.012, mat="eye_glow")

  group "leg_fl_hinge" {
    connector "top" (at=[0,0,0], dir=[0,1,0])
    capsule "leg_fl_upper" (pos=[0, -0.04, 0], radius=0.015, height=0.05, mat="shell") { connector "knee" (at=[0, -0.04, 0], dir=[0, -1, 0]) }
  }
  group "knee_fl_hinge" {
    connector "top" (at=[0,0,0], dir=[0,1,0])
    capsule "leg_fl_lower" (pos=[0, -0.04, 0], radius=0.012, height=0.05, mat="shell")
  }

  group "leg_fr_hinge" {
    connector "top" (at=[0,0,0], dir=[0,1,0])
    capsule "leg_fr_upper" (pos=[0, -0.04, 0], radius=0.015, height=0.05, mat="shell") { connector "knee" (at=[0, -0.04, 0], dir=[0, -1, 0]) }
  }
  group "knee_fr_hinge" {
    connector "top" (at=[0,0,0], dir=[0,1,0])
    capsule "leg_fr_lower" (pos=[0, -0.04, 0], radius=0.012, height=0.05, mat="shell")
  }

  group "leg_bl_hinge" {
    connector "top" (at=[0,0,0], dir=[0,1,0])
    capsule "leg_bl_upper" (pos=[0, -0.04, 0], radius=0.015, height=0.05, mat="shell") { connector "knee" (at=[0, -0.04, 0], dir=[0, -1, 0]) }
  }
  group "knee_bl_hinge" {
    connector "top" (at=[0,0,0], dir=[0,1,0])
    capsule "leg_bl_lower" (pos=[0, -0.04, 0], radius=0.012, height=0.05, mat="shell")
  }

  group "leg_br_hinge" {
    connector "top" (at=[0,0,0], dir=[0,1,0])
    capsule "leg_br_upper" (pos=[0, -0.04, 0], radius=0.015, height=0.05, mat="shell") { connector "knee" (at=[0, -0.04, 0], dir=[0, -1, 0]) }
  }
  group "knee_br_hinge" {
    connector "top" (at=[0,0,0], dir=[0,1,0])
    capsule "leg_br_lower" (pos=[0, -0.04, 0], radius=0.012, height=0.05, mat="shell")
  }

  spline_tube "tail" (points=[[0,0,0], [0,0.05,0.05], [0,0.1,0.12]], radius=0.01, mat="joint")
  sphere "tail_tip" (radius=0.02, mat="accent")

  attach (parent="head", child="face", socket="face_sock", plug="back")
  attach (parent="face", child="eye_l", socket="eye_l_sock", plug="back")
  attach (parent="face", child="eye_r", socket="eye_r_sock", plug="back")
  attach (parent="face", child="nose", socket="nose_sock", plug="back")
  attach (parent="head", child="ear_l", socket="ear_l_sock", plug="bottom")
  attach (parent="head", child="ear_r", socket="ear_r_sock", plug="bottom")
  attach (parent="head", child="antenna_base", socket="antenna_sock", plug="bottom")
  attach (parent="antenna_base", child="antenna_tip", socket="top", plug="bottom")

  attach (parent="body", child="head", socket="neck", plug="bottom")
  
  attach (parent="body", child="leg_fl_hinge", socket="leg_fl_sock", plug="top")
  attach (parent="leg_fl_upper", child="knee_fl_hinge", socket="knee", plug="top")
  
  attach (parent="body", child="leg_fr_hinge", socket="leg_fr_sock", plug="top")
  attach (parent="leg_fr_upper", child="knee_fr_hinge", socket="knee", plug="top")
  
  attach (parent="body", child="leg_bl_hinge", socket="leg_bl_sock", plug="top")
  attach (parent="leg_bl_upper", child="knee_bl_hinge", socket="knee", plug="top")
  
  attach (parent="body", child="leg_br_hinge", socket="leg_br_sock", plug="top")
  attach (parent="leg_br_upper", child="knee_br_hinge", socket="knee", plug="top")
  
  attach (parent="body", child="tail", socket="tail_sock", plug="start")
  attach (parent="tail", child="tail_tip", socket="end", plug="bottom")
}

wave "tail_wag" (target="tail", axis=[0, 1, 0], amplitude=25, hz=0.5)
wave "ear_twitch_l" (target="ear_l", axis=[0, 0, 1], amplitude=10, hz=0.8)
wave "ear_twitch_r" (target="ear_r", axis=[0, 0, 1], amplitude=10, hz=0.85)

joint "hip_fl" (type=hinge, axis=[1, 0, 0], pivot="leg_fl_hinge")
joint "knee_fl" (type=hinge, axis=[1, 0, 0], pivot="knee_fl_hinge")
joint "hip_fr" (type=hinge, axis=[1, 0, 0], pivot="leg_fr_hinge")
joint "knee_fr" (type=hinge, axis=[1, 0, 0], pivot="knee_fr_hinge")
joint "hip_bl" (type=hinge, axis=[1, 0, 0], pivot="leg_bl_hinge")
joint "knee_bl" (type=hinge, axis=[1, 0, 0], pivot="knee_bl_hinge")
joint "hip_br" (type=hinge, axis=[1, 0, 0], pivot="leg_br_hinge")
joint "knee_br" (type=hinge, axis=[1, 0, 0], pivot="knee_br_hinge")

clip "walk" (seconds=1.0) {
  track "hip_fl" (prop=rotation, axis=[1, 0, 0], keys=[[0, 20], [0.5, -20], [1.0, 20]])
  track "knee_fl" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.25, 20], [0.5, 0], [1.0, 0]])
  
  track "hip_br" (prop=rotation, axis=[1, 0, 0], keys=[[0, 20], [0.5, -20], [1.0, 20]])
  track "knee_br" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.25, -20], [0.5, 0], [1.0, 0]])
  
  track "hip_fr" (prop=rotation, axis=[1, 0, 0], keys=[[0, -20], [0.5, 20], [1.0, -20]])
  track "knee_fr" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.5, 0], [0.75, 20], [1.0, 0]])
  
  track "hip_bl" (prop=rotation, axis=[1, 0, 0], keys=[[0, -20], [0.5, 20], [1.0, -20]])
  track "knee_bl" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.5, 0], [0.75, -20], [1.0, 0]])
}

clip "blink" (seconds=3.0) {
  track "eye_l" (prop=scale, keys=[[0, 1], [2.8, 1], [2.9, 0.1], [3.0, 1]])
  track "eye_r" (prop=scale, keys=[[0, 1], [2.8, 1], [2.9, 0.1], [3.0, 1]])
}`;export{e as default};
