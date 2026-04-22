// A rectangular dining table: top slab + four cylindrical legs, each attached
// to a corner mount on the top's underside. Uses `attach` instead of an
// `array` rotation because the top is rectangular — four symmetric positions
// around Y would push two legs off the ends.

material "wood" (color=[0.55, 0.35, 0.18], metallic=0.0, roughness=0.75)

scene {
  group "table" (pos=[0, 0.94, 0], role="furniture", tags="table") {
    box "top" (size=[1.6, 0.08, 0.9], mat="wood", role="top") {
      connector "mount_fl" (at=[-0.75, -0.04, -0.40], dir=[0, -1, 0])
      connector "mount_fr" (at=[ 0.75, -0.04, -0.40], dir=[0, -1, 0])
      connector "mount_bl" (at=[-0.75, -0.04,  0.40], dir=[0, -1, 0])
      connector "mount_br" (at=[ 0.75, -0.04,  0.40], dir=[0, -1, 0])
    }

    cylinder "leg_fl" (radius=0.04, height=0.9, mat="wood", role="leg")
    cylinder "leg_fr" (radius=0.04, height=0.9, mat="wood", role="leg")
    cylinder "leg_bl" (radius=0.04, height=0.9, mat="wood", role="leg")
    cylinder "leg_br" (radius=0.04, height=0.9, mat="wood", role="leg")

    attach (parent="top", child="leg_fl", socket="mount_fl", plug="top")
    attach (parent="top", child="leg_fr", socket="mount_fr", plug="top")
    attach (parent="top", child="leg_bl", socket="mount_bl", plug="top")
    attach (parent="top", child="leg_br", socket="mount_br", plug="top")
  }
}
