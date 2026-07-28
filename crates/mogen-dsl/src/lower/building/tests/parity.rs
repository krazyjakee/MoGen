//! A fingerprint of the whole generator, across 48 buildings.
//!
//! Written to measure the port onto `lower::arch` — the claim was that
//! perimeter wall corners stop being double-covered and *nothing else* moves,
//! and there was no way to check a claim like that about 48 buildings. It did
//! its job: it caught the perimeter delta landing on `4·wt²·h` per storey to
//! the fourth decimal, and it caught interior walls moving when a `slice` bug
//! slanted every cut.
//!
//! The plan said to delete it afterwards, along with the box builder. Keeping
//! it instead, because what it actually is — now that the port is done — is the
//! only test that would notice a POI transform drifting by a millimetre. The
//! goldens lock four buildings as opaque GLB bytes; this locks 48 at the level
//! of *which tier changed*, which is the difference between "something moved"
//! and "the furniture moved". It costs 20 KB and about a second.
//!
//! Regenerate the baseline with `MOGEN_GOLDENS_UPDATE=1`, the same variable the
//! GLB goldens use — but read the diff first. A change in `poi` or `slot` is
//! almost always a bug; a change in `tree` means geometry was renamed.
//!
//! # Why hashes and not a full dump
//!
//! A three-storey building is 782 nodes, 245 of them POI markers. Committing
//! every transform across 48 configurations is ~2 MB of generated text that
//! nobody will read. So the committed baseline stores one **hash per tier per
//! config** — enough to fail loudly and say which tier moved, small enough to
//! read in a diff.
//!
//! The detail still exists, it just is not committed. Every run writes the
//! full per-tier listing to `MOGEN_PARITY_DUMP` if that variable is set, so
//! the workflow when something breaks is:
//!
//! ```sh
//! git switch --detach <pre-port-commit>
//! MOGEN_PARITY_DUMP=/tmp/before cargo test -p mogen-dsl parity
//! git switch -
//! MOGEN_PARITY_DUMP=/tmp/after  cargo test -p mogen-dsl parity
//! diff -ru /tmp/before /tmp/after
//! ```
//!
//! # The five tiers
//!
//! | Tier | Tolerance | What a change means |
//! |---|---|---|
//! | `tree` | exact | a node appeared, vanished, moved, or was renamed |
//! | `poi` | 1e-6 | a gameplay anchor drifted — never acceptable |
//! | `slot` | 1e-6 | a furniture slot drifted — never acceptable |
//! | `aabb` | 1e-4 | some mesh changed shape or place |
//! | `role` volumes | 1e-4, listed in full | how much geometry each role owns |
//!
//! The role volumes are deliberately *not* hashed. They are the tier most
//! likely to move for a good reason, so they are written out as readable lines
//! and a diff says which roles gained or lost geometry and by how much. That is
//! what made the perimeter port checkable: `exterior_wall` fell by exactly
//! `4·wt²·h` per storey — the four corner columns the box builder covered
//! twice — and every other role held still.

use super::lower_src;
use glam::{Mat4, Vec3};
use mogen_core::{NodeId, SceneGraph};
use std::fmt::Write as _;

/// One building to measure.
struct Config {
    style: &'static str,
    roof: &'static str,
    storeys: u32,
    cellar: u32,
    seed: u32,
}

impl Config {
    fn label(&self) -> String {
        format!(
            "{}/{}/s{}/c{}/seed{}",
            self.style, self.roof, self.storeys, self.cellar, self.seed
        )
    }

    fn src(&self) -> String {
        let Config { style, roof, storeys, cellar, seed } = self;
        format!(
            r#"
material "concrete" (color=[0.8, 0.8, 0.8])
building "b" (
  seed={seed}, style="{style}", roof="{roof}",
  floors_above={storeys}, floors_below={cellar},
  floor_area=180, rooms=8, windows=5, entrances=1,
  staircases=1, elevators=1, mat="concrete",
) {{
  room_type "office" (kind=staff_only, density=1)
}}
"#
        )
    }
}

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

