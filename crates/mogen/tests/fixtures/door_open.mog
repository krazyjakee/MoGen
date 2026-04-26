// A swinging door driven by a named joint + authored clip.
// The door group pivots around +Y; the clip drives it from 0° to 90°
// over one second via the joint's axis and the track's `from`/`to` scalars.

material "wood" (color=[0.55, 0.35, 0.18], metallic=0.0, roughness=0.8)

scene {
  // Frame for context. Not animated.
  group "frame" {
    box "left"  (pos=[-0.55, 1.0, 0], size=[0.1, 2.0, 0.1], mat="wood")
    box "right" (pos=[ 0.55, 1.0, 0], size=[0.1, 2.0, 0.1], mat="wood")
    box "top"   (pos=[   0,  2.05, 0], size=[1.2, 0.1, 0.1], mat="wood")
  }

  // The door pivots around its left edge: shift its local origin to the hinge
  // by positioning the box child with half-width offset inside the group.
  group "door" (pos=[-0.5, 1.0, 0]) {
    box "panel" (pos=[0.5, 0, 0], size=[1.0, 1.9, 0.05], mat="wood")
  }
}

joint "door_hinge" (type=hinge, axis=[0, 1, 0], limits=[-10, 100], pivot="door")

clip "open" (seconds=1.0) {
  track "door_hinge" (from=0, to=90)
}
