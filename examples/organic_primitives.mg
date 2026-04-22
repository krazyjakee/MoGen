// Showcase of the four organic-shape primitives: superellipsoid (egg),
// curved_plane (leaf), lathe (vase), and spline_tube (banana).
//
// Each sample is laid out along +X so they can be inspected side by side.
scene {
  material "gold"  (color=[0.85, 0.70, 0.15], metallic=0.6, roughness=0.35)
  material "leaf"  (color=[0.20, 0.55, 0.18], roughness=0.7)
  material "clay"  (color=[0.70, 0.35, 0.25], roughness=0.85)
  material "peel"  (color=[0.95, 0.85, 0.25], roughness=0.6)

  // Egg / soft-box: apple-ish bias, slightly shorter than wide.
  superellipsoid "egg" (pos=[-1.5, 0.6, 0],
                        size=[0.8, 1.0, 0.8], ew=1.3, ns=0.9,
                        mat="gold", tags="floating")

  // Curved leaf blade — bent along its long axis (X), slight cup across Z.
  curved_plane "leaf" (pos=[-0.5, 0.4, 0],
                       size=[0.9, 0.35], bend_u=35, bend_v=15,
                       segments_u=20, segments_v=8,
                       mat="leaf", tags="floating")

  // Vase revolved from a profile authored bottom-to-top.
  lathe "vase" (pos=[0.5, 0, 0],
                profile=[[0.00, 0.00],
                         [0.35, 0.05],
                         [0.25, 0.20],
                         [0.20, 0.45],
                         [0.30, 0.70],
                         [0.45, 0.85],
                         [0.40, 0.95],
                         [0.30, 1.00]],
                segments=48, cap_ends=1,
                mat="clay", tags="floating")

  // Banana: curved tube with tapering radii at both ends.
  spline_tube "banana" (pos=[1.7, 0.5, 0],
                        points=[[-0.45,  0.00, 0],
                                [-0.20,  0.22, 0],
                                [ 0.10,  0.28, 0],
                                [ 0.35,  0.18, 0],
                                [ 0.50, -0.02, 0]],
                        radii=[0.03, 0.06, 0.07, 0.06, 0.03],
                        segments=16, samples=10, cap_ends=1,
                        mat="peel", tags="floating")
}
