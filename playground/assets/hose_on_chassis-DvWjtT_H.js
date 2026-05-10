const e=`// Rubber hose draped along a vehicle chassis.
//
// Demonstrates conforming a tube primitive (cylinder along Y) onto the
// curved upper surface of a chassis. The cross-section's circular ring is
// rotated by the surface frame at each path sample so the hose hugs the
// chassis the whole way.

material "metal"  (color=[0.55, 0.55, 0.58], roughness=0.4, metallic=0.8)
material "rubber" (color=[0.08, 0.08, 0.08], roughness=0.85)

scene {
  superellipsoid "chassis" (size=[1.6, 0.4, 0.7], ew=2.0, ns=2.0, mat="metal") {
    connector "port_a" (at=[-0.7,  0.20, 0.30], dir=[0, 1, 0])
    connector "port_b" (at=[ 0.7,  0.20, 0.30], dir=[0, 1, 0])
  }
  cylinder "hose" (radius=0.03, height=1.4, mat="rubber")
  conform (target="chassis", child="hose", from="port_a", to="port_b",
           along=y, samples=96, lift=0.005)
}
`;export{e as default};
