const e=`// Five distinct plant silhouettes from the same \`branch\` generator, each
// driven by a different \`form=\`. The form preset picks sensible defaults
// (length, branch_angle, leader_bias, leaf_aspect, etc.) — you still
// override anything you want with explicit attrs. Different \`seed=\` per
// plant so they don't all repeat the same RNG sequence.

material "bark"      (color=[0.36, 0.25, 0.15], roughness=0.95)
material "pine_bark" (color=[0.30, 0.20, 0.13], roughness=0.95)
material "broadleaf" (
    color=[0.20, 0.50, 0.22],
    roughness=0.65,
    alpha_mode="mask",
    alpha_cutoff=0.5,
    double_sided=1
)
material "needle" (
    color=[0.12, 0.36, 0.18],
    roughness=0.7,
    alpha_mode="mask",
    alpha_cutoff=0.5,
    double_sided=1
)
material "frond" (
    color=[0.22, 0.55, 0.20],
    roughness=0.6,
    alpha_mode="mask",
    alpha_cutoff=0.5,
    double_sided=1
)

scene {
  // The five plants stand apart on the ground — they're not meant to be
  // joined into one connected mesh. \`tags="floating"\` opts the whole row
  // out of the connectivity validator so each plant is treated as its
  // own island.
  group "garden" (tags="floating") {
    // Oak — default decurrent form, baseline broadleaf silhouette.
    branch "oak" (
      pos=[-6, 0, 0],
      form="decurrent",
      seed=7,
      leaf_mat="broadleaf",
      mat="bark"
    )

    // Pine — excurrent (central leader) with needle-shaped leaves.
    // Defaults give a tall conical silhouette; tweak \`depth\` / \`splits\`
    // for density.
    branch "pine" (
      pos=[-3, 0, 0],
      form="excurrent",
      seed=11,
      leaf_mat="needle",
      mat="pine_bark"
    )

    // Willow — weeping form. Strong negative tropism makes branches arc
    // downward; narrow leaves read as long willow foliage.
    branch "willow" (
      pos=[0, 0, 0],
      form="weeping",
      seed=23,
      leaf_mat="broadleaf",
      mat="bark"
    )

    // Bush — multiple short stems from the base, low overall height.
    branch "bush" (
      pos=[3, 0, 0],
      form="shrub",
      seed=41,
      leaf_mat="broadleaf",
      mat="bark"
    )

    // Palm — straight trunk + frond rosette at the top, no recursive
    // branching. \`leaf_cards\` controls the rosette count.
    branch "palm" (
      pos=[6, 0, 0],
      form="palm",
      seed=97,
      leaf_mat="frond",
      mat="bark"
    )
  }
}
`;export{e as default};
