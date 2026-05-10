const e=`// I-beam structural member built with the \`extrude\` primitive. The 2D
// I-shape is authored as a closed CCW polygon in the local XZ plane; the
// \`extrude\` then pushes it 3 m along +Y to make a column. Same primitive
// drops in for gear teeth, custom mouldings, hex bolt heads — anything
// whose silhouette is a hand-authored polygon.

material "steel" (color=[0.6, 0.62, 0.65], metallic=0.85, roughness=0.35)

scene {
  extrude "i_beam" (
    points=[
      [-0.5, -0.05], [0.5, -0.05], [0.5, 0.05], [0.1, 0.05],
      [0.1, 0.45], [0.5, 0.45], [0.5, 0.55], [-0.5, 0.55],
      [-0.5, 0.45], [-0.1, 0.45], [-0.1, 0.05], [-0.5, 0.05]
    ],
    height=3.0,
    mat="steel"
  )
}
`;export{e as default};
