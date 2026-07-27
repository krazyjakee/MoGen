//! Every style against every roof, on more than one seed.
//!
//! Written **before** retargeting the generator onto the shared architectural
//! IR, and deliberately not after. The existing tests are thorough about the
//! configurations someone thought to write down; this is about the ones nobody
//! did. Seven styles times six roofs is 42 combinations, of which the suite
//! exercised a handful — so a refactor could break a third of the surface and
//! stay green.
//!
//! Expect this to find *pre-existing* bugs. That is the point of running it
//! first: anything it catches now is a bug that already existed, and anything
//! it catches later is one the refactor introduced. Landing them in that order
//! is the difference between a fix and a mystery.
//!
//! The assertions are deliberately shallow — lowers, produces geometry, encloses
//! volume, is reproducible. A matrix this wide cannot assert *shapes* without
//! becoming a second implementation of the generator. Depth lives in the
//! per-feature tests; this is about breadth.

use super::lower_src;
use glam::{Mat4, Vec3};
use mogen_core::{Mesh, NodeId, SceneGraph};

const STYLES: [&str; 7] = [
    "grid",
    "apartment-block",
    "hotel-corridor",
    "office-core",
    "radial",
    "organic",
    "maze",
];

const ROOFS: [&str; 6] = ["flat", "gabled", "pitched", "hipped", "mansard", "shed"];

/// Three seeds, because a generator that only works on the seed someone
/// happened to write down is not deterministic, it is lucky.
const SEEDS: [u32; 3] = [1, 7, 91];

fn src(style: &str, roof: &str, seed: u32, floors: u32) -> String {
    format!(
        r#"
material "concrete" (color=[0.8, 0.8, 0.8])
building "b" (
  seed={seed}, style="{style}", roof="{roof}", floors_above={floors},
  floor_area=180, rooms=8, windows=4, entrances=1, mat="concrete",
) {{
  room_type "office" (kind=staff_only, density=1)
}}
"#
    )
}

/// Total enclosed volume, via the divergence theorem. Zero means the mesh
/// bounds nothing — a plane, or a shell whose caps went missing.
fn volume(mesh: &Mesh) -> f32 {
    let p = &mesh.positions;
    mesh.indices
        .chunks_exact(3)
        .map(|t| {
            let (a, b, c) = (p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]);
            (a[0] * (b[1] * c[2] - c[1] * b[2]) - a[1] * (b[0] * c[2] - c[0] * b[2])
                + a[2] * (b[0] * c[1] - c[0] * b[1]))
                / 6.0
        })
        .sum::<f32>()
        .abs()
}

fn meshes(g: &SceneGraph) -> impl Iterator<Item = (&str, &Mesh)> {
    g.nodes
        .iter()
        .filter_map(|n| n.mesh.as_ref().map(|m| (n.name.as_str(), m)))
}

/// A node's world matrix, composed up the parent chain.
///
/// Needed rather than just summing `translation.y`: gable end-walls are built
/// flat and rotated −90° about X, so their local *z* becomes world *y*. Reading
/// local coordinates makes a three-storey building look 1.3 m tall, which is
/// how this helper came to exist.
fn world_of(g: &SceneGraph, id: NodeId) -> Mat4 {
    let mut m = Mat4::IDENTITY;
    let mut cur = Some(id);
    while let Some(i) = cur {
        let n = &g.nodes[i.0 as usize];
        m = Mat4::from_scale_rotation_translation(
            n.transform.scale,
            n.transform.rotation,
            n.transform.translation,
        ) * m;
        cur = n.parent;
    }
    m
}

/// The highest point any geometry reaches, in world space.
fn world_top(g: &SceneGraph) -> f32 {
    g.nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| n.mesh.as_ref().map(|m| (NodeId(i as u32), m)))
        .flat_map(|(id, mesh)| {
            let w = world_of(g, id);
            mesh.positions
                .iter()
                .map(move |p| w.transform_point3(Vec3::from(*p)).y)
        })
        .fold(f32::MIN, f32::max)
}

