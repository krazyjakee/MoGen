const e=`
meta (
  seed = "1777238846576117514",
  thinking = "high",
  prompt = "broken window",
)

material "wood_frame" (color=[0.85, 0.82, 0.78], roughness=0.9)
material "glass" (color=[0.88, 0.92, 0.95], transmission=0.95, roughness=0.02)

scene {
  // Outer window frame and inner mullions
  solid "frame" (mat="wood_frame", cleanup="coplanar") {
    box "frame_left"   (pos=[-0.6, 0, 0], size=[0.05, 1.6, 0.1])
    box "frame_right"  (pos=[ 0.6, 0, 0], size=[0.05, 1.6, 0.1])
    box "frame_top"    (pos=[ 0,  0.8, 0], size=[1.25, 0.05, 0.1])
    box "frame_bottom" (pos=[ 0, -0.8, 0], size=[1.25, 0.05, 0.1])
    box "mullion_v"    (pos=[ 0, 0, 0], size=[0.04, 1.6, 0.06])
    box "mullion_h"    (pos=[ 0, 0, 0], size=[1.2, 0.04, 0.06])
  }

  // Glass panes seated inside the frame quadrants
  group "panes" (mat="glass") {
    // Top-right: Intact
    box "pane_tr" (pos=[0.3, 0.4, 0], size=[0.55, 0.75, 0.01])
    
    // Bottom-left: Intact
    box "pane_bl" (pos=[-0.3, -0.4, 0], size=[0.55, 0.75, 0.01])

    // Top-left: Cracked but not shattered
    difference "pane_tl" (pos=[-0.3, 0.4, 0]) {
      box "base_tl" (size=[0.55, 0.75, 0.01])
      // Thin boxes acting as crack lines
      box "crack1" (pos=[-0.1, 0.2, 0], size=[0.4, 0.005, 0.1], rot=[0, 0, -25])
      box "crack2" (pos=[-0.1, 0.2, 0], size=[0.005, 0.3, 0.1], rot=[0, 0, 40])
      box "crack3" (pos=[-0.1, 0.2, 0], size=[0.25, 0.005, 0.1], rot=[0, 0, 15])
    }

    // Bottom-right: Shattered with a large hole
    difference "pane_br" (pos=[0.3, -0.4, 0]) {
      box "base_br" (size=[0.55, 0.75, 0.01])
      // Central impact hole (irregular, made of overlapping spheres)
      sphere "hole1" (pos=[0.05, -0.15, 0], radius=0.12)
      sphere "hole2" (pos=[0.13, -0.06, 0], radius=0.10)
      sphere "hole3" (pos=[-0.02, -0.23, 0], radius=0.11)
      sphere "hole4" (pos=[0.15, -0.22, 0], radius=0.09)
      sphere "hole5" (pos=[-0.03, -0.09, 0], radius=0.08)
      // Jagged break lines radiating from the hole
      box "break1" (pos=[0.05, -0.15, 0], size=[0.8, 0.02, 0.1], rot=[0, 0, 35])
      box "break2" (pos=[0.05, -0.15, 0], size=[0.02, 0.8, 0.1], rot=[0, 0, 12])
      box "break3" (pos=[0.05, -0.15, 0], size=[0.8, 0.015, 0.1], rot=[0, 0, -45])
      box "break4" (pos=[0.05, -0.15, 0], size=[0.02, 0.8, 0.1], rot=[0, 0, -75])
      box "break5" (pos=[0.05, -0.15, 0], size=[0.6, 0.03, 0.1], rot=[0, 0, -10])
    }
  }

}`;export{e as default};
