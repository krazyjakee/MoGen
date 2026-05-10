const e=`// \`sweep\` — push a small architectural moulding profile along a 3D
// path. The profile is a closed CCW shape in the path's local XY plane;
// the path is a Catmull–Rom curve through the supplied control points.
// The same primitive handles square pipes, gun rails, and picture-frame
// trim along arbitrary curves.

material "oak" (color=[0.55, 0.35, 0.18], roughness=0.7)

scene {
  sweep "moulding" (
    profile=[
      [0.0, 0.0], [0.04, 0.0], [0.04, 0.02],
      [0.02, 0.04], [0.0, 0.04]
    ],
    path=[[-1, 0, 0], [0, 0, -0.5], [1, 0, 0]],
    samples=24,
    mat="oak"
  )
}
`;export{e as default};
