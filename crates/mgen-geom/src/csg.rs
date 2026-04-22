//! BSP-tree boolean CSG over triangle meshes.
//!
//! Port of Evan Wallace's csg.js algorithm
//! (<https://evanw.github.io/csg.js/>). Polygons are stored with their
//! supporting plane; booleans are computed by clipping each operand against
//! the other's BSP tree and recombining.
//!
//! The roadmap's original plan was to wrap `csgrs`, but that crate is
//! currently uninstallable from crates.io (its `core2 = "0.4"` transitive
//! dependency is fully yanked). This in-crate port keeps the same public
//! surface — `union` / `difference` / `intersect` on `Mesh` — so we can swap
//! back later without touching callers.

use glam::Vec3;

use mgen_core::Mesh;

const EPS: f32 = 1e-5;

#[derive(Clone, Copy, Debug)]
struct Vertex {
    pos: Vec3,
    normal: Vec3,
}

impl Vertex {
    fn new(pos: Vec3, normal: Vec3) -> Self {
        Self { pos, normal }
    }

    /// Interpolate position & normal along an edge. `t` is 0..1 from self→other.
    fn interpolate(&self, other: &Self, t: f32) -> Self {
        Self {
            pos: self.pos.lerp(other.pos, t),
            normal: self.normal.lerp(other.normal, t),
        }
    }

    fn flip(&mut self) {
        self.normal = -self.normal;
    }
}

#[derive(Clone, Copy, Debug)]
struct Plane {
    normal: Vec3,
    w: f32,
}

impl Plane {
    /// Build a plane from three points in CCW order. Returns `None` if the
    /// triangle is degenerate.
    fn from_points(a: Vec3, b: Vec3, c: Vec3) -> Option<Self> {
        let n = (b - a).cross(c - a);
        let len = n.length();
        if len < EPS {
            return None;
        }
        let normal = n / len;
        Some(Self { normal, w: normal.dot(a) })
    }

    fn flip(&mut self) {
        self.normal = -self.normal;
        self.w = -self.w;
    }