/// 42 style×roof pairs, with storeys / cellar / seed cycled across them.
///
/// A full cross would be 252 buildings and most of the extra coverage would be
/// redundant: the roof solver does not know how many storeys are under it, and
/// the layout does not know what is on top. Cycling the other three axes on
/// coprime strides visits every (storeys, cellar) pair against every style and
/// every roof without paying for the product.
fn configs() -> Vec<Config> {
    let mut out = Vec::new();
    for (si, style) in STYLES.iter().enumerate() {
        for (ri, roof) in ROOFS.iter().enumerate() {
            let n = si * ROOFS.len() + ri;
            out.push(Config {
                style,
                roof,
                storeys: if n % 2 == 0 { 1 } else { 3 },
                cellar: (n / 2 % 2) as u32,
                seed: [1u32, 7, 91][n % 3],
            });
        }
    }
    // Six deliberate extras: the tall-with-cellar case on every layout style
    // whose circulation lands differently, because stacked shafts are where
    // the port's opening assignment has the most to get wrong.
    for (i, style) in STYLES.iter().take(6).enumerate() {
        out.push(Config {
            style,
            roof: ROOFS[i % ROOFS.len()],
            storeys: 3,
            cellar: 1,
            seed: 2024,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// measurement
// ---------------------------------------------------------------------------

/// A node's world matrix, composed up the parent chain.
///
/// Local transforms are not enough: gable end-walls are built flat and rotated
/// −90° about X, so a wall that moved sideways can look like it moved
/// vertically if you read `translation` alone.
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

/// `parent/child/grandchild`, so an entry survives nodes being inserted
/// earlier in the arena. Index-keyed detail would report every line as changed
/// the moment one node appeared near the front.
fn path_of(g: &SceneGraph, id: NodeId) -> String {
    let mut parts = Vec::new();
    let mut cur = Some(id);
    while let Some(i) = cur {
        let n = &g.nodes[i.0 as usize];
        parts.push(n.name.as_str());
        cur = n.parent;
    }
    parts.reverse();
    parts.join("/")
}

fn depth_of(g: &SceneGraph, id: NodeId) -> usize {
    let mut d = 0;
    let mut cur = g.nodes[id.0 as usize].parent;
    while let Some(i) = cur {
        d += 1;
        cur = g.nodes[i.0 as usize].parent;
    }
    d
}

/// Fixed-point formatting with no `-0`, so a sign flip on a zero component
/// cannot masquerade as a real difference.
fn num(v: f32, dp: usize) -> String {
    let s = format!("{v:.dp$}");
    if s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == '.') {
        s[1..].to_string()
    } else {
        s
    }
}

fn mat(m: &Mat4, dp: usize) -> String {
    m.to_cols_array()
        .iter()
        .map(|v| num(*v, dp))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Signed volume via the divergence theorem, in world space.
fn world_volume(g: &SceneGraph, id: NodeId) -> f32 {
    let n = &g.nodes[id.0 as usize];
    let Some(mesh) = &n.mesh else { return 0.0 };
    let w = world_of(g, id);
    mesh.indices
        .chunks_exact(3)
        .map(|t| {
            let p = |k: usize| w.transform_point3(Vec3::from(mesh.positions[t[k] as usize]));
            let (a, b, c) = (p(0), p(1), p(2));
            a.dot(b.cross(c)) / 6.0
        })
        .sum::<f32>()
        .abs()
}

fn world_aabb(g: &SceneGraph, id: NodeId) -> Option<(Vec3, Vec3)> {
    let n = &g.nodes[id.0 as usize];
    let mesh = n.mesh.as_ref()?;
    let w = world_of(g, id);
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for p in &mesh.positions {
        let v = w.transform_point3(Vec3::from(*p));
        lo = lo.min(v);
        hi = hi.max(v);
    }
    (lo.x.is_finite()).then_some((lo, hi))
}

/// The detail lines behind one tier, in arena order.
struct Tiers {
    tree: Vec<String>,
    poi: Vec<String>,
    slot: Vec<String>,
    aabb: Vec<String>,
    roles: Vec<String>,
}

fn measure(g: &SceneGraph) -> Tiers {
    let ids: Vec<NodeId> = (0..g.nodes.len() as u32).map(NodeId).collect();

    let tree = ids
        .iter()
        .map(|&id| {
            let n = &g.nodes[id.0 as usize];
            format!(
                "{} {} kind={} role={} mesh={}",
                depth_of(g, id),
                n.name,
                n.kind,
                n.role.as_deref().unwrap_or("-"),
                n.mesh.is_some() as u8,
            )
        })
        .collect();

    let poi = ids
        .iter()
        .filter(|&&id| g.nodes[id.0 as usize].kind == "poi")
        .map(|&id| {
            let n = &g.nodes[id.0 as usize];
            format!(
                "{} role={} tags={} {}",
                path_of(g, id),
                n.role.as_deref().unwrap_or("-"),
                n.tags.join(","),
                mat(&world_of(g, id), 6),
            )
        })
        .collect();

    let slot = ids
        .iter()
        .filter_map(|&id| {
            let n = &g.nodes[id.0 as usize];
            let s = n.slot.as_ref()?;
            Some(format!(
                "{} kind={} w={} h={} d={} {}",
                path_of(g, id),
                s.kind,
                num(s.width, 6),
                num(s.height, 6),
                num(s.depth, 6),
                mat(&world_of(g, id), 6),
            ))
        })
        .collect();

    let aabb = ids
        .iter()
        .filter_map(|&id| {
            let (lo, hi) = world_aabb(g, id)?;
            Some(format!(
                "{} {} {} {} {} {} {}",
                path_of(g, id),
                num(lo.x, 4),
                num(lo.y, 4),
                num(lo.z, 4),
                num(hi.x, 4),
                num(hi.y, 4),
                num(hi.z, 4),
            ))
        })
        .collect();

    // Grouped by sorting a Vec rather than by HashMap, because the baseline is
    // a text file and map iteration order would rewrite it at random.
    let mut per_role: Vec<(String, f32)> = ids
        .iter()
        .filter(|&&id| g.nodes[id.0 as usize].mesh.is_some())
        .map(|&id| {
            let n = &g.nodes[id.0 as usize];
            (
                n.role.clone().unwrap_or_else(|| "-".into()),
                world_volume(g, id),
            )
        })
        .collect();
    per_role.sort_by(|a, b| a.0.cmp(&b.0));
    let mut roles = Vec::new();
    let mut i = 0;
    while i < per_role.len() {
        let mut j = i;
        let mut vol = 0.0;
        while j < per_role.len() && per_role[j].0 == per_role[i].0 {
            vol += per_role[j].1;
            j += 1;
        }
        roles.push(format!("{} n={} vol={}", per_role[i].0, j - i, num(vol, 4)));
        i = j;
    }

    Tiers { tree, poi, slot, aabb, roles }
}

/// FNV-1a, so the baseline is stable across platforms and toolchains without
/// pulling a hashing crate into a test that is scheduled for deletion.
fn digest(lines: &[String]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for line in lines {
        for b in line.as_bytes().iter().chain(b"\n") {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// The committed baseline: one block per config, hashes plus role volumes.
fn render_baseline() -> String {
    let mut out = String::new();
    out.push_str(
        "# Generated by `MOGEN_GOLDENS_UPDATE=1 cargo test -p mogen-dsl parity`.\n\
         # See parity.rs for what each tier means and how to recover the detail.\n",
    );
    for cfg in configs() {
        let g = lower_src(&cfg.src());
        let t = measure(&g);
        let _ = write!(out, "\n## {}\n", cfg.label());
        let _ = writeln!(out, "tree {} n={}", digest(&t.tree), t.tree.len());
        let _ = writeln!(out, "poi  {} n={}", digest(&t.poi), t.poi.len());
        let _ = writeln!(out, "slot {} n={}", digest(&t.slot), t.slot.len());
        let _ = writeln!(out, "aabb {} n={}", digest(&t.aabb), t.aabb.len());
        for line in &t.roles {
            let _ = writeln!(out, "role {line}");
        }
        if let Some(dir) = std::env::var_os("MOGEN_PARITY_DUMP") {
            dump_detail(std::path::Path::new(&dir), &cfg, &t);
        }
    }
    out
}

fn dump_detail(dir: &std::path::Path, cfg: &Config, t: &Tiers) {
    std::fs::create_dir_all(dir).expect("create dump dir");
    let name = cfg.label().replace('/', "_");
    let mut body = String::new();
    for (tier, lines) in [
        ("tree", &t.tree),
        ("poi", &t.poi),
        ("slot", &t.slot),
        ("aabb", &t.aabb),
        ("role", &t.roles),
    ] {
        for line in lines {
            let _ = writeln!(body, "{tier} {line}");
        }
    }
    std::fs::write(dir.join(format!("{name}.txt")), body).expect("write dump");
}

fn baseline_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/lower/building/tests/parity_baseline.txt")
}

#[test]
fn the_generator_matches_its_pre_port_baseline() {
    let actual = render_baseline();
    let path = baseline_path();

    if std::env::var_os("MOGEN_GOLDENS_UPDATE").is_some() {
        std::fs::write(&path, &actual).expect("write baseline");
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "no parity baseline at {}: {e}\n\
             create it with MOGEN_GOLDENS_UPDATE=1 cargo test -p mogen-dsl parity",
            path.display()
        )
    });

    if expected == actual {
        return;
    }

    // Report the first handful of differing lines rather than the whole file:
    // 48 blocks of hashes is unreadable as a panic message, and the tier name
    // plus config is already enough to say what to go and dump.
    let diffs: Vec<String> = expected
        .lines()
        .zip(actual.lines())
        .filter(|(a, b)| a != b)
        .take(12)
        .map(|(a, b)| format!("  - {a}\n  + {b}"))
        .collect();
    let mut msg = format!(
        "building output drifted from the parity baseline ({} of {} lines differ)",
        expected
            .lines()
            .zip(actual.lines())
            .filter(|(a, b)| a != b)
            .count(),
        expected.lines().count(),
    );
    if expected.lines().count() != actual.lines().count() {
        let _ = write!(
            msg,
            "\n(line counts differ: {} vs {} — a config produced a different set of roles)",
            expected.lines().count(),
            actual.lines().count(),
        );
    }
    let _ = write!(msg, "\n{}", diffs.join("\n"));
    let _ = write!(
        msg,
        "\n\nRecover the detail with:\n  \
         MOGEN_PARITY_DUMP=/tmp/after cargo test -p mogen-dsl parity\n  \
         and the same on the pre-port commit into /tmp/before, then diff -ru."
    );
    panic!("{msg}");
}

#[test]
fn the_baseline_covers_every_style_and_roof() {
    // A harness that silently stopped visiting half the matrix would pass
    // forever. Cheap to assert, and it fails at the moment someone trims the
    // config list rather than at the moment a regression slips through it.
    let cfgs = configs();
    assert_eq!(cfgs.len(), 48, "config count changed");
    for style in STYLES {
        assert!(cfgs.iter().any(|c| c.style == style), "{style} unvisited");
    }
    for roof in ROOFS {
        assert!(cfgs.iter().any(|c| c.roof == roof), "{roof} unvisited");
    }
    for (storeys, cellar) in [(1, 0), (1, 1), (3, 0), (3, 1)] {
        assert!(
            cfgs.iter().any(|c| c.storeys == storeys && c.cellar == cellar),
            "{storeys}-storey cellar={cellar} unvisited",
        );
    }
    let labels: std::collections::BTreeSet<String> =
        cfgs.iter().map(|c| c.label()).collect();
    assert_eq!(labels.len(), cfgs.len(), "two configs share a label");
}
