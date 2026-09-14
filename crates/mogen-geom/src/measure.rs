use glam::{DVec3, Vec3};

/// Closest points on two finite segments, including point segments.
pub fn closest_segment_points(p: Vec3, q: Vec3, r: Vec3, s: Vec3) -> (Vec3, Vec3) {
    let p = p.as_dvec3();
    let q = q.as_dvec3();
    let r = r.as_dvec3();
    let s = s.as_dvec3();
    let u = q - p;
    let v = s - r;
    let w = p - r;
    let a = u.dot(u);
    let b = u.dot(v);
    let c = v.dot(v);
    let d = u.dot(w);
    let e = v.dot(w);
    let (mut x, y);
    if a == 0.0 && c == 0.0 {
        return (p.as_vec3(), r.as_vec3());
    }
    if a == 0.0 {
        x = 0.0;
        y = (e / c).clamp(0.0, 1.0);
    } else if c == 0.0 {
        x = (-d / a).clamp(0.0, 1.0);
        y = 0.0;
    } else {
        let denom = a * c - b * b;
        x = if denom > f64::EPSILON * a * c {
            (b * e - c * d) / denom
        } else {
            0.0
        };
        x = x.clamp(0.0, 1.0);
        let t = (b * x + e) / c;
        if t < 0.0 {
            y = 0.0;
            x = (-d / a).clamp(0.0, 1.0);
        } else if t > 1.0 {
            y = 1.0;
            x = ((b - d) / a).clamp(0.0, 1.0);
        } else {
            y = t;
        }
    }
    ((p + u * x).as_vec3(), (r + v * y).as_vec3())
}

/// Closest point on a triangle, using f64 region tests. Degenerate triangles
/// fall back to their three edges.
pub fn closest_triangle_point(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    fn solve(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> Option<DVec3> {
        let ab = b - a;
        let ac = c - a;
        let ap = p - a;
        if ab.cross(ac).length_squared() == 0.0 {
            return None;
        }
        let d1 = ab.dot(ap);
        let d2 = ac.dot(ap);
        if d1 <= 0.0 && d2 <= 0.0 {
            return Some(a);
        }
        let bp = p - b;
        let d3 = ab.dot(bp);
        let d4 = ac.dot(bp);
        if d3 >= 0.0 && d4 <= d3 {
            return Some(b);
        }
        let vc = d1 * d4 - d3 * d2;
        if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
            return Some(a + ab * (d1 / (d1 - d3)));
        }
        let cp = p - c;
        let d5 = ab.dot(cp);
        let d6 = ac.dot(cp);
        if d6 >= 0.0 && d5 <= d6 {
            return Some(c);
        }
        let vb = d5 * d2 - d1 * d6;
        if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
            return Some(a + ac * (d2 / (d2 - d6)));
        }
        let va = d3 * d6 - d5 * d4;
        if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
            return Some(b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))));
        }
        let inv = 1.0 / (va + vb + vc);
        Some(a + ab * (vb * inv) + ac * (vc * inv))
    }
    solve(p.as_dvec3(), a.as_dvec3(), b.as_dvec3(), c.as_dvec3())
        .map(|p| p.as_vec3())
        .unwrap_or_else(|| {
            [(a, b), (b, c), (c, a)]
                .into_iter()
                .map(|(a, b)| closest_segment_points(p, p, a, b).1)
                .min_by(|a, b| a.distance_squared(p).total_cmp(&b.distance_squared(p)))
                .unwrap()
        })
}