    /// Split `poly` against this plane. Each fragment is placed into one of:
    /// `coplanar_front`, `coplanar_back`, `front`, `back`.
    fn split_polygon(
        &self,
        poly: &Polygon,
        coplanar_front: &mut Vec<Polygon>,
        coplanar_back: &mut Vec<Polygon>,
        front: &mut Vec<Polygon>,
        back: &mut Vec<Polygon>,
    ) {
        const COPLANAR: u8 = 0;
        const FRONT: u8 = 1;
        const BACK: u8 = 2;
        const SPANNING: u8 = 3;

        let mut polygon_type: u8 = 0;
        let mut types: Vec<u8> = Vec::with_capacity(poly.verts.len());
        for v in &poly.verts {
            let t = self.normal.dot(v.pos) - self.w;
            let c = if t < -EPS {
                BACK
            } else if t > EPS {
                FRONT
            } else {
                COPLANAR
            };
            polygon_type |= c;
            types.push(c);
        }

        match polygon_type {
            COPLANAR => {
                if self.normal.dot(poly.plane.normal) > 0.0 {
                    coplanar_front.push(poly.clone());
                } else {
                    coplanar_back.push(poly.clone());
                }
            }
            FRONT => front.push(poly.clone()),
            BACK => back.push(poly.clone()),
            _ => {
                // SPANNING: subdivide the polygon along the plane.
                let mut f: Vec<Vertex> = Vec::new();
                let mut b: Vec<Vertex> = Vec::new();
                let n = poly.verts.len();
                for i in 0..n {
                    let j = (i + 1) % n;
                    let ti = types[i];
                    let tj = types[j];
                    let vi = poly.verts[i];
                    let vj = poly.verts[j];
                    if ti != BACK {
                        f.push(vi);
                    }
                    if ti != FRONT {
                        // Push a distinct copy so later normal flips on one
                        // side don't bleed across through shared references.
                        b.push(if ti != BACK { vi } else { vi });
                    }
                    if (ti | tj) == SPANNING {
                        let t = (self.w - self.normal.dot(vi.pos))
                            / self.normal.dot(vj.pos - vi.pos);
                        let v = vi.interpolate(&vj, t);
                        f.push(v);
                        b.push(v);
                    }
                }
                if f.len() >= 3 {
                    if let Some(p) = Polygon::try_new(f) {
                        front.push(p);
                    }
                }
                if b.len() >= 3 {
                    if let Some(p) = Polygon::try_new(b) {
                        back.push(p);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Polygon {
    verts: Vec<Vertex>,
    plane: Plane,
}

impl Polygon {
    fn try_new(verts: Vec<Vertex>) -> Option<Self> {
        if verts.len() < 3 {
            return None;
        }
        let plane = Plane::from_points(verts[0].pos, verts[1].pos, verts[2].pos)?;
        Some(Self { verts, plane })
    }

    fn flip(&mut self) {
        self.verts.reverse();
        for v in &mut self.verts {
            v.flip();
        }
        self.plane.flip();
    }
}

/// BSP node. The plane is `None` only for the empty tree.
#[derive(Default)]
struct Node {
    plane: Option<Plane>,
    front: Option<Box<Node>>,
    back: Option<Box<Node>>,
    polys: Vec<Polygon>,
}

impl Node {
    fn from_polygons(polys: Vec<Polygon>) -> Self {
        let mut node = Node::default();
        if !polys.is_empty() {
            node.build(polys);
        }
        node
    }

    fn invert(&mut self) {
        for p in &mut self.polys {
            p.flip();
        }
        if let Some(pl) = self.plane.as_mut() {
            pl.flip();
        }
        if let Some(f) = self.front.as_mut() {
            f.invert();
        }
        if let Some(b) = self.back.as_mut() {
            b.invert();
        }
        std::mem::swap(&mut self.front, &mut self.back);
    }

    /// Return `polys` trimmed of fragments that lie inside this BSP.
    fn clip_polygons(&self, polys: Vec<Polygon>) -> Vec<Polygon> {
        let Some(plane) = self.plane else {
            return polys;
        };
        let mut cf = Vec::new();
        let mut cb = Vec::new();
        let mut front = Vec::new();
        let mut back = Vec::new();
        for p in &polys {
            plane.split_polygon(p, &mut cf, &mut cb, &mut front, &mut back);
        }
        // Coplanar fragments are placed on their natural side; clipPolygons
        // treats coplanar-with-same-orientation as "in front" and the opposite
        // as "in back".
        front.extend(cf);
        back.extend(cb);
        let front = match &self.front {
            Some(n) => n.clip_polygons(front),
            None => front,
        };
        let back = match &self.back {
            Some(n) => n.clip_polygons(back),
            None => Vec::new(),
        };
        let mut out = front;
        out.extend(back);
        out
    }

    /// Remove all of `self`'s polygons that are inside `other`.
    fn clip_to(&mut self, other: &Node) {
        self.polys = other.clip_polygons(std::mem::take(&mut self.polys));
        if let Some(f) = self.front.as_mut() {
            f.clip_to(other);
        }
        if let Some(b) = self.back.as_mut() {
            b.clip_to(other);
        }
    }

    fn all_polygons(&self) -> Vec<Polygon> {
        let mut out = self.polys.clone();
        if let Some(f) = &self.front {
            out.extend(f.all_polygons());
        }
        if let Some(b) = &self.back {
            out.extend(b.all_polygons());
        }
        out
    }

    /// Build a BSP by picking the first polygon's plane as the splitter and
    /// recursing on the rest.
    fn build(&mut self, polys: Vec<Polygon>) {
        if polys.is_empty() {
            return;
        }
        if self.plane.is_none() {
            self.plane = Some(polys[0].plane);
        }
        let plane = self.plane.unwrap();
        let mut cf = Vec::new();
        let mut cb = Vec::new();
        let mut front = Vec::new();
        let mut back = Vec::new();
        for p in &polys {
            plane.split_polygon(p, &mut cf, &mut cb, &mut front, &mut back);
        }
        // Coplanar polygons (either orientation) stay at this BSP node.
        self.polys.extend(cf);
        self.polys.extend(cb);
        if !front.is_empty() {
            self.front.get_or_insert_with(|| Box::new(Node::default())).build(front);
        }
        if !back.is_empty() {
            self.back.get_or_insert_with(|| Box::new(Node::default())).build(back);
        }
    }
}

fn mesh_to_polygons(mesh: &Mesh) -> Vec<Polygon> {
    let mut out = Vec::with_capacity(mesh.indices.len() / 3);
    for tri in mesh.indices.chunks_exact(3) {
        let [ia, ib, ic] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let pa = Vec3::from_array(mesh.positions[ia]);
        let pb = Vec3::from_array(mesh.positions[ib]);
        let pc = Vec3::from_array(mesh.positions[ic]);
        let va = Vertex::new(pa, Vec3::from_array(mesh.normals[ia]));
        let vb = Vertex::new(pb, Vec3::from_array(mesh.normals[ib]));
        let vc = Vertex::new(pc, Vec3::from_array(mesh.normals[ic]));
        if let Some(poly) = Polygon::try_new(vec![va, vb, vc]) {
            out.push(poly);
        }
    }
    out
}

fn polygons_to_mesh(polys: &[Polygon]) -> Mesh {
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut indices = Vec::new();
    for poly in polys {
        if poly.verts.len() < 3 {
            continue;
        }
        // Fan-triangulate. Polygons produced by BSP clipping are convex, so
        // the fan from vertex 0 is always valid.
        let base = positions.len() as u32;
        for v in &poly.verts {
            positions.push([v.pos.x, v.pos.y, v.pos.z]);
            normals.push([v.normal.x, v.normal.y, v.normal.z]);
        }
        for i in 1..(poly.verts.len() as u32 - 1) {
            indices.extend_from_slice(&[base, base + i, base + i + 1]);
        }
    }
    Mesh { positions, normals, indices, ..Default::default() }
}

fn boolean(a: &Mesh, b: &Mesh, op: Op) -> Mesh {
    let mut na = Node::from_polygons(mesh_to_polygons(a));
    let mut nb = Node::from_polygons(mesh_to_polygons(b));

    match op {
        Op::Union => {
            na.clip_to(&nb);
            nb.clip_to(&na);
            nb.invert();
            nb.clip_to(&na);
            nb.invert();
            let b_polys = nb.all_polygons();
            na.build(b_polys);
        }
        Op::Difference => {
            na.invert();
            na.clip_to(&nb);
            nb.clip_to(&na);
            nb.invert();
            nb.clip_to(&na);
            nb.invert();
            let b_polys = nb.all_polygons();
            na.build(b_polys);
            na.invert();
        }
        Op::Intersect => {
            na.invert();
            nb.clip_to(&na);
            nb.invert();
            na.clip_to(&nb);
            nb.clip_to(&na);
            let b_polys = nb.all_polygons();
            na.build(b_polys);
            na.invert();
        }
    }

    polygons_to_mesh(&na.all_polygons())
}

#[derive(Clone, Copy)]
enum Op {
    Union,
    Difference,
    Intersect,
}

pub fn union(a: &Mesh, b: &Mesh) -> Mesh {
    boolean(a, b, Op::Union)
}

pub fn difference(a: &Mesh, b: &Mesh) -> Mesh {
    boolean(a, b, Op::Difference)
}

pub fn intersect(a: &Mesh, b: &Mesh) -> Mesh {
    boolean(a, b, Op::Intersect)
}

/// Left-fold `a` through each subsequent mesh with `difference` — i.e. subtract
/// every element of `rest` from `a`.
pub fn difference_many(a: &Mesh, rest: &[Mesh]) -> Mesh {
    rest.iter().fold(a.clone(), |acc, m| difference(&acc, m))
}

pub fn union_many(meshes: &[Mesh]) -> Mesh {
    let mut it = meshes.iter();
    let Some(first) = it.next() else {
        return Mesh::default();
    };
    it.fold(first.clone(), |acc, m| union(&acc, m))
}

pub fn intersect_many(meshes: &[Mesh]) -> Mesh {
    let mut it = meshes.iter();
    let Some(first) = it.next() else {
        return Mesh::default();
    };
    it.fold(first.clone(), |acc, m| intersect(&acc, m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{box_mesh, cylinder_mesh};

    fn bounds(m: &Mesh) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for p in &m.positions {
            min = min.min(Vec3::from_array(*p));
            max = max.max(Vec3::from_array(*p));
        }
        (min, max)
    }

    #[test]
    fn union_of_disjoint_boxes_has_both() {
        let a = box_mesh([1.0, 1.0, 1.0]);
        let mut b = box_mesh([1.0, 1.0, 1.0]);
        for p in &mut b.positions {
            p[0] += 3.0;
        }
        let u = union(&a, &b);
        let (min, max) = bounds(&u);
        assert!(min.x <= -0.5 + 1e-4);
        assert!(max.x >= 3.5 - 1e-4);
        assert!(!u.indices.is_empty());
    }

    #[test]
    fn difference_cuts_hole_in_wall() {
        let wall = box_mesh([4.0, 3.0, 0.2]);
        // Cylinder pierces all the way through the wall's thickness.
        let hole = cylinder_mesh(0.4, 1.0, 16);
        let result = difference(&wall, &hole);
        assert!(!result.indices.is_empty());
        let (min, max) = bounds(&result);
        // Wall's x/y envelope is preserved; only material near the cylinder is removed.
        assert!(min.x <= -1.99);
        assert!(max.x >= 1.99);
        assert!(min.y <= -1.49);
        assert!(max.y >= 1.49);
    }

    #[test]
    fn box_minus_small_box() {
        let a = box_mesh([2.0, 2.0, 2.0]);
        let b = box_mesh([1.0, 1.0, 1.0]);
        let r = difference(&a, &b);
        let (min, max) = bounds(&r);
        assert!(min.x <= -0.99);
        assert!(max.x >= 0.99);
    }

    #[test]
    fn box_minus_contained_cylinder() {
        let a = box_mesh([2.0, 2.0, 2.0]);
        let b = cylinder_mesh(0.3, 1.0, 16);
        let r = difference(&a, &b);
        let (min, max) = bounds(&r);
        assert!(min.x <= -0.99);
        assert!(max.x >= 0.99);
    }

    #[test]
    fn intersect_of_disjoint_is_empty() {
        let a = box_mesh([1.0, 1.0, 1.0]);
        let mut b = box_mesh([1.0, 1.0, 1.0]);
        for p in &mut b.positions {
            p[0] += 3.0;
        }
        let i = intersect(&a, &b);
        assert!(i.indices.is_empty());
    }
}
