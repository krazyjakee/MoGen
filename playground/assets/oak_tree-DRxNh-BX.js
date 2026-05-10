const e=`
meta (
  seed = "1777166379305653000",
  thinking = "high",
  prompt = "oak tree",
)

material "oak_bark" (color=[0.35, 0.28, 0.20], roughness=1, base_color_texture="textures/oak_tree/oak_bark_albedo.png", normal_texture="textures/oak_tree/oak_bark_normal.png", metallic_roughness_texture="textures/oak_tree/oak_bark_metallicRoughness.png", occlusion_texture="textures/oak_tree/oak_bark_ao.png", occlusion_strength=0.63, double_sided=0, normal_strength=1.5, uv_scale=[3, 3])
material "oak_leaf" (
  color=[0.25, 0.45, 0.15], 
  roughness=1,
  alpha_mode="mask", 
  alpha_cutoff=0.5, 
  double_sided=1
, base_color_texture="textures/oak_tree/oak_leaf_albedo.png", normal_texture="textures/oak_tree/oak_leaf_normal.png", metallic_roughness_texture="textures/oak_tree/oak_leaf_metallicRoughness.png", metallic=0, normal_strength=0, uv_mode="tile", transmission=0, occlusion_strength=1, occlusion_texture="textures/oak_tree/oak_leaf_ao.png")

scene {
  branch "oak" (
    length=1.8, 
    radius=0.25, 
    depth=5, 
    splits=2,
    length_falloff=0.75, 
    radius_falloff=0.65,
    branch_angle=45, 
    bend=15, 
    tropism=-0.1, 
    jitter=0.3, 
    seed=42,
    leaves=1,
    leaf_size=1.65,
    leaf_cards=3,
    leaf_mat="oak_leaf",
    mat="oak_bark"
  )
}`;export{e as default};