/// Includes edge/face piercing: vertex/face and edge/edge distances alone miss
/// triangles intersecting through their interiors.
fn segment_triangle(p: Vec3, q: Vec3, tri: [Vec3; 3]) -> Option<Vec3> {
    let [a, b, c] = tri.map(|p| p.as_dvec3());
    let p = p.as_dvec3();
    let d = q.as_dvec3() - p;
    let e1 = b - a;
    let e2 = c - a;
    let h = d.cross(e2);
    let det = e1.dot(h);
    if det == 0.0 {
        return None;
    } // Coplanar contact is covered by closest edges/faces.
    let s = p - a;
    let u = s.dot(h) / det;
    let v = d.dot(s.cross(e1)) / det;
    let t = e2.dot(s.cross(e1)) / det;
    if u >= 0.0 && v >= 0.0 && u + v <= 1.0 && (0.0..=1.0).contains(&t) {
        Some((p + d * t).as_vec3())
    } else {
        None
    }
}
pub fn closest_triangle_points(a: [Vec3; 3], b: [Vec3; 3]) -> (Vec3, Vec3) {
    for i in 0..3 {
        if let Some(p) = segment_triangle(a[i], a[(i + 1) % 3], b) {
            return (p, p);
        }
        if let Some(p) = segment_triangle(b[i], b[(i + 1) % 3], a) {
            return (p, p);
        }
    }
    let mut best = (a[0], b[0]);
    let mut distance = (best.0.as_dvec3() - best.1.as_dvec3()).length_squared();
    let mut consider = |p: Vec3, q: Vec3| {
        let d = (p.as_dvec3() - q.as_dvec3()).length_squared();
        if d < distance {
            distance = d;
            best = (p, q);
        }
    };
    for i in 0..3 {
        consider(a[i], closest_triangle_point(a[i], b[0], b[1], b[2]));
        consider(closest_triangle_point(b[i], a[0], a[1], a[2]), b[i]);
        for j in 0..3 {
            let (p, q) = closest_segment_points(a[i], a[(i + 1) % 3], b[j], b[(j + 1) % 3]);
            consider(p, q);
        }
    }
    best
}

use anyhow::{bail, Result};
use mogen_core::{Aabb, NodeId, SceneGraph, SurfaceDistance};
use std::collections::BinaryHeap;

