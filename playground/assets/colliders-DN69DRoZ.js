const o=`// A tiny diorama showing how \`collider="aabb"\` decorates geometry, groups,
// and \`use\` invocations. Build with \`mogen build examples/colliders.mog -o
// colliders.glb\`; the colliders end up in \`node.extras.collider\` for any
// downstream importer (e.g. Godot) to consume.

material "wood"    (color=[0.45, 0.30, 0.18], roughness=0.8)
material "plaster" (color=[0.92, 0.90, 0.88], roughness=0.95)

module "table" () {
  rounded_box "top" (size=[1.2, 0.05, 0.7], radius=0.01, mat="wood")
  cylinder "leg_fl" (pos=[-0.5, -0.4, -0.3], radius=0.03, height=0.8, mat="wood")
  cylinder "leg_fr" (pos=[ 0.5, -0.4, -0.3], radius=0.03, height=0.8, mat="wood")
  cylinder "leg_bl" (pos=[-0.5, -0.4,  0.3], radius=0.03, height=0.8, mat="wood")
  cylinder "leg_br" (pos=[ 0.5, -0.4,  0.3], radius=0.03, height=0.8, mat="wood")
}

scene {
  // Floor + back wall as solid colliders. \`slab\` anchors to its bottom
  // face by default, so \`y=0.1\` puts the wall's base flush with the
  // floor's top (floor thickness = 0.1).
  slab "floor"     (size=[6, 0.1, 4], mat="wood",    collider="aabb")
  slab "wall_back" (size=[6, 2.5, 0.15], y=0.1, z=-1.925, mat="plaster", collider="aabb")

  // The whole table gets one AABB enclosing the top + legs.
  use "table" (pos=[0, 0.9, 0], collider="aabb")
}
`;export{o as default};
