//! Mitred wall meshes for a producer that already knows where its nodes go.
//!
//! [`super::to_mog`] is the whole-model verb: hand it an [`super::ArchModel`]
//! and it decides what solids exist, what they are called, and where. That is
//! right for an importer, which has no opinion. It is wrong for the `building`
//! generator, which has 73 tests, a Studio inspector and a POI contract all
//! keyed to node names and transforms it chose itself.
//!
//! So this is the narrower verb. The producer says where each wall's
//! centreline runs and which frame it wants the mesh in; the solver mitres the
//! corners and returns one mesh per request, in that frame. Node identity stays
//! entirely with the producer — which is the only reason retargeting the
//! generator can be a change to *geometry* rather than a change to everything.
//!
//! # Why the frame is passed in rather than derived
//!
//! The obvious design returns world-space meshes and lets the caller apply an
//! inverse transform. That works and is wrong here: the round trip through
//! `Quat::from_rotation_y(FRAC_PI_2)` — whose cosine is −4.4e-8, not zero —
//! smears every vertex by a few ULPs, and the generator's whole promise is that
//! the same seed gives byte-identical geometry. Taking the local axes as exact
//! unit vectors keeps the projection to a dot product with ±1 and 0, which is
//! exact for the axis-aligned walls the generator builds.

use mogen_core::Mesh;

use super::ir::{LevelId, Wall, WallId, P2};
use super::miter;
use super::openings::solid_panels;
use super::plan;
use super::sink::mesh;

/// One wall to build.
///
/// `start`/`end` are world plan coordinates and are what the mitre solver
/// joins on — two requests meet at a corner exactly when they share an
/// endpoint. Everything else describes the frame the caller wants back.
#[derive(Clone, Debug)]
pub struct WallRequest {
    /// Centreline, world XZ. Thickness spreads ±`thickness/2` about it.
    pub start: P2,
    pub end: P2,
    pub thickness: f32,
    /// Full height. The mesh is centred vertically on its own origin, matching
    /// what `box_mesh` produces, so a caller's node transform does not move.
    pub height: f32,
    /// The node's local +X as a world XZ unit vector.
    pub axis_x: P2,
    /// The node's local +Z as a world XZ unit vector.
    pub axis_z: P2,
    /// The node's origin in world XZ.
    pub centre: P2,
    /// Openings as `[centre_x, centre_y, width, height]` in the wall's own
    /// centred elevation — identical to what the box builder took, so a caller
    /// switching over does not have to re-derive them.
    pub holes: Vec<[f32; 4]>,
}

/// Solve a set of walls together and return one mesh each, in each request's
/// own frame.
///
/// Together, not one at a time: a mitre is a property of a *junction*, so a
/// wall's shape depends on its neighbours. Passing walls in separately would
/// give every one of them a squared-off end.
///
/// A wall that cannot be solved comes back as an empty mesh rather than
/// failing the batch. The generator has no way to report a diagnostic from
/// here, and one missing wall is a better outcome than no building.
pub fn solve_wall_meshes(requests: &[WallRequest]) -> Vec<Mesh> {
    let walls: Vec<Wall> = requests
        .iter()
        .enumerate()
        .map(|(i, r)| Wall {
            id: WallId(i as u32),
            level: LevelId(0),
            start: r.start,
            end: r.end,
            thickness: r.thickness,
            height: Some(r.height),
            curve_offset: None,
            // Openings are already in the caller's elevation frame, so they go
            // straight to `solid_panels` below. Round-tripping them through the
            // IR's start-relative `along` would be two sign conversions for no
            // gain.
            openings: Vec::new(),
            material: None,
        })
        .collect();

    let solution = miter::solve_level(&walls, LevelId(0));

    // `solve_level` returns footprints for the walls it could solve, in wall
    // order but with gaps. Index them so a rejected wall does not shift every
    // wall after it onto the wrong geometry.
    let mut by_id: Vec<Option<&miter::WallFootprint>> = vec![None; requests.len()];
    for fp in &solution.footprints {
        by_id[fp.wall.0 as usize] = Some(fp);
    }

    let mut out: Vec<Mesh> = requests
        .iter()
        .enumerate()
        .map(|(i, r)| match by_id[i] {
            Some(fp) => build(r, fp),
            None => Mesh::default(),
        })
        .collect();

    // Butt-jointed corners leave a notch, and the notch is a hole in the
    // building. The solver hands back a patch for each one; without this they
    // would be silently dropped, which is the failure mode that matters most
    // here — an angle shallow enough to defeat the mitre is exactly the angle
    // nobody thinks to look at.
    //
    // A patch belongs to a junction rather than to a wall, so it is attached to
    // the lower-numbered of the two walls that made it. That is arbitrary but
    // it is *stable*, which is what the caller needs: the patch has to live on
    // some node, and it must be the same node every run.
    for filler in &solution.fillers {
        let mut hosts = [filler.walls[0].0 as usize, filler.walls[1].0 as usize];
        hosts.sort_unstable();
        let Some(&host) = hosts.iter().find(|&&i| !out[i].positions.is_empty()) else {
            continue;
        };
        let r = &requests[host];
        let ring: Vec<P2> = filler.ring.iter().map(|p| to_local(*p, r)).collect();
        if let Ok(m) = mesh::prism(&ring, &[], -0.5 * r.height, 0.5 * r.height) {
            let mut acc = std::mem::take(&mut out[host]);
            append(&mut acc, &m);
            out[host] = acc;
        }
    }

    out
}

