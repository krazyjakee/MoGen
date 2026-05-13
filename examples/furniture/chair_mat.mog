// Chair with PBR materials and semantic roles.
material "wood"   (color=[0.55, 0.35, 0.18], metallic=0.0, roughness=0.75)
material "fabric" (color=[0.20, 0.30, 0.55], metallic=0.0, roughness=0.95)

scene {
  group "chair" (pos=[0, 0, 0], role="furniture", tags="chair,seat") {
    box "seat"   (pos=[0, 0.5, 0],     size=[1.0, 0.1, 1.0], mat="fabric", role="seat")
    box "back"   (pos=[0, 1.0, -0.45], size=[1.0, 1.0, 0.1], mat="wood",   role="back")
    cylinder "leg_fl" (pos=[-0.45, 0.25, -0.45], radius=0.05, height=0.5, mat="wood", role="leg")
    cylinder "leg_fr" (pos=[ 0.45, 0.25, -0.45], radius=0.05, height=0.5, mat="wood", role="leg")
    cylinder "leg_bl" (pos=[-0.45, 0.25,  0.45], radius=0.05, height=0.5, mat="wood", role="leg")
    cylinder "leg_br" (pos=[ 0.45, 0.25,  0.45], radius=0.05, height=0.5, mat="wood", role="leg")
  }
}
