//! Cross-cutting guarantees for the arch module.
//!
//! Per-function behaviour is tested next to the code. What lives here are the
//! invariants that hold across the whole module and would otherwise be enforced
//! only by good intentions.

use super::consts::*;
use super::ir::*;
use super::{curve, plan, validate};

/// Drop `//`-style comments so the guards below scan code rather than prose.
///
/// Without this they trip over their own documentation: `mod.rs` explains that
/// nothing may reach for the RNG, and `ir.rs` explains why hash containers are
/// banned. Block comments aren't used anywhere in this module.
fn strip_line_comments(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every source file in `lower/arch/`, comments stripped, read at test time.
fn arch_sources() -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lower/arch");
    let mut out = Vec::new();
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read arch dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let src = std::fs::read_to_string(&path).expect("read source");
                out.push((name, strip_line_comments(&src)));
            }
        }
    }
    assert!(!out.is_empty(), "found no arch sources to scan");
    out
}

/// The solver must be a pure function of its input.
///
/// A single stray RNG draw would make output depend on call order, which breaks
/// the "same seed + same attrs ⇒ byte-identical geometry" contract for the
/// generator that will consume this later.
#[test]
fn arch_never_touches_the_rng() {
    for (name, src) in arch_sources() {
        if name == "tests.rs" {
            continue;
        }
        assert!(
            !src.contains("lower::rng") && !src.contains("use super::rng"),
            "{name} references the RNG; arch/ must be deterministic"
        );
    }
}

/// Hash iteration order is unspecified, so a `HashMap`/`HashSet` anywhere in a
/// path that produces geometry silently randomises output between runs.
#[test]
fn arch_never_uses_hash_containers() {
    for (name, src) in arch_sources() {
        if name == "tests.rs" {
            continue;
        }
        assert!(
            !src.contains("HashMap") && !src.contains("HashSet"),
            "{name} uses a hash container; sort-and-group or BTreeMap instead"
        );
    }
}

/// Tolerances must have exactly one definition. Two call sites disagreeing —
/// say, a wall tessellating at 24 segments while a filler assumes 32 — produces
/// geometry that very nearly matches, which is the worst kind.
#[test]
fn tolerances_are_not_redefined_outside_consts() {
    for (name, src) in arch_sources() {
        if name == "consts.rs" || name == "tests.rs" {
            continue;
        }
        for needle in ["const SNAP_MM", "const MITER_LIMIT", "const ARC_SEGMENTS"] {
            assert!(!src.contains(needle), "{name} redefines {needle}");
        }
    }
}

/// Building the same model twice must give identical results, including
/// floating-point bit patterns.
#[test]
fn sampling_is_reproducible() {
    let (a, b) = ([0.5, -1.25], [4.75, 2.5]);
    let first = curve::sample_centreline(a, b, Some(0.6));
    let second = curve::sample_centreline(a, b, Some(0.6));
    assert_eq!(first, second);
}

/// Endpoint snapping is what lets two walls at a junction agree on a corner.
/// If it were order-dependent, the corner would depend on which wall was
/// processed first.
#[test]
fn snapping_is_order_independent() {
    let raw = [[1.000_01, 2.000_02], [0.999_99, 1.999_98], [1.000_04, 2.000_03]];
    let snapped: Vec<_> = raw.iter().map(|p| plan::snap(*p)).collect();
    assert!(
        snapped.windows(2).all(|w| w[0] == w[1]),
        "points within the snap grid must collapse to one coordinate: {snapped:?}"
    );
}

/// A wall's plan footprint is the outer offset of its centreline, so its width
/// must equal the thickness regardless of orientation.
#[test]
fn offset_width_is_orientation_independent() {
    let thickness = 0.24_f32;
    for angle_deg in [0.0_f32, 17.0, 45.0, 90.0, 143.0, 270.0] {
        let a = angle_deg.to_radians();
        let (s, e) = ([0.0, 0.0], [4.0 * a.cos(), 4.0 * a.sin()]);
        let line = [s, e];
        let left = plan::offset_polyline(&line, thickness * 0.5);
        let right = plan::offset_polyline(&line, -thickness * 0.5);
        let gap = plan::distance(left[0], right[0]);
        assert!(
            (gap - thickness).abs() < 1e-4,
            "at {angle_deg}° the offsets are {gap} apart, want {thickness}"
        );
    }
}

/// A closed wall loop is the commonest real input. Every corner must be a
/// junction shared by exactly two walls, or mitring has nothing to work with.
#[test]
fn a_closed_rectangular_loop_validates() {
    let mut m = ArchModel::new(ModelSource::PascalEditor);
    m.levels.push(Level { id: LevelId(0), name: None, height: 2.5 });

    let corners = [[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]];
    for i in 0..4 {
        m.push_wall(Wall {
            id: WallId(0),
            level: LevelId(0),
            start: corners[i],
            end: corners[(i + 1) % 4],
            thickness: 0.15,
            height: None,
            curve_offset: None,
            openings: Vec::new(),
            material: None,
        });
    }

    assert_eq!(validate::check_model(&m), vec![]);

    // All four corners are shared by exactly two wall ends.
    let mut keys: Vec<_> = m
        .walls
        .iter()
        .flat_map(|w| [plan::key(w.start), plan::key(w.end)])
        .collect();
    keys.sort_unstable();
    let distinct = {
        let mut k = keys.clone();
        k.dedup();
        k
    };
    assert_eq!(distinct.len(), 4, "a rectangle has four junctions");
    for corner in &distinct {
        assert_eq!(keys.iter().filter(|k| *k == corner).count(), 2);
    }
}

/// The constants exist and are sane. Cheap, but it catches a bad merge that
/// zeroes a tolerance — which would otherwise show up as mysterious geometry.
#[test]
fn constants_are_sane() {
    assert!(SNAP_MM > 0.0);
    assert!(MITER_LIMIT > 1.0);
    assert!(ARC_SEGMENTS >= 8);
    assert!(CURVE_EPSILON > 0.0 && CURVE_EPSILON < 1e-3);
    assert!(COLLINEAR_EPS > 0.0);
    assert!(MIN_WALL_H > 0.0);
    assert!(MIN_PANEL > 0.0);
    assert!(CEILING_SHELL_THICKNESS > 0.0);
    assert!(CONNECTIVITY_SLOP > 0.0);
}