/// A world plan point in a request's own frame.
fn to_local(p: P2, r: &WallRequest) -> P2 {
    let d = plan::sub(p, r.centre);
    [plan::dot(d, r.axis_x), plan::dot(d, r.axis_z)]
}

/// One wall that meets nothing.
///
/// Not a convenience wrapper — it is a statement, and calling
/// [`solve_wall_meshes`] with a one-element slice would say the same thing less
/// clearly. The generator has walls that genuinely stand alone: an elevator's
/// west face, a column filler. Those come in *stacks*, one piece per storey at
/// the same plan position, and the mitre solver works in plan with no notion of
/// height — hand it the stack and it sees four walls sharing both endpoints and
/// mitres them into each other. So they must be solved one at a time, and the
/// name is there to stop someone helpfully batching them later.
pub fn solve_lone_wall_mesh(request: &WallRequest) -> Mesh {
    solve_wall_meshes(std::slice::from_ref(request))
        .pop()
        .unwrap_or_default()
}

fn build(r: &WallRequest, fp: &miter::WallFootprint) -> Mesh {
    let length = plan::distance(r.start, r.end);
    if length <= 0.0 || r.height <= 0.0 || r.thickness <= 0.0 {
        return Mesh::default();
    }
    let Some(dir) = plan::normalise(plan::sub(r.end, r.start)) else {
        return Mesh::default();
    };
    // +1 when the caller's local +X runs start → end, −1 when it runs the
    // other way. Both happen: `wall_frame` points the south wall's local +X at
    // world −X so its face looks outward.
    let sense = plan::dot(r.axis_x, dir).signum();

    let panels = solid_panels(length, r.height, &r.holes);
    let mut acc = Mesh::default();
    for panel in &panels {
        // Panel X is centred on the wall; the footprint is parameterised 0→1
        // from start to end.
        let (mut t0, mut t1) = (
            0.5 + sense * panel.x0 / length,
            0.5 + sense * panel.x1 / length,
        );
        if t0 > t1 {
            std::mem::swap(&mut t0, &mut t1);
        }
        let ring: Vec<P2> = fp
            .slice(t0.clamp(0.0, 1.0), t1.clamp(0.0, 1.0))
            .into_iter()
            .map(|p| to_local(p, r))
            .collect();

        match mesh::prism(&ring, &[], panel.y0, panel.y1) {
            Ok(m) => append(&mut acc, &m),
            // A degenerate panel is a sliver the mitre pinched out of
            // existence. Skipping it leaves the wall solid; the alternative —
            // letting it through — is the capless-mesh failure this whole layer
            // exists to prevent.
            Err(_) => continue,
        }
    }
    acc
}

