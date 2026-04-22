// Exercises hierarchy, rotation (degrees, XYZ Euler), and scale.
scene {
  group "pedestal" (pos=[0, 0, 0]) {
    box "base"   (pos=[0, 0.1, 0], size=[1.2, 0.2, 1.2])
    box "column" (pos=[0, 0.8, 0], size=[0.4, 1.2, 0.4])
    group "top_cap" (pos=[0, 1.46, 0], rot=[0, 45, 0]) {
      box "plate" (size=[1.0, 0.1, 1.0], scale=1.2)
      box "knob"  (pos=[0, 0.15, 0], size=[0.2, 0.2, 0.2], rot=[0, 0, 45])
    }
  }
}
