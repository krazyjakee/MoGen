// mgen-generate seed=1776903446478943000
// prompt: fix the arm animations

material "skin"  (color=[0.90, 0.75, 0.62], roughness=0.7, base_color_texture="textures/skin_albedo.png", normal_texture="textures/skin_normal.png", metallic_roughness_texture="textures/skin_metallicRoughness.png", occlusion_texture="textures/skin_ao.png")
material "cloth" (color=[0.25, 0.35, 0.60], roughness=0.9, base_color_texture="textures/cloth_albedo.png", normal_texture="textures/cloth_normal.png", metallic_roughness_texture="textures/cloth_metallicRoughness.png", occlusion_texture="textures/cloth_ao.png")

scene {
  // Torso has custom hip connectors on its bottom face so the two legs
  // land on opposite sides instead of stacking on one point.
  rounded_box "body" (size=[0.6, 1.0, 0.3], radius=0.08, mat="cloth") {
    connector "hip_l" (at=[-0.15, -0.5, 0], dir=[0, -1, 0])
    connector "hip_r" (at=[ 0.15, -0.5, 0], dir=[0, -1, 0])
    connector "shoulder_l" (at=[-0.3, 0.4, 0], dir=[0, -1, 0])
    connector "shoulder_r" (at=[ 0.3, 0.4, 0], dir=[0, -1, 0])
  }

  cylinder "neck"  (radius=0.06, height=0.15, mat="skin")
  sphere   "head"  (radius=0.25, mat="skin")
  capsule  "arm_l" (radius=0.08, height=0.8, mat="skin")
  capsule  "arm_r" (radius=0.08, height=0.8, mat="skin")
  sphere   "hand_l" (radius=0.09, mat="skin")
  sphere   "hand_r" (radius=0.09, mat="skin")
  capsule  "leg_l" (radius=0.11, height=0.9, mat="cloth")
  capsule  "leg_r" (radius=0.11, height=0.9, mat="cloth")
  rounded_box "foot_l" (size=[0.14, 0.08, 0.25], radius=0.02, mat="cloth") {
    connector "ankle" (at=[0, 0.04, 0.04], dir=[0, 1, 0])
  }
  rounded_box "foot_r" (size=[0.14, 0.08, 0.25], radius=0.02, mat="cloth") {
    connector "ankle" (at=[0, 0.04, 0.04], dir=[0, 1, 0])
  }

  // Head tops the body (default socket=top, plug=bottom).
  attach (parent="body", child="neck")
  attach (parent="neck", child="head")

  // Arms hang down from the custom shoulder connectors.
  attach (parent="body", child="arm_l", socket="shoulder_l", plug="top")
  attach (parent="body", child="arm_r", socket="shoulder_r", plug="top")
  attach (parent="arm_l", child="hand_l", socket="bottom", plug="top")
  attach (parent="arm_r", child="hand_r", socket="bottom", plug="top")

  // Legs hang down from the custom hip connectors.
  attach (parent="body", child="leg_l", socket="hip_l", plug="top")
  attach (parent="body", child="leg_r", socket="hip_r", plug="top")
  attach (parent="leg_l", child="foot_l", socket="bottom", plug="ankle")
  attach (parent="leg_r", child="foot_r", socket="bottom", plug="ankle")

  joint "j_head" (type=hinge, pivot="head", axis=[0, 1, 0])
  joint "j_arm_l" (type=ball, pivot="arm_l")
  joint "j_arm_r" (type=ball, pivot="arm_r")
  joint "j_leg_l" (type=hinge, pivot="leg_l", axis=[1, 0, 0])
  joint "j_leg_r" (type=hinge, pivot="leg_r", axis=[1, 0, 0])
  joint "j_hand_l" (type=hinge, pivot="hand_l", axis=[1, 0, 0])
  joint "j_hand_r" (type=hinge, pivot="hand_r", axis=[1, 0, 0])
  joint "j_foot_l" (type=hinge, pivot="foot_l", axis=[1, 0, 0])
  joint "j_foot_r" (type=hinge, pivot="foot_r", axis=[1, 0, 0])
}

clip "walk" (seconds=1.0) {
  track "j_leg_l" (prop=rotation, axis=[1, 0, 0], keys=[[0, -25], [0.5,  25], [1.0, -25]])
  track "j_leg_r" (prop=rotation, axis=[1, 0, 0], keys=[[0,  25], [0.5, -25], [1.0,  25]])
  track "j_arm_l" (prop=rotation, axis=[1, 0, 0], keys=[[0,  20], [0.5, -20], [1.0,  20]])
  track "j_arm_r" (prop=rotation, axis=[1, 0, 0], keys=[[0, -20], [0.5,  20], [1.0, -20]])
}

clip "run" (seconds=0.6) {
  track "j_leg_l" (prop=rotation, axis=[1, 0, 0], keys=[[0, -45], [0.3,  45], [0.6, -45]])
  track "j_leg_r" (prop=rotation, axis=[1, 0, 0], keys=[[0,  45], [0.3, -45], [0.6,  45]])
  track "j_arm_l" (prop=rotation, axis=[1, 0, 0], keys=[[0,  40], [0.3, -40], [0.6,  40]])
  track "j_arm_r" (prop=rotation, axis=[1, 0, 0], keys=[[0, -40], [0.3,  40], [0.6, -40]])
}

clip "jump" (seconds=1.0) {
  track "body"  (prop=translation, axis=[0, 1, 0], keys=[[0, 0], [0.5, 0.5], [1.0, 0]])
  track "j_leg_l" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.2, -20], [0.5, 10], [0.8, -20], [1.0, 0]])
  track "j_leg_r" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.2, -20], [0.5, 10], [0.8, -20], [1.0, 0]])
  track "j_arm_l" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.5, 120], [1.0, 0]])
  track "j_arm_r" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [0.5, 120], [1.0, 0]])
}

clip "look_around" (seconds=2.0) {
  track "j_head" (prop=rotation, axis=[0, 1, 0], keys=[[0, 0], [0.5, 45], [1.5, -45], [2.0, 0]])
}

clip "wave" (seconds=1.0) {
  track "j_arm_r" (prop=rotation, axis=[0, 0, 1], keys=[[0, 0], [0.25, 120], [0.5, 90], [0.75, 120], [1.0, 0]])
}