#[test]
fn every_style_roof_combo_lowers_and_is_closed() {
    let mut checked = 0;
    for style in STYLES {
        for roof in ROOFS {
            for seed in SEEDS {
                let g = lower_src(&src(style, roof, seed, 1));
                let label = format!("{style}/{roof}/seed{seed}");

                let mut total = 0.0;
                let mut count = 0;
                for (name, mesh) in meshes(&g) {
                    assert!(
                        !mesh.positions.is_empty() && !mesh.indices.is_empty(),
                        "{label}: {name} is an empty mesh",
                    );
                    assert!(
                        mesh.indices.len() % 3 == 0,
                        "{label}: {name} has a partial triangle",
                    );
                    assert!(
                        mesh.positions.iter().flatten().all(|v| v.is_finite()),
                        "{label}: {name} has a non-finite vertex",
                    );
                    total += volume(mesh);
                    count += 1;
                }

                assert!(count > 0, "{label}: produced no geometry at all");
                assert!(total > 1.0, "{label}: encloses only {total} m³");
                checked += 1;
            }
        }
    }
    assert_eq!(checked, STYLES.len() * ROOFS.len() * SEEDS.len());
}

#[test]
fn every_style_roof_combo_is_reproducible() {
    // The generator's central contract: same seed and attrs, byte-identical
    // geometry. Checked across the whole matrix rather than one configuration,
    // because a stray hash iteration or an RNG draw added to the wrong phase
    // shows up in whichever combination happens to reach it.
    for style in STYLES {
        for roof in ROOFS {
            let source = src(style, roof, 42, 1);
            let (a, b) = (lower_src(&source), lower_src(&source));
            let label = format!("{style}/{roof}");

            let names_a: Vec<&str> = a.nodes.iter().map(|n| n.name.as_str()).collect();
            let names_b: Vec<&str> = b.nodes.iter().map(|n| n.name.as_str()).collect();
            assert_eq!(names_a, names_b, "{label}: node tree differs between runs");

            for ((n, ma), (_, mb)) in meshes(&a).zip(meshes(&b)) {
                assert_eq!(ma.positions, mb.positions, "{label}: {n} vertices differ");
                assert_eq!(ma.indices, mb.indices, "{label}: {n} indices differ");
            }
        }
    }
}

#[test]
fn changing_the_seed_changes_the_building() {
    // The other half of determinism, and the one a broken RNG passes: output
    // that is stable *and* identical for every seed is not a generator.
    //
    // `radial` is exempt, and it is worth saying why rather than quietly
    // dropping it. Its layout is concentric rectangular rings whose count
    // falls out of the room count -- it takes the RNG state and never draws
    // from it. That is deliberate (a rotunda is a regular arrangement, not a
    // random one), so seed-invariant geometry is the correct answer there and
    // asserting otherwise would be asserting a bug into existence.
    for style in STYLES.iter().copied().filter(|s| *s != "radial") {
        let a = lower_src(&src(style, "gabled", 1, 1));
        let b = lower_src(&src(style, "gabled", 999, 1));
        let va: f32 = meshes(&a).map(|(_, m)| volume(m)).sum();
        let vb: f32 = meshes(&b).map(|(_, m)| volume(m)).sum();
        assert!(
            (va - vb).abs() > 1e-4 || a.nodes.len() != b.nodes.len(),
            "{style}: seeds 1 and 999 produced the same building",
        );
    }
}

#[test]
fn every_style_survives_extra_storeys() {
    // Multi-storey is where the floor divisions have to seal, and where a
    // roof has to find the top of the stack rather than the ground.
    for style in STYLES {
        for roof in ROOFS {
            let g = lower_src(&src(style, roof, 3, 3));
            let label = format!("{style}/{roof}/3-storey");
            let total: f32 = meshes(&g).map(|(_, m)| volume(m)).sum();
            assert!(total > 1.0, "{label}: encloses only {total} m³");

            // The building must actually be three storeys tall, not one
            // repeated in place. Measured in world space -- see `world_of`.
            let top = world_top(&g);
            assert!(top > 5.0, "{label}: tops out at {top}m");
        }
    }
}
