//! Credential-free regression of the actual #138/#139 procedural body.
use std::collections::{BTreeMap, BTreeSet};
fn mesh(source: &str) -> mogen_core::Mesh {
    let scene = mogen_dsl::lower(&mogen_dsl::parse(source).unwrap()).unwrap();
    scene
        .nodes
        .iter()
        .find(|n| n.name == "body_shell")
        .unwrap()
        .mesh
        .clone()
        .unwrap()
}
#[test]
fn sports_body_shading_subdivision_and_glb_share_mesh_contract() {
    let source = include_str!("../../../benches/quality/targets/sports_body.mog");
    let original = mesh(source);
    let triangles = original.indices.len() / 3;
    let mut minimum = [f32::INFINITY; 3];
    let mut maximum = [f32::NEG_INFINITY; 3];
    for p in &original.positions {
        for axis in 0..3 {
            minimum[axis] = minimum[axis].min(p[axis]);
            maximum[axis] = maximum[axis].max(p[axis]);
        }
    }
    // Regression diagnosis: render-vertex boundary valence >2, while exact
    // geometric adjacency is closed. The old unnormalised boundary mask
    // used .75*self + .125*sum(neighbors) and exceeded unit total weight.
    let mut edges = BTreeMap::<_, usize>::new();
    for t in original.indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let mut boundary = BTreeMap::<_, BTreeSet<_>>::new();
    for ((a, b), count) in edges {
        if count == 1 {
            boundary.entry(a).or_default().insert(b);
            boundary.entry(b).or_default().insert(a);
        }
    }
    assert!(boundary.values().any(|neighbors| neighbors.len() > 2));
    for level in [0, 1, 2] {
        let start = std::time::Instant::now();
        let variant = source.replace(
            "mat=\"rosso\"",
            &format!("mat=\"rosso\", crease_angle=40, subdivide={level}"),
        );
        let result = mesh(&variant);
        assert_eq!(result.indices.len() / 3, triangles * 4usize.pow(level));
        // Every Loop stencil is a convex combination of its local support;
        // its convex hull, hence each coordinate interval, cannot expand.
        for p in &result.positions {
            for axis in 0..3 {
                assert!(
                    p[axis].is_finite()
                        && p[axis] >= minimum[axis] - 1e-5
                        && p[axis] <= maximum[axis] + 1e-5,
                    "level {level}: {p:?}"
                );
            }
        }
        for n in &result.normals {
            let length = n.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((length - 1.).abs() < 1e-4);
        }
        let scene = mogen_dsl::lower(&mogen_dsl::parse(&variant).unwrap()).unwrap();
        assert!(!mogen_core::has_mesh_contract_errors(
            &mogen_core::validate_renderable_scene(&scene)
        ));
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("body.glb");
        mogen_export::write_glb(&scene, &path).unwrap();
        let exported = gltf::Gltf::open(path).unwrap();
        let blob = exported.blob.as_deref().unwrap();
        let mut count = 0;
        for primitive in exported.meshes().flat_map(|mesh| mesh.primitives()) {
            let reader = primitive.reader(|_| Some(blob));
            let positions: Vec<_> = reader.read_positions().unwrap().collect();
            for p in &positions {
                for axis in 0..3 {
                    assert!(p[axis] >= minimum[axis] - 1e-5 && p[axis] <= maximum[axis] + 1e-5);
                }
            }
            for n in reader.read_normals().unwrap() {
                assert!((n.iter().map(|x| x * x).sum::<f32>().sqrt() - 1.).abs() < 1e-4);
            }
            count += reader.read_indices().unwrap().into_u32().count() / 3;
        }
        assert_eq!(count, triangles * 4usize.pow(level));
        eprintln!(
            "sports body level {level}: {} vertices, {} triangles, {:?}",
            result.positions.len(),
            result.indices.len() / 3,
            start.elapsed()
        );
    }
    for control in ["faceted=1", "crease_angle=40", "crease_angle=180"] {
        let result = mesh(&source.replace("mat=\"rosso\"", &format!("mat=\"rosso\", {control}")));
        assert_eq!(original.indices.len(), result.indices.len());
        for (&a, &b) in original.indices.iter().zip(&result.indices) {
            assert_eq!(original.positions[a as usize], result.positions[b as usize]);
            assert_eq!(original.uvs[a as usize], result.uvs[b as usize]);
        }
    }
}
