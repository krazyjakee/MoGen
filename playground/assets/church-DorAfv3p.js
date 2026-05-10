const o=`
meta (
  seed = "1776951857766439313",
  prompt = "a gothic church with high detail", mogen_version = "0.1.1"
)

material "stone" (color=[0.55, 0.53, 0.50], roughness=0.9)
material "roof"  (color=[0.15, 0.20, 0.18], roughness=0.8)
material "glass" (color=[0.1, 0.3, 0.6], emissive=[0.2, 0.4, 0.8], emissive_strength=1.5, roughness=0.2, base_color_texture="textures/church/glass_albedo.png", normal_texture="textures/church/glass_normal.png", metallic_roughness_texture="textures/church/glass_metallicRoughness.png", occlusion_texture="textures/church/glass_ao.png")
material "wood"  (color=[0.25, 0.15, 0.08], roughness=0.85, base_color_texture="textures/church/wood_albedo.png", normal_texture="textures/church/wood_normal.png", metallic_roughness_texture="textures/church/wood_metallicRoughness.png", occlusion_texture="textures/church/wood_ao.png")
material "gold"  (color=[0.8, 0.65, 0.1], metallic=0.9, roughness=0.3, base_color_texture="textures/church/gold_albedo.png", normal_texture="textures/church/gold_normal.png", metallic_roughness_texture="textures/church/gold_metallicRoughness.png", occlusion_texture="textures/church/gold_ao.png")

module "buttress_l" () {
  post "pillar" (size=[0.8, 4.5, 0.8], pos=[0, 0, 0], mat="stone")
  box "flyer"   (size=[2.5, 0.5, 0.5], pos=[1.25, 3.5, 0], rot=[0, 0, -25], mat="stone")
  pyramid "pin" (pos=[0, 4.5, 0], radius=0.565, height=1.5, sides=4, anchor=bottom, mat="stone")
}

module "buttress_r" () {
  post "pillar" (size=[0.8, 4.5, 0.8], pos=[0, 0, 0], mat="stone")
  box "flyer"   (size=[2.5, 0.5, 0.5], pos=[-1.25, 3.5, 0], rot=[0, 0, 25], mat="stone")
  pyramid "pin" (pos=[0, 4.5, 0], radius=0.565, height=1.5, sides=4, anchor=bottom, mat="stone")
}

module "window_side" () {
  box "w_base"  (size=[0.1, 2.0, 1.0], pos=[0, 0, 0], anchor=bottom, mat="glass")
  prism "w_top" (size=[1.0, 0.8, 0.1], pos=[0, 2.0, 0], rot=[0, 90, 0], anchor=bottom, mat="glass")
}

module "window_front" () {
  box "w_base"  (size=[0.8, 2.5, 0.1], pos=[0, 0, 0], anchor=bottom, mat="glass")
  prism "w_top" (size=[0.8, 0.8, 0.1], pos=[0, 2.5, 0], anchor=bottom, mat="glass")
}

module "cross" () {
  box "v" (size=[0.1, 1.0, 0.1], anchor=bottom, mat="gold")
  box "h" (size=[0.6, 0.1, 0.1], pos=[0, 0.6, 0], mat="gold")
}

scene {
  // Main stone structure merged for clean interior intersections
  solid "church_stone" (mat="stone", cleanup="coplanar") {
    // Facade and Towers
    box "facade_center" (pos=[0, 0, 0], size=[4, 6, 2], anchor=bottom)
    box "tower_l"       (pos=[-2.5, 0, 0], size=[2, 10, 2], anchor=bottom)
    box "tower_r"       (pos=[ 2.5, 0, 0], size=[2, 10, 2], anchor=bottom)
    
    // Cruciform Body
    box "nave"           (pos=[0, 0, -7], size=[4, 6, 12], anchor=bottom)
    box "transept"       (pos=[0, 0, -7], size=[10, 6, 4], anchor=bottom)
    box "crossing_tower" (pos=[0, 6, -7], size=[3, 3, 3], anchor=bottom)
    
    // Spires
    pyramid "spire_l"      (pos=[-2.5, 10, 0], radius=1.414, height=6, sides=4, anchor=bottom)
    pyramid "spire_r"      (pos=[ 2.5, 10, 0], radius=1.414, height=6, sides=4, anchor=bottom)
    pyramid "spire_center" (pos=[0, 9, -7], radius=2.121, height=6, sides=4, anchor=bottom)
    
    // Pointed Arches over doors
    prism "arch_c" (pos=[0, 2.4, 1.05], size=[1.6, 1.0, 0.15], anchor=bottom)
  }

  // Roofs
  prism "roof_nave"     (pos=[0, 6, -7], size=[4.2, 3, 12.2], anchor=bottom, mat="roof")
  prism "roof_transept" (pos=[0, 6, -7], size=[4.2, 3, 10.2], rot=[0, 90, 0], anchor=bottom, mat="roof")
  prism "roof_facade"   (pos=[0, 6, 0], size=[4.2, 2, 2.2], anchor=bottom, mat="roof")

  // Flying Buttresses along the nave
  grid "buttresses_left"  (count=[1, 1, 4], step=[0, 0, -2.5], pos=[-4.5, 0, -3.5]) { use "buttress_l" () }
  grid "buttresses_right" (count=[1, 1, 4], step=[0, 0, -2.5], pos=[ 4.5, 0, -3.5]) { use "buttress_r" () }

  // Stained Glass Windows
  cylinder "rose_window" (pos=[0, 4.5, 1.05], radius=1.2, height=0.1, rot=[90, 0, 0], mat="glass")
  
  group "wf_l" (pos=[-2.5, 6, 1.05]) { use "window_front" () }
  group "wf_r" (pos=[ 2.5, 6, 1.05]) { use "window_front" () }
  
  grid "windows_left"  (count=[1, 1, 4], step=[0, 0, -2.5], pos=[-2.05, 2.5, -3.5]) { use "window_side" () }
  grid "windows_right" (count=[1, 1, 4], step=[0, 0, -2.5], pos=[ 2.05, 2.5, -3.5]) { use "window_side" () }

  // Wooden Doors
  box "door_c" (pos=[0, 0, 1.05], size=[1.6, 2.4, 0.1], anchor=bottom, mat="wood")

  // Golden Crosses on Spires
  group "cross_l" (pos=[-2.5, 16, 0]) { use "cross" () }
  group "cross_r" (pos=[ 2.5, 16, 0]) { use "cross" () }
  group "cross_c" (pos=[0, 15, -7]) { use "cross" () }
}`;export{o as default};