/// Both limits are hard caps. The triangle cap bounds tree construction; work
/// counts visited tree pairs plus triangle tests. No work runs on every edit.
#[derive(Debug, Clone, Copy)]
pub struct MeasureOptions {
    pub tolerance: f64,
    pub max_work: usize,
    pub max_triangles: usize,
}
impl Default for MeasureOptions {
    fn default() -> Self {
        Self {
            tolerance: 0.002,
            max_work: 100_000,
            max_triangles: 100_000,
        }
    }
}
fn world_triangles(scene: &SceneGraph, root: NodeId, max: usize) -> Result<Vec<[Vec3; 3]>> {
    let world = scene.world_transforms();
    let mut triangles = Vec::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        let node = scene.get(id);
        pending.extend(&node.children);
        if let Some(mesh) = &node.mesh {
            for face in mesh.indices.chunks_exact(3) {
                if triangles.len() == max {
                    bail!("Measurement triangle limit {max} exceeded on '{}'; measure a smaller named part",scene.get(root).name);
                }
                let mut tri = [Vec3::ZERO; 3];
                for (i, &index) in face.iter().enumerate() {
                    let p = mesh
                        .positions
                        .get(index as usize)
                        .ok_or_else(|| anyhow::anyhow!("Invalid triangle index"))?;
                    tri[i] = world[id.0 as usize].transform_point3(Vec3::from_array(*p));
                    if !tri[i].is_finite() {
                        bail!("Measurement requires finite world geometry");
                    }
                }
                triangles.push(tri);
            }
        }
    }
    if triangles.is_empty() {
        bail!("Part '{}' has no triangle surface", scene.get(root).name);
    }
    Ok(triangles)
}
#[derive(Debug)]
struct Branch {
    bounds: Aabb,
    start: usize,
    end: usize,
    children: Option<(usize, usize)>,
}
struct Tree {
    triangles: Vec<[Vec3; 3]>,
    nodes: Vec<Branch>,
}
impl Tree {
    fn new(triangles: Vec<[Vec3; 3]>) -> Self {
        let mut tree = Self {
            triangles,
            nodes: Vec::new(),
        };
        tree.build(0, tree.triangles.len());
        tree
    }
    fn build(&mut self, start: usize, end: usize) -> usize {
        let mut bounds = Aabb::empty();
        for tri in &self.triangles[start..end] {
            for p in tri {
                bounds.expand(*p);
            }
        }
        let index = self.nodes.len();
        self.nodes.push(Branch {
            bounds,
            start,
            end,
            children: None,
        });
        if end - start > 4 {
            let size = bounds.max - bounds.min;
            let axis = if size.x >= size.y && size.x >= size.z {
                0
            } else if size.y >= size.z {
                1
            } else {
                2
            };
            self.triangles[start..end].sort_unstable_by(|a, b| {
                let center = |t: &[Vec3; 3]| {
                    (t[0][axis] as f64 + t[1][axis] as f64 + t[2][axis] as f64) / 3.0
                };
                center(a).total_cmp(&center(b))
            });
            let mid = start + (end - start) / 2;
            let l = self.build(start, mid);
            let r = self.build(mid, end);
            self.nodes[index].children = Some((l, r));
        }
        index
    }
}
fn box_distance(a: Aabb, b: Aabb) -> f64 {
    let delta = (a.min.as_dvec3() - b.max.as_dvec3())
        .max(b.min.as_dvec3() - a.max.as_dvec3())
        .max(DVec3::ZERO);
    delta.length()
}
#[derive(Clone, Copy, PartialEq)]
struct Pair {
    lower: f64,
    a: usize,
    b: usize,
}
impl Eq for Pair {}
impl PartialOrd for Pair {
    fn partial_cmp(&self, b: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(b))
    }
}
impl Ord for Pair {
    fn cmp(&self, b: &Self) -> std::cmp::Ordering {
        b.lower
            .total_cmp(&self.lower)
            .then_with(|| b.a.cmp(&self.a))
            .then_with(|| b.b.cmp(&self.b))
    }
}
/// Minimum distance between current world-space triangle surfaces of two
/// disjoint named subtrees. `exact` means complete for these tessellated meshes,
/// subject to floating-point precision; it never means analytic-solid distance.
pub fn measure_surfaces(
    scene: &SceneGraph,
    first: NodeId,
    second: NodeId,
    options: MeasureOptions,
) -> Result<SurfaceDistance> {
    if first == second || scene.is_ancestor(first, second) || scene.is_ancestor(second, first) {
        bail!("Choose two distinct, non-overlapping subtrees");
    }
    if !options.tolerance.is_finite()
        || options.tolerance < 0.0
        || !(1..=1_000_000).contains(&options.max_work)
        || !(1..=100_000).contains(&options.max_triangles)
    {
        bail!("Invalid measurement tolerance or budget (work 1..1000000, triangles 1..100000)");
    }
    let a = Tree::new(world_triangles(scene, first, options.max_triangles)?);
    let b = Tree::new(world_triangles(scene, second, options.max_triangles)?);
    let mut best = closest_triangle_points(a.triangles[0], b.triangles[0]);
    let mut distance = (best.0.as_dvec3() - best.1.as_dvec3()).length();
    let mut work = 1;
    let mut tests = 1;
    let mut queue = BinaryHeap::new();
    queue.push(Pair {
        lower: box_distance(a.nodes[0].bounds, b.nodes[0].bounds),
        a: 0,
        b: 0,
    });
    'search: while let Some(pair) = queue.pop() {
        if pair.lower >= distance {
            continue;
        }
        if work >= options.max_work {
            queue.push(pair);
            break;
        }
        work += 1;
        let an = &a.nodes[pair.a];
        let bn = &b.nodes[pair.b];
        if an.children.is_none() && bn.children.is_none() {
            for at in &a.triangles[an.start..an.end] {
                for bt in &b.triangles[bn.start..bn.end] {
                    if work >= options.max_work {
                        queue.push(pair);
                        break 'search;
                    }
                    work += 1;
                    tests += 1;
                    let candidate = closest_triangle_points(*at, *bt);
                    let d = (candidate.0.as_dvec3() - candidate.1.as_dvec3()).length();
                    if d < distance {
                        distance = d;
                        best = candidate;
                    }
                    if distance == 0.0 {
                        queue.clear();
                        break 'search;
                    }
                }
            }
        } else {
            let split_a = an.children.is_some()
                && (bn.children.is_none() || an.end - an.start >= bn.end - bn.start);
            let children = if split_a {
                let (l, r) = an.children.unwrap();
                [(l, pair.b), (r, pair.b)]
            } else {
                let (l, r) = bn.children.unwrap();
                [(pair.a, l), (pair.a, r)]
            };
            for (ai, bi) in children {
                let lower = box_distance(a.nodes[ai].bounds, b.nodes[bi].bounds);
                if lower < distance {
                    queue.push(Pair {
                        lower,
                        a: ai,
                        b: bi,
                    });
                }
            }
        }
    }
    let lower = queue.peek().map_or(distance, |p| p.lower.min(distance));
    let exact = lower >= distance;
    let delta = best.1.as_dvec3() - best.0.as_dvec3();
    Ok(SurfaceDistance {
        distance,
        closest_first: best.0.to_array(),
        closest_second: best.1.to_array(),
        direction_first_to_second: if distance > 0.0 {
            Some((delta / distance).as_vec3().to_array())
        } else {
            None
        },
        lower_bound: lower,
        exact,
        tolerance: options.tolerance,
        status: if distance <= options.tolerance {
            "within_tolerance"
        } else if lower > options.tolerance {
            "separated"
        } else {
            "inconclusive_budget"
        }
        .into(),
        work,
        triangle_tests: tests,
    })
}
