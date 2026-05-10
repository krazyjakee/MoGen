const t=`
meta (
  seed = "1776943179019951452",
  prompt = "make the center cube yellow",
)

material "spike_mat" (color=[0.8, 0.8, 0.8], metallic=0.8, roughness=0.2)
material "yellow" (color=[1.0, 0.9, 0.1], roughness=0.5)

scene {
  rounded_box "b" (size=[1, 1, 1], radius=0.1, pos=[0, 0, -0.3763], mat="yellow")
  
  cone "spike_t" (radius=0.2, height=0.4, mat="spike_mat")
  cone "spike_b" (radius=0.2, height=0.4, mat="spike_mat")
  cone "spike_l" (radius=0.2, height=0.4, mat="spike_mat")
  cone "spike_r" (radius=0.2, height=0.4, mat="spike_mat")
  cone "spike_f" (radius=0.2, height=0.4, mat="spike_mat")
  cone "spike_bk" (radius=0.2, height=0.4, mat="spike_mat")

  attach (parent="b", child="spike_t", socket="top", plug="bottom")
  attach (parent="b", child="spike_b", socket="bottom", plug="bottom")
  attach (parent="b", child="spike_l", socket="left", plug="bottom")
  attach (parent="b", child="spike_r", socket="right", plug="bottom")
  attach (parent="b", child="spike_f", socket="front", plug="bottom")
  attach (parent="b", child="spike_bk", socket="back", plug="bottom")
}`;export{t as default};
