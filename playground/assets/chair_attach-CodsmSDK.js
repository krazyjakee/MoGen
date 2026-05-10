const t=`// A four-legged chair assembled with \`attach\`.
//
// Custom \`connector\`s on the seat give each leg its own mount point in the
// corner (a single \`bottom\` socket would stack all four legs on one spot).
// The legs and back are built at origin and let \`attach\` position them.

meta (mogen_version = "0.1.4")

material "wood" (color=[0.55, 0.35, 0.18], roughness=0.75)

scene {
  box "seat" (size=[1.0, 0.1, 1.0], mat="wood") {
    connector "mount_fl" (at=[-0.45, -0.05, -0.45], dir=[0, -1, 0])
    connector "mount_fr" (at=[ 0.45, -0.05, -0.45], dir=[0, -1, 0])
    connector "mount_bl" (at=[-0.45, -0.05,  0.45], dir=[0, -1, 0])
    connector "mount_br" (at=[ 0.45, -0.05,  0.45], dir=[0, -1, 0])
  }
  box "back" (size=[1.0, 1.0, 0.1], mat="wood", rot=[-90, 0, 0], pos=[0, 0.55, -0.55])
  cylinder "leg_fl" (radius=0.05, height=0.5, mat="wood")
  cylinder "leg_fr" (radius=0.05, height=0.5, mat="wood")
  cylinder "leg_bl" (radius=0.05, height=0.5, mat="wood")
  cylinder "leg_br" (radius=0.05, height=0.5, mat="wood")

  attach (parent="seat", child="back",   socket="back",     plug="bottom")
  attach (parent="seat", child="leg_fl", socket="mount_fl", plug="top")
  attach (parent="seat", child="leg_fr", socket="mount_fr", plug="top")
  attach (parent="seat", child="leg_bl", socket="mount_bl", plug="top")
  attach (parent="seat", child="leg_br", socket="mount_br", plug="top")
}
`;export{t as default};
