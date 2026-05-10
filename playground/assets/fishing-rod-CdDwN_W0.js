const t=`meta (seed = "1777855689736026800", thinking = "high", prompt = "a fishing rod", mogen_version = "0.1.1")

material "cork" (color=[0.75, 0.6, 0.4], roughness=0.9, base_color_texture="textures/fishing-rod/cork_albedo.png", normal_texture="textures/fishing-rod/cork_normal.png", metallic_roughness_texture="textures/fishing-rod/cork_metallicRoughness.png", occlusion_texture="textures/fishing-rod/cork_ao.png")
material "carbon" (color=[0.1, 0.1, 0.12], roughness=0.4, base_color_texture="textures/fishing-rod/carbon_albedo.png", normal_texture="textures/fishing-rod/carbon_normal.png", metallic_roughness_texture="textures/fishing-rod/carbon_metallicRoughness.png", occlusion_texture="textures/fishing-rod/carbon_ao.png")
material "metal" (color=[0.8, 0.8, 0.85], metallic=0.9, roughness=0.2, base_color_texture="textures/fishing-rod/metal_albedo.png", normal_texture="textures/fishing-rod/metal_normal.png", metallic_roughness_texture="textures/fishing-rod/metal_metallicRoughness.png", occlusion_texture="textures/fishing-rod/metal_ao.png")
material "plastic" (color=[0.15, 0.15, 0.15], roughness=0.6, base_color_texture="textures/fishing-rod/plastic_albedo.png", normal_texture="textures/fishing-rod/plastic_normal.png", metallic_roughness_texture="textures/fishing-rod/plastic_metallicRoughness.png", occlusion_texture="textures/fishing-rod/plastic_ao.png")

scene {
  group "fishing_rod" {
    cylinder "handle" (radius=0.015, height=0.3, mat="cork")
    hemisphere "butt_cap" (radius=0.015, mat="plastic")
    cylinder "reel_seat" (radius=0.012, height=0.15, mat="plastic")
    cylinder "foregrip" (radius=0.014, height=0.1, mat="cork")
    
    spline_tube "blank" (
      points=[[0,0,0], [0,0.5,0], [0,1.0,0], [0,1.5,0], [0,1.8,0]], 
      radii=[0.008, 0.006, 0.004, 0.002, 0.0015], 
      mat="carbon"
    ) {
      group "guide_1" (pos=[0, 0.2, 0]) {
        torus "ring1" (major=0.008, minor=0.001, pos=[0.012, 0, 0], mat="metal")
        box "foot1" (size=[0.012, 0.002, 0.002], pos=[0.006, 0, 0], mat="metal")
      }
      group "guide_2" (pos=[0, 0.6, 0]) {
        torus "ring2" (major=0.006, minor=0.001, pos=[0.010, 0, 0], mat="metal")
        box "foot2" (size=[0.010, 0.002, 0.002], pos=[0.005, 0, 0], mat="metal")
      }
      group "guide_3" (pos=[0, 1.0, 0]) {
        torus "ring3" (major=0.005, minor=0.001, pos=[0.008, 0, 0], mat="metal")
        box "foot3" (size=[0.008, 0.002, 0.002], pos=[0.004, 0, 0], mat="metal")
      }
      group "guide_4" (pos=[0, 1.4, 0]) {
        torus "ring4" (major=0.004, minor=0.0008, pos=[0.006, 0, 0], mat="metal")
        box "foot4" (size=[0.006, 0.0015, 0.0015], pos=[0.003, 0, 0], mat="metal")
      }
      group "guide_5" (pos=[0, 1.7, 0]) {
        torus "ring5" (major=0.003, minor=0.0008, pos=[0.004, 0, 0], mat="metal")
        box "foot5" (size=[0.004, 0.0015, 0.0015], pos=[0.002, 0, 0], mat="metal")
      }
      group "guide_tip" (pos=[0, 1.8, 0]) {
        torus "ring_tip" (major=0.003, minor=0.0008, pos=[0.002, 0, 0], mat="metal")
        box "foot_tip" (size=[0.002, 0.0015, 0.0015], pos=[0.001, 0, 0], mat="metal")
      }
    }

    box "stem" (size=[0.03, 0.01, 0.015], mat="metal")
    rounded_box "body" (size=[0.04, 0.07, 0.04], radius=0.01, mat="plastic")
    cylinder "spool" (radius=0.022, height=0.035, mat="metal")
    box "crank_arm" (size=[0.008, 0.008, 0.04], mat="metal")
    capsule "crank_knob" (radius=0.008, height=0.025, mat="plastic")

    attach (parent="handle", child="butt_cap", socket="bottom", plug="base")
    attach (parent="handle", child="reel_seat", socket="top", plug="bottom")
    attach (parent="reel_seat", child="foregrip", socket="top", plug="bottom")
    attach (parent="foregrip", child="blank", socket="top", plug="start")
    
    attach (parent="reel_seat", child="stem", socket="side", plug="left")
    attach (parent="stem", child="body", socket="right", plug="left")
    attach (parent="body", child="spool", socket="top", plug="bottom")
    attach (parent="body", child="crank_arm", socket="front", plug="back")
    attach (parent="crank_arm", child="crank_knob", socket="front", plug="bottom")
  }
}

wave "rod_swing" (target="fishing_rod", axis=[1, 0, 0], amplitude=10, hz=0.5)

clip "cast_windup" (seconds=2.0) {
  track "fishing_rod" (prop=rotation, axis=[1, 0, 0], keys=[[0, 0], [1.0, -80], [2.0, 0]])
}`;export{t as default};