fn append(acc: &mut Mesh, src: &Mesh) {
    let base = acc.positions.len() as u32;
    acc.positions.extend_from_slice(&src.positions);
    acc.normals.extend_from_slice(&src.normals);
    acc.uvs.extend_from_slice(&src.uvs);
    acc.indices.extend(src.indices.iter().map(|i| base + i));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wall running along world +X, local +X the same way.
    fn along_x(x0: f32, x1: f32, z: f32, t: f32, h: f32) -> WallRequest {
        WallRequest {
            start: [x0, z],
            end: [x1, z],
            thickness: t,
            height: h,
            axis_x: [1.0, 0.0],
            axis_z: [0.0, 1.0],
            centre: [0.5 * (x0 + x1), z],
            holes: Vec::new(),
        }
    }

    /// A wall running along world +Z, with local +X pointing world −Z — the
    /// reversed sense the generator's south and west walls use.
    fn along_z_reversed(z0: f32, z1: f32, x: f32, t: f32, h: f32) -> WallRequest {
        WallRequest {
            start: [x, z0],
            end: [x, z1],
            thickness: t,
            height: h,
            axis_x: [0.0, -1.0],
            axis_z: [1.0, 0.0],
            centre: [x, 0.5 * (z0 + z1)],
            holes: Vec::new(),
        }
    }

    fn volume(m: &Mesh) -> f32 {
        m.indices
            .chunks_exact(3)
            .map(|t| {
                let p = |k: usize| glam::Vec3::from(m.positions[t[k] as usize]);
                p(0).dot(p(1).cross(p(2))) / 6.0
            })
            .sum()
    }

    fn bounds(m: &Mesh) -> ([f32; 3], [f32; 3]) {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for p in &m.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        (lo, hi)
    }

    #[test]
    fn a_lone_wall_is_exactly_its_centreline_box() {
        // Nothing to mitre against, so the ends square off on the endpoints and
        // the answer must be the plain box the old builder produced.
        let m = &solve_wall_meshes(&[along_x(0.0, 4.0, 0.0, 0.2, 2.5)])[0];
        let (lo, hi) = bounds(m);
        assert_eq!([lo[0], hi[0]], [-2.0, 2.0], "length");
        assert_eq!([lo[1], hi[1]], [-1.25, 1.25], "height, centred on origin");
        assert_eq!([lo[2], hi[2]], [-0.1, 0.1], "thickness");
        assert!((volume(m) - 4.0 * 2.5 * 0.2).abs() < 1e-5, "{}", volume(m));
    }

    #[test]
    fn the_local_frame_is_honoured_rather_than_assumed() {
        // Same wall, mesh asked for in a frame whose local +X is world −Z. The
        // mesh must still come out with length on local X.
        let m = &solve_wall_meshes(&[along_z_reversed(0.0, 4.0, 3.0, 0.2, 2.5)])[0];
        let (lo, hi) = bounds(m);
        assert!((lo[0] + 2.0).abs() < 1e-5 && (hi[0] - 2.0).abs() < 1e-5, "length on local X");
        // Approximate, not exact: the solver normalises the wall direction, so
        // the offset faces land a float epsilon off ±t/2 even for an
        // axis-aligned wall. Well under the 0.1 mm the DSL rounds to.
        assert!((lo[2] + 0.1).abs() < 1e-5 && (hi[2] - 0.1).abs() < 1e-5, "thickness");
    }

    #[test]
    fn a_corner_mitres_instead_of_overlapping() {
        // Two walls meeting at a right angle. Each reaches the far side of the
        // corner on its outside and stops short on its inside — which is the
        // point: the corner volume belongs to exactly one of them.
        let t = 0.2;
        let a = WallRequest { ..along_x(-5.0, 0.0, 0.0, t, 2.5) };
        let b = WallRequest {
            start: [0.0, 0.0],
            end: [0.0, 5.0],
            axis_x: [0.0, 1.0],
            axis_z: [-1.0, 0.0],
            centre: [0.0, 2.5],
            ..along_x(0.0, 0.0, 0.0, t, 2.5)
        };
        let meshes = solve_wall_meshes(&[a, b]);
        let (lo, hi) = bounds(&meshes[0]);
        // Wall A's local X spans −2.5..2.5 before mitring. The corner pushes
        // one side out by half a thickness and pulls the other in by the same,
        // so the extremes are ±(2.5 + t/2) and the near face stops at 2.5 − t/2.
        assert!((hi[0] - 2.6).abs() < 1e-4, "outer corner at {}", hi[0]);
        assert!((lo[0] + 2.5).abs() < 1e-4, "free end unchanged at {}", lo[0]);

        // The invariant worth stating: for equal thicknesses a mitre only
        // *redistributes* the corner, so the pair's volume is exactly the sum
        // of their centreline boxes. Unmitred walls score less than this — they
        // overlap in the corner and the overlap is counted once — and butted
        // ones less again, because they leave the corner square empty.
        let total: f32 = meshes.iter().map(volume).sum();
        let expected = 2.0 * (5.0 * t * 2.5);
        assert!((total - expected).abs() < 1e-3, "{total} vs {expected}");
    }

    #[test]
    fn a_hole_splits_the_wall_and_keeps_the_mitre() {
        // The pier beside a corner must still reach the mitred corner — that is
        // the reason the footprint keeps both sides rather than a ring.
        let t = 0.2;
        let a = WallRequest {
            holes: vec![[0.0, 0.0, 1.0, 1.2]],
            ..along_x(-5.0, 0.0, 0.0, t, 2.5)
        };
        let b = WallRequest {
            start: [0.0, 0.0],
            end: [0.0, 5.0],
            axis_x: [0.0, 1.0],
            axis_z: [-1.0, 0.0],
            centre: [0.0, 2.5],
            ..along_x(0.0, 0.0, 0.0, t, 2.5)
        };
        let m = &solve_wall_meshes(&[a, b])[0];
        let (_, hi) = bounds(m);
        assert!((hi[0] - 2.6).abs() < 1e-4, "corner lost when the wall split");
        // Same invariant per wall: the centreline box, less the opening.
        let expected = (5.0 * 2.5 - 1.0 * 1.2) * t;
        assert!((volume(m) - expected).abs() < 1e-4, "{}", volume(m));
    }

    #[test]
    fn a_shallow_corner_gets_its_notch_patched() {
        // Below the mitre limit the solver butts both walls and hands back a
        // patch instead. Dropping that patch leaves a notch open through the
        // full wall height, and a 6° corner is precisely the case nobody
        // inspects. So: the pair must still hold at least the volume of their
        // two centreline boxes minus their overlap, rather than visibly less.
        let t = 0.2;
        let h = 2.5;
        let a = WallRequest {
            start: [-5.0, 0.0],
            end: [0.0, 0.0],
            thickness: t,
            height: h,
            axis_x: [1.0, 0.0],
            axis_z: [0.0, 1.0],
            centre: [-2.5, 0.0],
            holes: Vec::new(),
        };
        // Six degrees off straight — well inside MITER_LIMIT's reach.
        let (dx, dz) = (5.0f32 * 0.9945, 5.0f32 * 0.1045);
        let b = WallRequest {
            start: [0.0, 0.0],
            end: [dx, dz],
            centre: [0.5 * dx, 0.5 * dz],
            axis_x: [0.9945, 0.1045],
            axis_z: [-0.1045, 0.9945],
            ..a.clone()
        };
        let meshes = solve_wall_meshes(&[a, b]);
        let total: f32 = meshes.iter().map(volume).sum();
        // Two 5 m centreline boxes, less the wedge their butt ends cannot
        // reach. Anything materially under this means the patch went missing.
        let boxes = 2.0 * 5.0 * t * h;
        assert!(
            total > boxes - 0.5 * t * t * h && total < boxes + 0.5 * t * t * h,
            "{total} vs ~{boxes}",
        );
        for m in &meshes {
            assert!(!m.positions.is_empty(), "a wall vanished");
        }
    }

    #[test]
    fn a_degenerate_request_yields_an_empty_mesh_not_a_panic() {
        let zero = WallRequest { thickness: 0.0, ..along_x(0.0, 4.0, 0.0, 0.0, 2.5) };
        assert!(solve_wall_meshes(&[zero])[0].positions.is_empty());
    }

    #[test]
    fn one_mesh_comes_back_per_request_in_order() {
        // A rejected wall must not shift its neighbours onto the wrong shape.
        let out = solve_wall_meshes(&[
            along_x(0.0, 4.0, 0.0, 0.2, 2.5),
            along_x(0.0, 0.0, 9.0, 0.2, 2.5), // zero length, rejected
            along_x(0.0, 6.0, 20.0, 0.2, 2.5),
        ]);
        assert_eq!(out.len(), 3);
        assert!(out[1].positions.is_empty(), "the rejected wall is the empty one");
        let (lo, hi) = bounds(&out[2]);
        assert_eq!([lo[0], hi[0]], [-3.0, 3.0], "third wall kept its own length");
    }
}

